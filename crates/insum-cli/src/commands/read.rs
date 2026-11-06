//! `insum read` — whole-note reading: JSON envelope or annotated projection
//! The projection is read-only text with pointer anchors; it is
//! never a storage format and has no parser.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};

use insum_core::model::{BlockEntry, INLINE_NODE_TYPES};
use insum_core::{ErrorCode, Limits, UmError};

use crate::cursor::Cursor;
use crate::support::{Output, Revs, SourceReader, render_path};

pub fn run(
    path: &Path,
    note: Option<&str>,
    format: &str,
    max_bytes: Option<usize>,
    cursor_token: Option<&str>,
    limits: &Limits,
) -> Result<Output, UmError> {
    match format {
        "json" | "annotated" => {}
        other => {
            return Err(UmError::usage(format!(
                "--format must be `json` or `annotated`, got {other:?}"
            )));
        }
    }
    if format == "json" && cursor_token.is_some() {
        // --cursor was silently ignored on json output — reject instead
        return Err(
            UmError::usage("--cursor applies to --format annotated only in this release")
                .with_hint(
                    "json reads the whole note; use `insum tree` for block-wise JSON pagination.",
                ),
        );
    }

    let rendered = render_path(path);
    let cursor = match cursor_token {
        Some(token) => Some(Cursor::decode(token)?),
        None => None,
    };
    // No cursor-based narrowing — a multi-note container is read
    // across notes; the lazy reader keeps the per-page cost bounded.
    let filters = note.map(|n| vec![n.to_owned()]);
    let mut reader = SourceReader::open(path, filters.as_deref(), limits, Revs::Compute)?;
    let start_note = cursor.as_ref().map_or(0, |c| c.note_index);
    let budget = limits.output_bytes(max_bytes);

    if cursor.is_some() && start_note >= reader.len() {
        return Err(stale_cursor(
            "cursor points past the last note of this response",
        ));
    }

    if format == "json" {
        // The budget is per response — no silent oversize; the command
        // names the required size instead of exceeding it
        let mut notes = Vec::new();
        let mut used: usize = 256;
        for pos in 0..reader.len() {
            let view = reader.read_at(pos)?;
            let note_value = json!({
                "note_id": view.note_id,
                "note_title": view.note_title,
                "note_rev": view.note_rev,
                "features": view.features,
                "document": view.doc,
            });
            let size = serde_json::to_string(&note_value).map_or(0, |text| text.len());
            if used + size + 1 > budget {
                return Err(UmError::new(
                    ErrorCode::LimitExceeded,
                    format!(
                        "note does not fit into the output budget ({budget} bytes, need {needed})",
                        needed = used + size + 1
                    ),
                )
                .with_hint(
                    "Raise --max-bytes, or use `insum tree` / `insum read --format annotated` for block-wise pagination.",
                ));
            }
            used += size + 1;
            notes.push(note_value);
        }
        return Ok(Output::Json(json!({
            "api_version": 1,
            "path": rendered,
            "file_rev": reader.file_rev()?,
            "notes": notes,
        })));
    }

    // annotated: paginate by top-level blocks across notes.
    // Page shape mirrors tree: the first note may be partial (mid-note
    // cursor); every following note is emitted whole or the page stops on
    // the note boundary with um:incomplete + cursor to it.
    let mut out = String::new();
    let mut pos = start_note;

    // first note of the page (partial allowed, mid-note cursor)
    if pos < reader.len() {
        let view = reader.read_at(pos)?;
        if let Some(cursor) = &cursor {
            // Unconditional rev binding; the bi⇔np invariant
            // is enforced inside validate
            cursor.validate(
                "read",
                &rendered,
                view.note_id.as_deref(),
                view.note_rev.as_str(),
                None,
                0,
            )?;
        }
        let start_block = cursor.as_ref().map_or(0, |c| c.block_index);
        let header = format!(
            "<!-- um:api_version=1 note_id=\"{}\" note_rev=\"{}\" -->\n",
            escape_comment_value(&view.note_id.clone().unwrap_or_default()),
            view.note_rev
        );
        out.push_str(&header);
        let mut used = out.len();
        let blocks = insum_core::traverse(&view.doc, None, limits.max_depth);
        if blocks.depth_exceeded {
            return Err(UmError::new(
                ErrorCode::LimitExceeded,
                format!("document deeper than UM_MAX_DEPTH ({})", limits.max_depth),
            ));
        }
        // Children are indexed once per note instead of O(B²) scans
        let children_of = children_index(&blocks.blocks);
        let top_level: Vec<&BlockEntry> = blocks.blocks.iter().filter(|b| b.depth == 0).collect();
        // Parity with tree: a mid-note resume pointer must match the
        // block at the resumed position
        if let Some(expected) = cursor.as_ref().and_then(|c| c.next_pointer.clone())
            && start_block > 0
            && !top_level
                .get(start_block)
                .is_some_and(|block| block.pointer == expected)
        {
            return Err(stale_cursor(
                "cursor block position does not match the recorded pointer",
            ));
        }
        let mut oversized: Vec<&BlockEntry> = Vec::new();
        for (block_position, block) in top_level.iter().enumerate().skip(start_block) {
            let mut rendered_block = String::new();
            render_block(block, &view.doc, &children_of, &mut rendered_block);
            let chunk = format!("\n{rendered_block}");
            if used + chunk.len() > budget && block_position > start_block {
                // whole blocks only — no silent or partial truncation.
                // W-01: flush the oversized markers collected so far BEFORE
                // the incomplete return — they belong to blocks already
                // emitted, whatever follows them.
                for pending in &oversized {
                    out.push_str(&format!(
                        "\n<!-- um:oversized-block pointer=\"{}\" budget=\"{}\" -->\n",
                        pending.pointer, budget
                    ));
                }
                let next_pointer = top_level.get(block_position).map(|b| b.pointer.clone());
                let cursor = Cursor::read(
                    rendered.clone(),
                    view.note_id.clone(),
                    view.note_rev.clone(),
                    pos,
                    block_position,
                    None,
                    0,
                    next_pointer,
                );
                out.push_str(&format!(
                    "\n<!-- um:incomplete next_cursor=\"{}\" -->\n",
                    cursor.encode()
                ));
                return Ok(Output::Text(out));
            }
            // PR review: a single block larger than the whole budget is still
            // emitted whole (documented exception) — but explicitly marked.
            if used + chunk.len() > budget {
                oversized.push(block);
            }
            used += chunk.len();
            out.push_str(&chunk);
        }
        for block in oversized {
            out.push_str(&format!(
                "\n<!-- um:oversized-block pointer=\"{}\" budget=\"{}\" -->\n",
                block.pointer, budget
            ));
        }
        pos += 1;
    }

    // following notes: whole-fit or boundary marker
    while pos < reader.len() {
        let view = reader.read_at(pos)?;
        let mut scratch = String::new();
        if !out.is_empty() {
            scratch.push('\n');
        }
        scratch.push_str(&format!(
            "<!-- um:api_version=1 note_id=\"{}\" note_rev=\"{}\" -->\n",
            view.note_id.clone().unwrap_or_default(),
            view.note_rev
        ));
        let blocks = insum_core::traverse(&view.doc, None, limits.max_depth);
        if blocks.depth_exceeded {
            return Err(UmError::new(
                ErrorCode::LimitExceeded,
                format!("document deeper than UM_MAX_DEPTH ({})", limits.max_depth),
            ));
        }
        let children_of = children_index(&blocks.blocks);
        for block in &blocks.blocks {
            if block.depth != 0 {
                continue;
            }
            let mut rendered_block = String::new();
            render_block(block, &view.doc, &children_of, &mut rendered_block);
            scratch.push_str(&format!("\n{rendered_block}"));
        }
        if out.len() + scratch.len() <= budget {
            out.push_str(&scratch);
            pos += 1;
            continue;
        }
        // Boundary marker + cursor to this note
        let cursor = Cursor::read(
            rendered.clone(),
            view.note_id.clone(),
            view.note_rev.clone(),
            pos,
            0,
            None,
            0,
            None,
        );
        out.push_str(&format!(
            "\n<!-- um:incomplete next_cursor=\"{}\" -->\n",
            cursor.encode()
        ));
        return Ok(Output::Text(out));
    }

    Ok(Output::Text(out))
}

