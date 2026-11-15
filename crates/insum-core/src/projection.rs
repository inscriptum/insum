//! Markdown projection of a note document for agent reading (§6.5).
//!
//! ONE rendering pipeline, two output modes selected by [`Projection`]:
//!
//! - `anchors: true` — the `annotated` projection: a `um:pointer`/`um:id`
//!   anchor comment before every structural block (nested paragraphs inside
//!   lists are addressable too), CLI markers as HTML comments;
//! - `anchors: false` — the `compact` projection: pure markdown with not a
//!   single service comment; projection losses stay explicit as visible
//!   text markers instead (§3.5 — no silent losses).
//!
//! Byte contract: `anchors: true` reproduces the annotated projection
//! byte-for-byte; `anchors: false` differs only by the removed anchors and
//! the comment → text marker shape. The projection is a read-only view: it
//! is never a storage format, has no parser and is never fed to `apply`;
//! exact reading is JSON. Table cells are escaped by the single GFM shaping
//! of [`crate::tables::escape_cell_text`] — the read projection does not
//! keep a second renderer.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::model::{BlockEntry, INLINE_NODE_TYPES};
use crate::tables::escape_cell_text;

/// Escape a document-controlled value interpolated into a CLI comment
/// template: both comment delimiters are broken so the value can neither
/// open a forged comment nor close the CLI's own one early (review X-01
/// class 3). Double quotes and newlines are also neutralized (review
/// Y-01/Y-02): a quote cannot terminate an attribute to inject a new one,
/// and a newline cannot split the header into a separate forged line.
pub fn escape_comment_value(value: &str) -> String {
    value
        // deletions first: no later step can reassemble what was removed
        .replace('\r', "")
        // structural escapes: backslash before quote (Z-05), then delimiters
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace("<!--", "<! --")
        .replace("-->", "-- >")
        .replace("--!>", "-- !>")
        .replace('\n', " ")
}

/// The projection renderer. One pipeline, anchors on (annotated) or off
/// (compact); every marker the CLI or the renderer emits goes through this
/// type so the two modes cannot drift apart.
pub struct Projection {
    anchors: bool,
}

impl Projection {
    pub fn new(anchors: bool) -> Self {
        Self { anchors }
    }

    /// `true` for the annotated shape (header + anchor comments + comment
    /// markers), `false` for the anchor-free compact shape.
    pub fn anchors(&self) -> bool {
        self.anchors
    }

    /// A CLI-emitted service marker (pagination, oversized blocks) in the
    /// mode's shape: an HTML comment for annotated, a visible text marker
    /// for compact — a compact page must not contain a single comment.
    pub fn service_marker(&self, content: &str) -> String {
        if self.anchors {
            format!("<!-- {content} -->")
        } else {
            format!("[{content}]")
        }
    }

    /// Map parent pointer → direct block children, in document order.
    pub fn children_index(blocks: &[BlockEntry]) -> HashMap<&str, Vec<&BlockEntry>> {
        let mut index: HashMap<&str, Vec<&BlockEntry>> = HashMap::new();
        for block in blocks {
            if let Some(parent) = block.parent_pointer.as_deref() {
                index.entry(parent).or_default().push(block);
            }
        }
        index
    }

    /// Anchor comment line before a structural block (annotated only);
    /// compact emits nothing here — that is the projection's difference.
    /// attrs.id is document-controlled — escaped for the comment template.
    fn block_anchor(&self, block: &BlockEntry) -> String {
        if !self.anchors {
            return String::new();
        }
        let id_suffix = block
            .id
            .as_deref()
            .map(|id| format!(" um:id=\"{}\"", escape_comment_value(id)))
            .unwrap_or_default();
        format!("<!-- um:pointer=\"{}\"{} -->\n", block.pointer, id_suffix)
    }

