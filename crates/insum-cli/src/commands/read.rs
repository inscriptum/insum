//! `insum read` — whole-note reading: JSON envelope or annotated projection
//! (spec §6.5). The projection is read-only text with pointer anchors; it is
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
        // C-08: --cursor was silently ignored on json output — reject instead
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
    // NEW-2: no cursor-based narrowing — a multi-note container is read
    // across notes; the lazy reader keeps the per-page cost bounded (P-02).
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
        // P-01: the budget is per response — no silent oversize; the command
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

    // annotated: paginate by top-level blocks across notes (spec §6.5).
    // Page shape mirrors tree: the first note may be partial (mid-note
    // cursor); every following note is emitted whole or the page stops on
    // the note boundary with um:incomplete + cursor to it (NEW-2).
    let mut out = String::new();
    let mut pos = start_note;

    // first note of the page (partial allowed, mid-note cursor)
    if pos < reader.len() {
        let view = reader.read_at(pos)?;
        if let Some(cursor) = &cursor {
            // NEW-4: unconditional rev binding; NEW-5: the bi⇔np invariant
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
            view.note_id.clone().unwrap_or_default(),
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
        // P-06: children are indexed once per note instead of O(B²) scans
        let children_of = children_index(&blocks.blocks);
        let top_level: Vec<&BlockEntry> = blocks.blocks.iter().filter(|b| b.depth == 0).collect();
        // S-04 parity with tree: a mid-note resume pointer must match the
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
        for (block_position, block) in top_level.iter().enumerate().skip(start_block) {
            let mut rendered_block = String::new();
            render_block(block, &view.doc, &children_of, &mut rendered_block);
            let chunk = format!("\n{rendered_block}");
            if used + chunk.len() > budget && block_position > start_block {
                // whole blocks only — no silent or partial truncation
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
            used += chunk.len();
            out.push_str(&chunk);
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
        // NEW-2: boundary marker + cursor to this note
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
/// block — nested paragraphs inside lists are addressable too (review K-06,
/// spec §6.5 «перед каждым блоком»).
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

    let id_suffix = block
        .id
        .as_deref()
        .map(|id| format!(" um:id=\"{id}\""))
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
                inline.unwrap_or_default()
            ));
        }
        "paragraph" => {
            let rendered_inline = render_inline_nodes(block, doc);
            out.push_str(&rendered_inline);
            out.push('\n');
        }
        "noteTitle" => {
            out.push_str(&format!("# {}\n", inline.unwrap_or_default()));
        }
        "noteSummary" => {
            out.push_str(&inline.unwrap_or_default());
            out.push('\n');
        }
        "noteProperty" => {
            let name = children
                .iter()
                .find(|child| child.node_type == "notePropertyName")
                .and_then(|child| child.text.clone())
                .unwrap_or_default();
            let value: Vec<String> = children
                .iter()
                .filter(|child| child.node_type == "notePropertyValue")
                .filter_map(|child| child.text.clone())
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
            let language = block
                .attrs
                .get("class")
                .and_then(Value::as_str)
                .and_then(|class| class.strip_prefix("language-"))
                .unwrap_or("");
            out.push_str("```");
            out.push_str(language);
            out.push('\n');
            for child in children {
                if let Some(text) = &child.text {
                    out.push_str(text);
                    out.push('\n');
                }
            }
            out.push_str("```\n");
        }
        "mermaid" => {
            out.push_str("```mermaid\n");
            if let Some(code) = block.attrs.get("code").and_then(Value::as_str) {
                out.push_str(code);
                out.push('\n');
            }
            out.push_str("```\n");
        }
        "horizontalRule" => {
            out.push_str("---\n");
        }
        "table" => {
            // markdown table: rows of cells; a cell is paragraphs — their
            // inline text, newlines as <br>, pipes escaped
            let rows: Vec<&BlockEntry> = children
                .iter()
                .filter(|row| row.node_type == "tableRow")
                .copied()
                .collect();
            if rows.is_empty() {
                out.push_str(&format!(
                    "<!-- um:unrendered type=\"{}\" pointer=\"{}\" -->\n",
                    block.node_type, block.pointer
                ));
            } else {
                for (index, row) in rows.iter().enumerate() {
                    let cells = children_of
                        .get(row.pointer.as_str())
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    let texts: Vec<String> = cells
                        .iter()
                        .map(|cell| {
                            let paragraphs: Vec<String> = children_of
                                .get(cell.pointer.as_str())
                                .map(|cs| {
                                    cs.iter()
                                        .filter(|p| p.node_type == "paragraph")
                                        .filter_map(|p| p.text.clone())
                                        .collect()
                                })
                                .unwrap_or_default();
                            let text = if paragraphs.is_empty() {
                                cell.text.clone().unwrap_or_default()
                            } else {
                                paragraphs.join(" ")
                            };
                            let text = text.replace('\n', "<br>").replace('|', "\\|");
                            if text.trim().is_empty() {
                                " ".to_owned()
                            } else {
                                text
                            }
                        })
                        .collect();
                    out.push_str(&format!("| {} |\n", texts.join(" | ")));
                    if index == 0 {
                        let separator: Vec<&str> = texts.iter().map(|_| "---").collect();
                        out.push_str(&format!("| {} |\n", separator.join(" | ")));
                    }
                }
            }
        }
        _ => {
            // known-but-unrendered or unknown nodes stay explicit (spec §6.5)
            out.push_str(&format!(
                "<!-- um:unrendered type=\"{}\" pointer=\"{}\" -->\n",
                block.node_type, block.pointer
            ));
        }
    }
}

