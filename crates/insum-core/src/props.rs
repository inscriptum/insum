//! Read-only extraction of note properties (`noteProperty` rows).
//!
//! A property row is a `noteProperty` block whose content is
//! `notePropertyName notePropertyValue*` (title-family schema v3+, plugin
//! 0.11.0). Rows are collected from the WHOLE document in document order
//! (preorder): the title family keeps them top-level right after
//! `noteTitle`, but nothing in the node itself requires that position, and
//! a read command must not silently skip a row it did not expect to find.
//!
//! Values are read as-is (read-path constraint): the text of a name/value
//! node is the concatenation of its direct inline content — the unified
//! [`crate::model::inline_text`] discipline, marks never alter the text, no
//! type interpolation (`attrs.type` "text"/"list" is metadata, not a
//! coercion). A name/value whose inline content contains an unknown node
//! type has its text WITHHELD (`text: None`) instead of returning partial
//! text — no silent losses.

use serde_json::Value;

use crate::model::{ID_ATTR, inline_text};

/// The name node of one property row (`notePropertyName`).
#[derive(Debug, Clone)]
pub struct PropertyName {
    pub pointer: String,
    pub id: Option<String>,
    /// `None` = withheld: the inline content contains an unknown node type,
    /// so the text is unknowable without a schema artifact.
    pub text: Option<String>,
}

/// One value node of a property row (`notePropertyValue`).
#[derive(Debug, Clone)]
pub struct PropertyValue {
    pub pointer: String,
    pub id: Option<String>,
    /// `None` = withheld (unknown inline node type in the content).
    pub text: Option<String>,
}

/// One `noteProperty` row.
#[derive(Debug, Clone)]
pub struct PropertyRow {
    pub pointer: String,
    pub id: Option<String>,
    /// The first `notePropertyName` child; `None` when the row has none
    /// (malformed row — reported via `shape_warnings`).
    pub name: Option<PropertyName>,
    /// Every `notePropertyValue` child in document order (the content rule
    /// allows zero or more).
    pub values: Vec<PropertyValue>,
    /// Diagnostics about children that are not part of the
    /// `notePropertyName notePropertyValue*` shape (extra name nodes,
    /// unexpected node types) — verdict-neutral warnings, never silent
    /// drops.
    pub shape_warnings: Vec<String>,
}

/// The property rows of one document plus the completeness signals.
#[derive(Debug, Default)]
pub struct NoteProperties {
    pub rows: Vec<PropertyRow>,
    /// True when some name/value text was withheld because of an unknown
    /// inline node type (props/ls consumers must not claim completeness).
    pub unknown_inline: bool,
    /// True when the document is deeper than the enforced JSON depth limit
    /// (UM_MAX_DEPTH) — callers must fail with `LIMIT_EXCEEDED`.
    pub depth_exceeded: bool,
}

/// Collect the `noteProperty` rows of a document (preorder, document
/// order), with JSON Pointers in the same `parent/content/index` discipline
/// as the block traversal.
pub fn collect(doc: &Value, max_json_depth: usize) -> NoteProperties {
    let mut out = NoteProperties::default();
    if json_depth_exceeded(doc, 0, max_json_depth) {
        out.depth_exceeded = true;
        return out;
    }
    if let Some(children) = doc.get("content").and_then(Value::as_array) {
        for (index, child) in children.iter().enumerate() {
            walk(child, None, index, 1, max_json_depth, &mut out);
        }
    }
    out
}

fn walk(
    node: &Value,
    parent_pointer: Option<&str>,
    index: usize,
    json_depth: usize,
    max_json_depth: usize,
    out: &mut NoteProperties,
) {
    if json_depth > max_json_depth {
        out.depth_exceeded = true;
        return;
    }
    let Some(object) = node.as_object() else {
        return;
    };
    let node_type = object.get("type").and_then(Value::as_str).unwrap_or("");
    let pointer = match parent_pointer {
        None => format!("/content/{index}"),
        Some(parent) => format!("{parent}/content/{index}"),
    };
    if node_type == "noteProperty" {
        let row = parse_row(object, &pointer, json_depth + 1, max_json_depth, out);
        out.rows.push(row);
        return; // name/value are inline content — no rows inside a row
    }
    if let Some(children) = object.get("content").and_then(Value::as_array) {
        for (child_index, child) in children.iter().enumerate() {
            walk(
                child,
                Some(&pointer),
                child_index,
                json_depth + 1,
                max_json_depth,
                out,
            );
        }
    }
}