fn stale_cursor(message: &str) -> UmError {
    UmError::new(insum_core::ErrorCode::CursorStale, message.to_owned())
        .with_hint("Re-run the command without --cursor and paginate from the start.")
}

/// Map parent pointer → direct block children, in document order.
fn children_index(blocks: &[BlockEntry]) -> HashMap<&str, Vec<&BlockEntry>> {
    let mut index: HashMap<&str, Vec<&BlockEntry>> = HashMap::new();
    for block in blocks {
        if let Some(parent) = block.parent_pointer.as_deref() {
            index.entry(parent).or_default().push(block);
        }
    }
    index
}

/// Render a block with its anchor; anchors are emitted for EVERY structural
/// block — nested paragraphs inside lists are addressable too (
/// «перед каждым блоком»).
fn render_block(
    block: &BlockEntry,
    doc: &Value,
    children_of: &HashMap<&str, Vec<&BlockEntry>>,
    out: &mut String,
) {
    let children = children_of
        .get(block.pointer.as_str())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let inline = block.text.clone();

    // attrs.id is document-controlled — escaped for the comment template
    let id_suffix = block
        .id
        .as_deref()
        .map(|id| format!(" um:id=\"{}\"", escape_comment_value(id)))
        .unwrap_or_default();
    out.push_str(&format!(
        "<!-- um:pointer=\"{}\"{} -->\n",
        block.pointer, id_suffix
    ));

    match block.node_type.as_str() {
        "heading" => {
            let level = block
                .attrs
                .get("level")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 6) as usize;
            out.push_str(&format!(
                "{} {}\n",
                "#".repeat(level),
                neutralize_marker_syntax(inline.as_deref().unwrap_or(""))
            ));
        }
        "paragraph" => {
            let rendered_inline = render_inline_nodes(block, doc);
            out.push_str(&rendered_inline);
            out.push('\n');
        }
        "noteTitle" => {
            out.push_str(&format!(
                "# {}\n",
                neutralize_marker_syntax(inline.as_deref().unwrap_or(""))
            ));
        }
        "noteSummary" => {
            out.push_str(&neutralize_marker_syntax(inline.as_deref().unwrap_or("")));
            out.push('\n');
        }
        "noteProperty" => {
            let name = neutralize_marker_syntax(
                &children
                    .iter()
                    .find(|child| child.node_type == "notePropertyName")
                    .and_then(|child| child.text.clone())
                    .unwrap_or_default(),
            );
            let value: Vec<String> = children
                .iter()
                .filter(|child| child.node_type == "notePropertyValue")
                .filter_map(|child| child.text.clone().map(|t| neutralize_marker_syntax(&t)))
                .collect();
            out.push_str(&format!("{name}: {}\n", value.join(" ")));
        }
        "bulletList" | "taskList" | "orderedList" => {
            for (offset, child) in children.iter().enumerate() {
                let marker = match block.node_type.as_str() {
                    "orderedList" => {
                        let start = block
                            .attrs
                            .get("start")
                            .and_then(Value::as_u64)
                            .unwrap_or(1);
                        format!("{}. ", start + offset as u64)
                    }
                    _ => "- ".to_owned(),
                };
                render_list_item(child, doc, children_of, &marker, "  ", out);
            }
        }
        "blockquote" => {
            let mut inner = String::new();
            for child in children {
                render_block(child, doc, children_of, &mut inner);
            }
            for line in inner.lines() {
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
        }
        "hljsCodeBlock" => {
            let language = escape_comment_value(
                block
                    .attrs
                    .get("class")
                    .and_then(Value::as_str)
                    .and_then(|class| class.strip_prefix("language-"))
                    .unwrap_or(""),
            )
            .replace('\n', " ")
            .replace('`', "");
            out.push_str("```");
            out.push_str(&language);
            out.push('\n');
            for child in children {
                if let Some(text) = &child.text {
                    out.push_str(&neutralize_marker_syntax(text));
                    out.push('\n');
                }
            }
            out.push_str("```\n");
        }
        "mermaid" => {
            out.push_str("```mermaid\n");
            if let Some(code) = block.attrs.get("code").and_then(Value::as_str) {
                out.push_str(&neutralize_marker_syntax(code));
                out.push('\n');
            }
            out.push_str("```\n");
        }
        "horizontalRule" => {
            out.push_str("---\n");
        }
        "table" => {
            // markdown table: rows of cells. Every structural level (row,
            // cell, cell paragraph) carries an inline pointer anchor — a
            // standalone comment line would split the markdown table, so
            // anchors ride inside the cells. Cell content is rendered with
            // the same fidelity as top-level blocks: paragraphs go through
            // the inline projection (marks, markers), non-paragraph children
            // are inlined recursively, anything unrenderable gets an
            // explicit marker — no silent losses.
            let rows: Vec<&BlockEntry> = children
                .iter()
                .filter(|row| row.node_type == "tableRow")
                .copied()
                .collect();
            if rows.is_empty() {
                out.push_str(&unrendered_marker(block));
                for child in children {
                    out.push_str(&unrendered_marker(child));
                }
            } else {
                for (index, row) in rows.iter().enumerate() {
                    let cells = children_of
                        .get(row.pointer.as_str())
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    if cells.is_empty() {
                        // W-06: an empty row keeps its own anchor — block get
                        // by this pointer must be explainable in the output
                        out.push_str(&format!("| {} |\n", anchor_comment(row)));
                        continue;
                    }
                    let rendered_cells: Vec<String> = cells
                        .iter()
                        .enumerate()
                        .map(|(cell_index, cell)| {
                            let mut cell_out = String::new();
                            // the row anchor rides in the first cell
                            if cell_index == 0 {
                                cell_out.push_str(&anchor_comment(row));
                            }
                            cell_out.push_str(&anchor_comment(cell));
                            let mut paragraph_seen = false;
                            for grandchild in children_of
                                .get(cell.pointer.as_str())
                                .map(Vec::as_slice)
                                .unwrap_or(&[])
                            {
                                // W-05: paragraphs are separated (the old
                                // renderer joined them with a space)
                                if paragraph_seen {
                                    cell_out.push(' ');
                                }
                                if grandchild.node_type == "paragraph" {
                                    paragraph_seen = true;
                                    cell_out.push_str(&anchor_comment(grandchild));
                                    cell_out.push_str(&render_inline_nodes(grandchild, doc));
                                } else {
                                    cell_out.push_str(&anchor_comment(grandchild));
                                    cell_out.push_str(&cell_child_inline(
                                        grandchild,
                                        doc,
                                        children_of,
                                    ));
                                }
                            }
                            if cell_out.trim().is_empty() {
                                " ".to_owned()
                            } else {
                                escape_cell(&cell_out)
                            }
                        })
                        .collect();
                    out.push_str(&format!("| {} |\n", rendered_cells.join(" | ")));
                    if index == 0 {
                        let separator: Vec<&str> = rendered_cells.iter().map(|_| "---").collect();
                        out.push_str(&format!("| {} |\n", separator.join(" | ")));
                    }
                }
                // table children that are not rows: explicit markers after
                // the table (a standalone comment inside it would split it)
                for child in children {
                    if child.node_type != "tableRow" {
                        out.push_str(&unrendered_marker(child));
                    }
                }
            }
        }
        _ => {
            // known-but-unrendered or unknown nodes stay explicit
            out.push_str(&format!(
                "<!-- um:unrendered type=\"{}\" pointer=\"{}\" -->\n",
                escape_comment_value(&block.node_type),
                block.pointer
            ));
        }
    }
}