/// Render the inline content of a paragraph-like block with markdown marks
/// (review K-08: known marks are projected; unknown marks get an explicit
/// trailing marker instead of a silent loss, spec §6.5).
fn render_inline_nodes(block: &BlockEntry, doc: &Value) -> String {
    let raw_node = insum_core::resolve(doc, &block.pointer)
        .cloned()
        .unwrap_or(Value::Null);
    let mut out = String::new();
    let mut unknown_marks: Vec<&str> = Vec::new();
    if let Some(children) = raw_node.get("content").and_then(Value::as_array) {
        for child in children {
            let child_type = child.get("type").and_then(Value::as_str).unwrap_or("");
            match child_type {
                "text" => {
                    let text = child.get("text").and_then(Value::as_str).unwrap_or("");
                    let marks = child.get("marks").and_then(Value::as_array);
                    let mut wrapped = text.to_owned();
                    if let Some(marks) = marks {
                        for mark in marks {
                            let mark_type = mark.get("type").and_then(Value::as_str).unwrap_or("");
                            match mark_type {
                                "bold" | "strong" => {
                                    wrapped = format!("**{wrapped}**");
                                }
                                "italic" | "em" => {
                                    wrapped = format!("*{wrapped}*");
                                }
                                "code" => {
                                    wrapped = format!("`{wrapped}`");
                                }
                                "strike" => {
                                    wrapped = format!("~~{wrapped}~~");
                                }
                                "" => {}
                                other => unknown_marks.push(other),
                            }
                        }
                    }
                    out.push_str(&wrapped);
                }
                "hardBreak" => out.push('\n'),
                "wikiLink" => {
                    // renders as its label; keep the link target explicit
                    let attrs = child.get("attrs");
                    let label = attrs
                        .and_then(|a| a.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let path = attrs
                        .and_then(|a| a.get("path"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    out.push_str(&format!("[{label}]({path})"));
                }
                other => {
                    if !INLINE_NODE_TYPES.contains(&other) {
                        out.push_str(&format!("<!-- um:unrendered-inline type=\"{other}\" -->"));
                    }
                }
            }
        }
    }
    unknown_marks.sort();
    unknown_marks.dedup();
    if !unknown_marks.is_empty() {
        out.push_str(&format!(
            " <!-- um:unrendered-marks=\"{}\" -->",
            unknown_marks.join(",")
        ));
    }
    out
}

/// NEW-3: the list marker / checkbox survives a first child that is not a
/// paragraph — the marker line is always emitted, non-paragraph children
/// render as indented blocks (review C-09 stays: the first paragraph joins
/// the marker line instead of duplicating).
fn render_list_item(
    item: &BlockEntry,
    doc: &Value,
    children_of: &HashMap<&str, Vec<&BlockEntry>>,
    marker: &str,
    indent: &str,
    out: &mut String,
) {
    // K-06: the item is a block and gets its own anchor
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
        // NEW-3: keep the marker even without a leading paragraph
        out.push_str(&first_line);
        out.push('\n');
    }
    for (position, child) in children.iter().enumerate() {
        if position == 0 && tight_first {
            // C-09: only the first *paragraph* contributes to the marker
            // line; its anchor precedes that line (K-06)
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