    /// Pointer-only anchor for places where a standalone comment line would
    /// split the markdown (inside table cells) or where the current
    /// annotated output never carried an id (list items).
    fn pointer_anchor(&self, pointer: &str, newline: bool) -> String {
        if !self.anchors {
            return String::new();
        }
        if newline {
            format!("<!-- um:pointer=\"{pointer}\" -->\n")
        } else {
            format!("<!-- um:pointer=\"{pointer}\" -->")
        }
    }

    /// Unrendered marker for a block. The type is document-controlled and
    /// escaped (X-01 class 3); the pointer is CLI-computed. Comment in the
    /// annotated mode, visible text marker in compact — losses are never
    /// silent in either (§3.5).
    fn unrendered_marker(&self, block: &BlockEntry) -> String {
        format!(
            "{}\n",
            self.service_marker(&format!(
                "um:unrendered type=\"{}\" pointer=\"{}\"",
                escape_comment_value(&block.node_type),
                block.pointer
            ))
        )
    }

    /// Render a block (and its children) into `out`.
    pub fn render_block(
        &self,
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

        out.push_str(&self.block_anchor(block));

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
                let rendered_inline = self.render_inline_nodes(block, doc);
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
                    self.render_list_item(child, doc, children_of, &marker, "  ", out);
                }
            }
            "blockquote" => {
                let mut inner = String::new();
                for child in children {
                    self.render_block(child, doc, children_of, &mut inner);
                }
                for line in inner.lines() {
                    out.push_str("> ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
            "hljsCodeBlock" => {
                // deletion-first: backticks removed BEFORE escape — a backtick
                // surviving into the escaped language could glue `<!` + `--` into
                // a live `<!--` on the info line (review N-01)
                let language = block
                    .attrs
                    .get("class")
                    .and_then(Value::as_str)
                    .and_then(|class| class.strip_prefix("language-"))
                    .unwrap_or("")
                    .replace('`', "")
                    .replace('\n', " ");
                let language = escape_comment_value(&language);
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
                self.render_table(block, children, doc, children_of, out);
            }
            _ => {
                // known-but-unrendered or unknown nodes stay explicit
                out.push_str(&self.unrendered_marker(block));
            }
        }
    }

    /// Markdown table: rows of cells. Every structural level (row, cell,
    /// cell paragraph) carries an inline pointer anchor in the annotated
    /// mode — a standalone comment line would split the markdown table, so
    /// anchors ride inside the cells. Cell content is rendered with the
    /// same fidelity as top-level blocks: paragraphs go through the inline
    /// projection (marks, markers), non-paragraph children are inlined
    /// recursively, anything unrenderable gets an explicit marker — no
    /// silent losses. Pipes are escaped by the shared GFM shaping
    /// (`tables::escape_cell_text`).
    fn render_table(
        &self,
        block: &BlockEntry,
        children: &[&BlockEntry],
        doc: &Value,
        children_of: &HashMap<&str, Vec<&BlockEntry>>,
        out: &mut String,
    ) {
        let rows: Vec<&BlockEntry> = children
            .iter()
            .filter(|row| row.node_type == "tableRow")
            .copied()
            .collect();
        if rows.is_empty() {
            out.push_str(&self.unrendered_marker(block));
            for child in children {
                out.push_str(&self.unrendered_marker(child));
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
                    out.push_str(&format!(
                        "| {} |\n",
                        self.pointer_anchor(&row.pointer, false)
                    ));
                    continue;
                }
                let rendered_cells: Vec<String> = cells
                    .iter()
                    .enumerate()
                    .map(|(cell_index, cell)| {
                        let mut cell_out = String::new();
                        // the row anchor rides in the first cell
                        if cell_index == 0 {
                            cell_out.push_str(&self.pointer_anchor(&row.pointer, false));
                        }
                        cell_out.push_str(&self.pointer_anchor(&cell.pointer, false));
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
                                cell_out.push_str(&self.pointer_anchor(&grandchild.pointer, false));
                                cell_out.push_str(&self.render_inline_nodes(grandchild, doc));
                            } else {
                                cell_out.push_str(&self.pointer_anchor(&grandchild.pointer, false));
                                cell_out.push_str(&self.cell_child_inline(
                                    grandchild,
                                    doc,
                                    children_of,
                                ));
                            }
                        }
                        if cell_out.trim().is_empty() {
                            " ".to_owned()
                        } else {
                            escape_cell_text(&cell_out)
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
                    out.push_str(&self.unrendered_marker(child));
                }
            }
        }
    }

    /// Apply projection marks to a text piece; unknown mark types are collected
    /// for the trailing marker.
    fn apply_inline_marks(
        text: &str,
        marks: Option<&Vec<Value>>,
        unknown: &mut Vec<String>,
    ) -> String {
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
    fn render_inline_nodes(&self, block: &BlockEntry, doc: &Value) -> String {
        let raw_node = crate::resolve(doc, &block.pointer)
            .cloned()
            .unwrap_or(json!(null));
        let mut out = String::new();
        let mut doc_run = String::new();
        let mut unknown_marks: Vec<String> = Vec::new();
        if let Some(children) = raw_node.get("content").and_then(Value::as_array) {
            for child in children {
                let child_type = child.get("type").and_then(Value::as_str).unwrap_or("");
                match child_type {
                    "text" => {
                        let text = child.get("text").and_then(Value::as_str).unwrap_or("");
                        doc_run.push_str(&Self::apply_inline_marks(
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
                            out.push_str(&self.service_marker(&format!(
                                "um:unrendered-inline type=\"{}\"",
                                escape_comment_value(other)
                            )));
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
                " {}",
                self.service_marker(&format!("um:unrendered-marks=\"{}\"", escaped.join(",")))
            ));
        }
        out
    }

    /// Inline rendering of a non-paragraph cell child: lists flatten to
    /// marker-prefixed lines (marks preserved via the inline projection),
    /// nested tables and unknown nodes stay explicit.
    fn cell_child_inline(
        &self,
        child: &BlockEntry,
        doc: &Value,
        children_of: &HashMap<&str, Vec<&BlockEntry>>,
    ) -> String {
        match child.node_type.as_str() {
            "bulletList" | "taskList" | "orderedList" => {
                let mut parts = Vec::new();
                self.inline_list_lines(child, doc, children_of, "- ", &mut parts);
                if parts.is_empty() {
                    self.service_marker(&format!(
                        "um:unrendered type=\"{}\" pointer=\"{}\"",
                        escape_comment_value(&child.node_type),
                        child.pointer
                    ))
                } else {
                    parts.join("<br>")
                }
            }
            "table" => self.service_marker(&format!(
                "um:unrendered type=\"table\" pointer=\"{}\"",
                child.pointer
            )),
            _ => {
                let text = neutralize_marker_syntax(&child.text.clone().unwrap_or_default());
                format!(
                    "{}{}",
                    self.service_marker(&format!(
                        "um:unrendered type=\"{}\" pointer=\"{}\"",
                        escape_comment_value(&child.node_type),
                        child.pointer
                    )),
                    text
                )
            }
        }
    }

    /// Flatten a list into cell-safe lines: markers/checkboxes, inline marks,
    /// nested lists indented one level deeper.
    fn inline_list_lines(
        &self,
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
                    let text = self.render_inline_nodes(item_child, doc);
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
                    self.inline_list_lines(
                        item_child,
                        doc,
                        children_of,
                        &format!("{indent}  "),
                        parts,
                    );
                } else {
                    // parity with the top-level renderer: anything unrenderable
                    // gets an explicit marker — empty text is still a loss marker
                    let text =
                        neutralize_marker_syntax(&item_child.text.clone().unwrap_or_default());
                    parts.push(format!(
                        "{}{}{text}",
                        marker,
                        self.service_marker(&format!(
                            "um:unrendered type=\"{}\" pointer=\"{}\"",
                            escape_comment_value(&item_child.node_type),
                            item_child.pointer
                        ))
                    ));
                }
            }
        }
    }

    /// The list marker / checkbox survives a first child that is not a
    /// paragraph — the marker line is always emitted, non-paragraph children
    /// render as indented blocks (the first paragraph joins
    /// the marker line instead of duplicating).
    fn render_list_item(
        &self,
        item: &BlockEntry,
        doc: &Value,
        children_of: &HashMap<&str, Vec<&BlockEntry>>,
        marker: &str,
        indent: &str,
        out: &mut String,
    ) {
        // The item is a block and gets its own anchor
        out.push_str(&self.pointer_anchor(&item.pointer, true));
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
                out.push_str(&self.pointer_anchor(&child.pointer, true));
                let text = self.render_inline_nodes(child, doc);
                first_line.push_str(text.trim_end_matches('\n'));
                out.push_str(&first_line);
                out.push('\n');
                continue;
            }
            // non-paragraph children (and extra paragraphs) render as blocks
            let mut nested = String::new();
            self.render_block(child, doc, children_of, &mut nested);
            for line in nested.lines() {
                out.push_str(indent);
                out.push_str(line);
                out.push('\n');
            }
        }
    }
}