fn parse_row(
    object: &serde_json::Map<String, Value>,
    pointer: &str,
    child_json_depth: usize,
    max_json_depth: usize,
    out: &mut NoteProperties,
) -> PropertyRow {
    let mut row = PropertyRow {
        pointer: pointer.to_owned(),
        id: node_id(object),
        name: None,
        values: Vec::new(),
        shape_warnings: Vec::new(),
    };
    if let Some(children) = object.get("content").and_then(Value::as_array) {
        for (index, child) in children.iter().enumerate() {
            if child_json_depth > max_json_depth {
                out.depth_exceeded = true;
                return row;
            }
            let child_type = child
                .as_object()
                .and_then(|child| child.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let child_pointer = format!("{pointer}/content/{index}");
            match child_type {
                "notePropertyName" => {
                    if row.name.is_some() {
                        // the content rule allows exactly one name node;
                        // extras stay in the document but are not
                        // represented in the projection
                        row.shape_warnings.push(format!(
                            "extra notePropertyName child at {child_pointer} (the content rule allows exactly one)"
                        ));
                    } else {
                        row.name = Some(PropertyName {
                            pointer: child_pointer,
                            id: node_id_of(child),
                            text: inline_text_of(child, out),
                        });
                    }
                }
                "notePropertyValue" => row.values.push(PropertyValue {
                    pointer: child_pointer,
                    id: node_id_of(child),
                    text: inline_text_of(child, out),
                }),
                other => row.shape_warnings.push(format!(
                    "unexpected child node type {other:?} at {child_pointer} (not part of the property pair shape)"
                )),
            }
        }
    }
    row
}

/// The unified inline-text discipline applied to a name/value node: direct
/// inline content only, marks ignored. Empty content is a real state (the
/// plugin writes empty values) and reads as an empty string; an unknown
/// inline node type withholds the text (`None`) — never partial text.
fn inline_text_of(node: &Value, out: &mut NoteProperties) -> Option<String> {
    match node.as_object().map(inline_text) {
        Some((Some(text), false)) => Some(text),
        Some((None, false)) => Some(String::new()), // empty inline content, as-is
        Some((_, true)) => {
            out.unknown_inline = true;
            None
        }
        None => {
            // not an object (malformed node) — nothing readable
            out.unknown_inline = true;
            None
        }
    }
}

fn node_id(object: &serde_json::Map<String, Value>) -> Option<String> {
    object
        .get("attrs")
        .and_then(|attrs| attrs.get(ID_ATTR))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn node_id_of(node: &Value) -> Option<String> {
    node.as_object().and_then(node_id)
}

/// A cheap structural pre-check mirroring the depth accounting of the walk:
/// the root itself sits at depth 0, its children at depth 1.
fn json_depth_exceeded(node: &Value, depth: usize, max_json_depth: usize) -> bool {
    if depth > max_json_depth {
        return true;
    }
    match node.get("content").and_then(Value::as_array) {
        Some(children) => children
            .iter()
            .any(|child| json_depth_exceeded(child, depth + 1, max_json_depth)),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_are_collected_in_document_order_with_ids() {
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "noteTitle", "content": [{"type": "text", "text": "issues"}]},
                {"type": "noteProperty", "attrs": {"id": "row1", "type": "text"}, "content": [
                    {"type": "notePropertyName", "attrs": {"id": "name1"}, "content": [{"type": "text", "text": "description"}]},
                    {"type": "notePropertyValue", "attrs": {"id": "val1"}, "content": [{"type": "text", "text": "Journal"}]}
                ]},
                {"type": "heading", "content": [{"type": "text", "text": "H"}]},
                {"type": "noteProperty", "attrs": {"id": "row2", "type": "list"}, "content": [
                    {"type": "notePropertyName", "attrs": {"id": "name2"}, "content": [{"type": "text", "text": "state"}]},
                    {"type": "notePropertyValue", "attrs": {"id": "val2a"}, "content": [{"type": "text", "text": "todo"}]},
                    {"type": "notePropertyValue", "attrs": {"id": "val2b"}, "content": [{"type": "text", "text": "done"}]}
                ]}
            ]
        });
        let collected = collect(&doc, 128);
        assert_eq!(collected.rows.len(), 2);
        assert!(!collected.unknown_inline);
        let first = &collected.rows[0];
        assert_eq!(first.pointer, "/content/1");
        assert_eq!(first.id.as_deref(), Some("row1"));
        let name = first.name.as_ref().expect("name present");
        assert_eq!(name.pointer, "/content/1/content/0");
        assert_eq!(name.id.as_deref(), Some("name1"));
        assert_eq!(name.text.as_deref(), Some("description"));
        assert_eq!(first.values[0].id.as_deref(), Some("val1"));
        assert_eq!(first.values[0].text.as_deref(), Some("Journal"));
        // a list-type row: one name, several values, each its own node
        let second = &collected.rows[1];
        assert_eq!(second.values.len(), 2);
        assert_eq!(second.values[1].text.as_deref(), Some("done"));
    }

    #[test]
    fn marks_do_not_alter_the_text_and_empty_values_read_as_empty() {
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "noteProperty", "content": [
                    {"type": "notePropertyName", "content": [
                        {"type": "text", "text": "de"},
                        {"type": "text", "marks": [{"type": "strong"}], "text": "scription"}
                    ]},
                    {"type": "notePropertyValue", "content": []}
                ]}
            ]
        });
        let collected = collect(&doc, 128);
        let row = &collected.rows[0];
        assert_eq!(
            row.name
                .as_ref()
                .and_then(|name| name.text.clone())
                .as_deref(),
            Some("description")
        );
        assert_eq!(row.values[0].text.as_deref(), Some(""));
    }

    #[test]
    fn unknown_inline_node_withholds_the_text() {
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "noteProperty", "content": [
                    {"type": "notePropertyName", "content": [{"type": "text", "text": "x"}]},
                    {"type": "notePropertyValue", "content": [
                        {"type": "text", "text": "before "},
                        {"type": "futureWidget"}
                    ]}
                ]}
            ]
        });
        let collected = collect(&doc, 128);
        assert!(collected.unknown_inline);
        // withheld entirely — never partial text
        assert_eq!(collected.rows[0].values[0].text, None);
    }

    #[test]
    fn nested_row_and_shape_warnings() {
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "bulletList", "content": [
                    {"type": "listItem", "content": [
                        {"type": "paragraph"},
                        {"type": "noteProperty", "attrs": {"id": "deep"}, "content": [
                            {"type": "notePropertyName", "content": [{"type": "text", "text": "side"}]},
                            {"type": "notePropertyName", "content": [{"type": "text", "text": "extra"}]},
                            {"type": "paragraph", "content": [{"type": "text", "text": "stray"}]}
                        ]}
                    ]}
                ]}
            ]
        });
        let collected = collect(&doc, 128);
        assert_eq!(
            collected.rows.len(),
            1,
            "rows are found anywhere in the document"
        );
        assert_eq!(collected.rows[0].pointer, "/content/0/content/0/content/1");
        assert_eq!(collected.rows[0].shape_warnings.len(), 2);
        assert!(collected.rows[0].shape_warnings[0].contains("extra notePropertyName"));
        assert!(collected.rows[0].shape_warnings[1].contains("unexpected child node type"));
    }

    #[test]
    fn zero_value_row_reads_as_is() {
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "noteProperty", "content": [
                    {"type": "notePropertyName", "content": [{"type": "text", "text": "empty"}]}
                ]}
            ]
        });
        let collected = collect(&doc, 128);
        let row = &collected.rows[0];
        assert!(row.name.as_ref().expect("name").text.as_deref() == Some("empty"));
        assert!(row.values.is_empty(), "the content rule allows zero values");
    }

    #[test]
    fn depth_exceeded_is_reported() {
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "noteProperty", "content": [
                    {"type": "notePropertyName", "content": [{"type": "text", "text": "x"}]}
                ]}
            ]
        });
        let collected = collect(&doc, 2);
        assert!(collected.depth_exceeded);
    }
}