/// Escape a document-controlled value interpolated into a CLI comment
/// template: both comment delimiters are broken so the value can neither
/// open a forged comment nor close the CLI's own one early (review X-01
/// class 3). Double quotes and newlines are also neutralized (review
/// Y-01/Y-02): a quote cannot terminate an attribute to inject a new one,
/// and a newline cannot split the header into a separate forged line.
fn escape_comment_value(value: &str) -> String {
    value
        .replace("<!--", "<! --")
        .replace("-->", "-- >")
        .replace('"', "\\\"")
        .replace('\n', " ")
        .replace('\r', "")
}

/// Apply projection marks to a text piece; unknown mark types are collected
/// for the trailing marker.
fn apply_inline_marks(text: &str, marks: Option<&Vec<Value>>, unknown: &mut Vec<String>) -> String {
    let mut wrapped = text.to_owned();
    if let Some(marks) = marks {
        for mark in marks {
            match mark.get("type").and_then(Value::as_str).unwrap_or("") {
                "bold" | "strong" => wrapped = format!("**{wrapped}**"),
                "italic" | "em" => wrapped = format!("*{wrapped}*"),
                "code" => wrapped = format!("`{wrapped}`"),
                "strike" => wrapped = format!("~~{wrapped}~~"),
                "" => {}
                other => unknown.push(other.to_owned()),
            }
        }
    }
    wrapped
}