/// Neutralize the comment-opener sequence in document-sourced text: it must
/// not forge CLI markers or anchors in the projection (review W-02/V-06).
/// `<!--` becomes `<! --` — readable, visibly altered, and impossible to
/// reassemble into `<!--` without editing.
fn neutralize_marker_syntax(text: &str) -> String {
    // CR removal first (Z-03): a CR-stripping consumer cannot reassemble
    // delimiters from text that had its CR removed before neutralization
    text.replace('\r', "").replace("<!--", "<! --")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(s: &str) -> Value {
        json!({"type": "text", "text": s})
    }

    fn paragraph(content: &[Value]) -> Value {
        json!({ "type": "paragraph", "content": content })
    }

    /// Render every top-level block of `doc` in the given mode.
    fn render(doc: &Value, anchors: bool) -> String {
        let blocks = crate::traverse(doc, None, 128);
        assert!(!blocks.depth_exceeded, "fixture fits under max depth");
        let children_of = Projection::children_index(&blocks.blocks);
        let projection = Projection::new(anchors);
        let mut out = String::new();
        for block in blocks.blocks.iter().filter(|b| b.depth == 0) {
            projection.render_block(block, doc, &children_of, &mut out);
        }
        out
    }

    /// A document covering every supported block/inline shape of the
    /// compact projection (no tables — the anchor-stripped equality below
    /// relies on anchors that occupy whole lines).
    fn rich_doc() -> Value {
        json!({ "type": "noteDoc", "content": [
            json!({"type": "noteTitle", "content": [text("Projection")]}),
            json!({"type": "heading", "attrs": {"level": 2}, "content": [text("Раздел")]}),
            paragraph(&[
                text("Обычный "),
                json!({"type": "text", "text": "жирный", "marks": [json!({"type": "bold"})]}),
                text(", "),
                json!({"type": "text", "text": "курсив", "marks": [json!({"type": "italic"})]}),
                text(", "),
                json!({"type": "text", "text": "код", "marks": [json!({"type": "code"})]}),
                text(", "),
                json!({"type": "text", "text": "зачёркнутый", "marks": [json!({"type": "strike"})]}),
                text("."),
            ]),
            json!({"type": "bulletList", "content": [
                json!({"type": "listItem", "content": [paragraph(&[text("первый")])]}),
                json!({"type": "listItem", "content": [
                    paragraph(&[text("второй")]),
                    json!({"type": "bulletList", "content": [
                        json!({"type": "listItem", "content": [paragraph(&[text("вложенный")])]}),
                    ]}),
                ]}),
            ]}),
            json!({"type": "orderedList", "attrs": {"start": 3}, "content": [
                json!({"type": "listItem", "content": [paragraph(&[text("третий")])]}),
            ]}),
            json!({"type": "taskList", "content": [
                json!({"type": "listItem", "attrs": {"checked": true}, "content": [paragraph(&[text("сделано")])]}),
                json!({"type": "listItem", "attrs": {"checked": false}, "content": [paragraph(&[text("не сделано")])]}),
            ]}),
            json!({"type": "blockquote", "content": [paragraph(&[text("цитата")])]}),
            json!({"type": "hljsCodeBlock", "attrs": {"class": "language-rust"}, "content": [
                json!({"type": "hljsCodeBlockRow", "attrs": {"class": "l"}, "content": [text("let x = 1;")]}),
            ]}),
            json!({"type": "mermaid", "attrs": {"code": "graph TD; A-->B;"}}),
            json!({"type": "horizontalRule"}),
            paragraph(&[
                text("строка 1"),
                json!({"type": "hardBreak"}),
                text("строка 2"),
                json!({"type": "wikiLink", "attrs": {"text": "метка", "path": "стр.md"}}),
            ]),
        ]})
    }

    #[test]
    fn annotated_and_compact_differ_only_by_anchors() {
        let doc = rich_doc();
        let annotated = render(&doc, true);
        let compact = render(&doc, false);
        assert!(annotated.contains("<!-- um:pointer="));
        // the compact page carries not a single comment opener — a stray
        // `-->` can only come from the document itself (mermaid arrows)
        assert!(!compact.contains("<!--"));
        // annotated minus its whole-line anchors reproduces compact exactly
        // (nested anchors carry the list indent / blockquote prefix — trim
        // before matching)
        let stripped: Vec<&str> = annotated
            .lines()
            .filter(|line| !line.trim_start_matches(['>', ' ']).starts_with("<!-- um:"))
            .collect();
        assert_eq!(stripped.join("\n"), compact.trim_end_matches('\n'));
    }

    #[test]
    fn compact_renders_every_supported_shape() {
        let compact = render(&rich_doc(), false);
        assert!(compact.contains("# Projection\n"));
        assert!(compact.contains("## Раздел\n"));
        assert!(compact.contains("Обычный **жирный**, *курсив*, `код`, ~~зачёркнутый~~.\n"));
        assert!(compact.contains("- первый\n"));
        assert!(compact.contains("- второй\n  - вложенный\n"));
        assert!(compact.contains("3. третий\n"));
        assert!(compact.contains("- [x] сделано\n"));
        assert!(compact.contains("- [ ] не сделано\n"));
        assert!(compact.contains("> цитата\n"));
        assert!(compact.contains("```rust\nlet x = 1;\n```\n"));
        assert!(compact.contains("```mermaid\ngraph TD; A-->B;\n```\n"));
        assert!(compact.contains("---\n"));
        assert!(compact.contains("строка 1\nстрока 2[метка](стр.md)"));
    }

    #[test]
    fn table_cells_escape_pipes_and_newlines() {
        let doc = json!({ "type": "noteDoc", "content": [
            json!({"type": "table", "content": [
                json!({"type": "tableRow", "content": [
                    json!({"type": "tableHeader", "attrs": {"colspan": 1, "rowspan": 1}, "content": [paragraph(&[text("A|B")])]}),
                    json!({"type": "tableHeader", "attrs": {"colspan": 1, "rowspan": 1}, "content": [paragraph(&[text("C")])]}),
                ]}),
                json!({"type": "tableRow", "content": [
                    json!({"type": "tableCell", "attrs": {"colspan": 1, "rowspan": 1}, "content": [paragraph(&[text("x|y\nz")])]}),
                    json!({"type": "tableCell", "attrs": {"colspan": 1, "rowspan": 1}, "content": [paragraph(&[text("ok")])]}),
                ]}),
                json!({"type": "tableRow", "content": []}),
            ]}),
        ]});
        let compact = render(&doc, false);
        assert!(compact.contains("| A\\|B | C |\n"));
        assert!(compact.contains("| --- | --- |\n"));
        assert!(compact.contains("| x\\|y<br>z | ok |\n"));
        // an empty row keeps a shape the reader can see
        assert!(compact.contains("|  |\n"));
        assert!(!compact.contains("<!--"));
        // the annotated mode puts the same anchors inside the cells
        let annotated = render(&doc, true);
        assert!(annotated.contains("<!-- um:pointer=\"/content/0/content/0\" -->"));
    }

    #[test]
    fn projection_losses_stay_explicit_in_both_modes() {
        let doc = json!({ "type": "noteDoc", "content": [
            json!({"type": "customWidget", "attrs": {}, "content": [paragraph(&[text("внутри")])]}),
            paragraph(&[
                text("текст "),
                json!({"type": "text", "text": "с пометкой", "marks": [json!({"type": "highlight"})]}),
                json!({"type": "fancySpan", "content": [text("таинственный")]}),
            ]),
        ]});
        let compact = render(&doc, false);
        assert!(compact.contains("[um:unrendered type=\"customWidget\" pointer=\"/content/0\"]"));
        assert!(compact.contains("[um:unrendered-marks=\"highlight\"]"));
        assert!(compact.contains("[um:unrendered-inline type=\"fancySpan\"]"));
        assert!(!compact.contains("<!--"));
        let annotated = render(&doc, true);
        assert!(
            annotated
                .contains("<!-- um:unrendered type=\"customWidget\" pointer=\"/content/0\" -->")
        );
        assert!(annotated.contains("<!-- um:unrendered-marks=\"highlight\" -->"));
        assert!(annotated.contains("<!-- um:unrendered-inline type=\"fancySpan\" -->"));
    }

    #[test]
    fn table_with_marks_and_nested_list_in_cell() {
        let doc = json!({ "type": "noteDoc", "content": [
            json!({"type": "table", "content": [
                json!({"type": "tableRow", "content": [
                    json!({"type": "tableHeader", "attrs": {"colspan": 1, "rowspan": 1}, "content": [paragraph(&[text("Cell")])]}),
                ]}),
                json!({"type": "tableRow", "content": [
                    json!({"type": "tableCell", "attrs": {"colspan": 1, "rowspan": 1}, "content": [
                        paragraph(&[text("с "), json!({"type": "text", "text": "акцентом", "marks": [json!({"type": "strong"})]})]),
                        json!({"type": "bulletList", "content": [
                            json!({"type": "listItem", "content": [paragraph(&[text("пункт")])]}),
                        ]}),
                    ]}),
                ]}),
            ]}),
        ]});
        let compact = render(&doc, false);
        // the paragraph goes through the inline projection; the flattened
        // list keeps the annotated cell shape (base indent + item marker)
        assert!(compact.contains("| с **акцентом** - - пункт |\n"));
        assert!(!compact.contains("<!--"));
    }

    #[test]
    fn comment_injection_in_text_is_neutralized() {
        let doc = json!({ "type": "noteDoc", "content": [
            paragraph(&[text("до <!-- um:pointer=\"/forged\" --> после")]),
        ]});
        let compact = render(&doc, false);
        assert!(!compact.contains("<!--"));
        // the opener is broken; the closer stays as plain text (it cannot
        // form a comment without an intact opener)
        assert!(compact.contains("до <! -- um:pointer=\"/forged\" --> после"));
        let annotated = render(&doc, true);
        assert_eq!(annotated.matches("<! --").count(), 1);
    }

    #[test]
    fn service_marker_shape_switches_with_anchors() {
        let projection = Projection::new(true);
        assert_eq!(
            projection.service_marker("um:incomplete next_cursor=\"t\""),
            "<!-- um:incomplete next_cursor=\"t\" -->"
        );
        let compact = Projection::new(false);
        assert_eq!(
            compact.service_marker("um:incomplete next_cursor=\"t\""),
            "[um:incomplete next_cursor=\"t\"]"
        );
    }
}