/// Render the inline content of a paragraph-like block with markdown marks
/// (known marks are projected; unknown marks get an explicit trailing
/// marker instead of a silent loss).
///
/// Boundary-based neutralization (review X-01): document-sourced pieces
/// (text runs, hard breaks, wikiLink projections) accumulate into a run
/// that is neutralized AS A WHOLE on flush — a forged `<!--` split across
/// adjacent text nodes forms (and is broken) inside the composed run, so
/// per-node neutralization cannot be bypassed. CLI-emitted markers
/// (unrendered-inline) are interleaved between runs and never pass through
/// the neutralizer.
fn render_inline_nodes(block: &BlockEntry, doc: &Value) -> String {
    let raw_node = insum_core::resolve(doc, &block.pointer)
        .cloned()
        .unwrap_or(Value::Null);
    let mut out = String::new();
    let mut doc_run = String::new();
    let mut unknown_marks: Vec<String> = Vec::new();
    if let Some(children) = raw_node.get("content").and_then(Value::as_array) {
        for child in children {
            let child_type = child.get("type").and_then(Value::as_str).unwrap_or("");
            match child_type {
                "text" => {
                    let text = child.get("text").and_then(Value::as_str).unwrap_or("");
                    doc_run.push_str(&apply_inline_marks(
                        text,
                        child.get("marks").and_then(Value::as_array),
                        &mut unknown_marks,
                    ));
                }
                "hardBreak" => doc_run.push('\n'),
                "wikiLink" => {
                    // renders as its label; keep the link target explicit.
                    // label/path are document-controlled — they join the run
                    // and are neutralized with it (X-01 class 2).
                    let attrs = child.get("attrs");
                    let label = attrs
                        .and_then(|a| a.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let path = attrs
                        .and_then(|a| a.get("path"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    doc_run.push_str(&format!("[{label}]({path})"));
                }
                other => {
                    if !INLINE_NODE_TYPES.contains(&other) {
                        // flush the document run BEFORE the CLI marker: the
                        // neutralization applies to the composed run, the
                        // marker template stays intact
                        out.push_str(&neutralize_marker_syntax(&doc_run));
                        doc_run.clear();
                        out.push_str(&format!(
                            "<!-- um:unrendered-inline type=\"{}\" -->",
                            escape_comment_value(other)
                        ));
                    }
                }
            }
        }
    }
    out.push_str(&neutralize_marker_syntax(&doc_run));
    unknown_marks.sort();
    unknown_marks.dedup();
    if !unknown_marks.is_empty() {
        let escaped: Vec<String> = unknown_marks
            .iter()
            .map(|mark| escape_comment_value(mark))
            .collect();
        out.push_str(&format!(
            " <!-- um:unrendered-marks=\"{}\" -->",
            escaped.join(",")
        ));
    }
    out
}

/// The list marker / checkbox survives a first child that is not a
/// paragraph — the marker line is always emitted, non-paragraph children
/// render as indented blocks (the first paragraph joins
/// the marker line instead of duplicating).
/// Inline pointer anchor for a block (used inside table cells, where a
/// standalone comment line would split the markdown table).
/// Neutralize the comment-opener sequence in document-sourced text: it must
/// not forge CLI markers or anchors in the annotated projection (review
/// W-02/V-06). `<!--` becomes `<! --` — readable, visibly altered, and
/// impossible to reassemble into `<!--` without editing.
fn neutralize_marker_syntax(text: &str) -> String {
    text.replace("<!--", "<! --")
}

fn anchor_comment(block: &BlockEntry) -> String {
    format!("<!-- um:pointer=\"{}\" -->", block.pointer)
}

/// Unrendered marker for a block. The type is document-controlled and
/// escaped (X-01 class 3); the pointer is CLI-computed.
fn unrendered_marker(block: &BlockEntry) -> String {
    format!(
        "<!-- um:unrendered type=\"{}\" pointer=\"{}\" -->\n",
        escape_comment_value(&block.node_type),
        block.pointer
    )
}

/// Cell-safe rendering: newlines become <br>, pipes are escaped — composed
/// after the content so the anchors themselves stay intact.
fn escape_cell(text: &str) -> String {
    text.replace('\n', "<br>").replace('|', "\\|")
}

/// Inline rendering of a non-paragraph cell child: lists flatten to
/// marker-prefixed lines (marks preserved via the inline projection),
/// nested tables and unknown nodes stay explicit.
fn cell_child_inline(
    child: &BlockEntry,
    doc: &Value,
    children_of: &HashMap<&str, Vec<&BlockEntry>>,
) -> String {
    match child.node_type.as_str() {
        "bulletList" | "taskList" | "orderedList" => {
            let mut parts = Vec::new();
            inline_list_lines(child, doc, children_of, "- ", &mut parts);
            if parts.is_empty() {
                format!(
                    "<!-- um:unrendered type=\"{}\" pointer=\"{}\" -->",
                    escape_comment_value(&child.node_type),
                    child.pointer
                )
            } else {
                parts.join("<br>")
            }
        }
        "table" => format!(
            "<!-- um:unrendered type=\"table\" pointer=\"{}\" -->",
            child.pointer
        ),
        _ => {
            let text = neutralize_marker_syntax(&child.text.clone().unwrap_or_default());
            format!(
                "<!-- um:unrendered type=\"{}\" pointer=\"{}\" -->{}",
                escape_comment_value(&child.node_type),
                child.pointer,
                text
            )
        }
    }
}

/// Flatten a list into cell-safe lines: markers/checkboxes, inline marks,
/// nested lists indented one level deeper.
fn inline_list_lines(
    list: &BlockEntry,
    doc: &Value,
    children_of: &HashMap<&str, Vec<&BlockEntry>>,
    indent: &str,
    parts: &mut Vec<String>,
) {
    let start = list.attrs.get("start").and_then(Value::as_u64).unwrap_or(1);
    for (offset, item) in children_of
        .get(list.pointer.as_str())
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let marker = match list.node_type.as_str() {
            "orderedList" => format!("{indent}{}. ", start + offset as u64),
            "taskList" => {
                let checked = item
                    .attrs
                    .get("checked")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                format!("{indent}[{}] ", if checked { "x" } else { " " })
            }
            _ => format!("{indent}- "),
        };
        for item_child in children_of
            .get(item.pointer.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            if item_child.node_type == "paragraph" {
                let text = render_inline_nodes(item_child, doc);
                for (line_index, line) in text.split('\n').enumerate() {
                    let nested = format!("{indent}  ");
                    let lead: &str = if line_index == 0 {
                        marker.as_str()
                    } else {
                        nested.as_str()
                    };
                    parts.push(format!("{lead}{line}"));
                }
            } else if matches!(
                item_child.node_type.as_str(),
                "bulletList" | "taskList" | "orderedList"
            ) {
                inline_list_lines(item_child, doc, children_of, &format!("{indent}  "), parts);
            } else {
                // parity with the top-level renderer: anything unrenderable
                // gets an explicit marker — empty text is still a loss marker
                let text = neutralize_marker_syntax(&item_child.text.clone().unwrap_or_default());
                parts.push(format!(
                    "{marker}<!-- um:unrendered type=\"{}\" pointer=\"{}\" -->{text}",
                    escape_comment_value(&item_child.node_type),
                    item_child.pointer
                ));
            }
        }
    }
}

fn render_list_item(
    item: &BlockEntry,
    doc: &Value,
    children_of: &HashMap<&str, Vec<&BlockEntry>>,
    marker: &str,
    indent: &str,
    out: &mut String,
) {
    // The item is a block and gets its own anchor
    out.push_str(&format!("<!-- um:pointer=\"{}\" -->\n", item.pointer));
    let children = children_of
        .get(item.pointer.as_str())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut first_line = String::new();
    if let Some(checked) = item.attrs.get("checked") {
        first_line.push_str(if checked.as_bool().unwrap_or(false) {
            "- [x] "
        } else {
            "- [ ] "
        });
    } else {
        first_line.push_str(marker);
    }
    let tight_first = children
        .first()
        .is_some_and(|first| first.node_type == "paragraph");
    if !tight_first {
        // Keep the marker even without a leading paragraph
        out.push_str(&first_line);
        out.push('\n');
    }
    for (position, child) in children.iter().enumerate() {
        if position == 0 && tight_first {
            // Only the first *paragraph* contributes to the marker
            // line; its anchor precedes that line
            out.push_str(&format!("<!-- um:pointer=\"{}\" -->\n", child.pointer));
            let text = render_inline_nodes(child, doc);
            first_line.push_str(text.trim_end_matches('\n'));
            out.push_str(&first_line);
            out.push('\n');
            continue;
        }
        // non-paragraph children (and extra paragraphs) render as blocks
        let mut nested = String::new();
        render_block(child, doc, children_of, &mut nested);
        for line in nested.lines() {
            out.push_str(indent);
            out.push_str(line);
            out.push('\n');
        }
    }
}
