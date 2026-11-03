//! Data model: manifests, note documents, block traversal (spec §4.2, §4.6,
//! and the node types observed in real on-disk Inscriptum documents).

use serde_json::{Map, Value};

use crate::error::{ErrorCode, UmError};

/// Node types whose content is inline (text-level), per the observed
/// dictionary. Every other node found in a `content` array is a block.
/// `wikiLink` is an inline ATOM: no content, the displayed label lives in
/// `attrs.text` (um-format dictionary, plugin 0.13).
pub const INLINE_NODE_TYPES: [&str; 3] = ["text", "hardBreak", "wikiLink"];

/// Reserved attr that holds the block ID once migration/annotation happened
/// (spec §4.4); it is reported separately, not inside `attrs`.
pub const ID_ATTR: &str = "id";

/// The node dictionary observed in real on-disk Inscriptum documents.
/// Types outside it are preserved but reported as unknown (warnings) — and
/// their text cannot be extracted (§4.7).
pub const KNOWN_NODE_TYPES: [&str; 28] = [
    "noteDoc",
    "doc",
    "noteTitle",
    "noteSummary",
    "noteProperty",
    "notePropertyName",
    "notePropertyValue",
    "heading",
    "paragraph",
    "text",
    "hardBreak",
    "wikiLink",
    "bulletList",
    "orderedList",
    "taskList",
    "listItem",
    "taskItem",
    "blockquote",
    "mermaid",
    "attachment",
    "horizontalRule",
    "hljsCodeBlock",
    "hljsCodeBlockRow",
    "image",
    "table",
    "tableRow",
    "tableCell",
    "tableHeader",
];

/// A parsed manifest: known fields extracted, the raw value kept for
/// unknown-field preservation (spec §3.10, §12).
#[derive(Debug, Clone)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub container_type: String,
    pub notes: Vec<NoteDescriptor>,
    /// Top-level manifest features, if the manifest carries them.
    pub features: Option<Value>,
    pub raw: Value,
}

/// One note entry of the manifest; unknown fields stay in `raw`.
#[derive(Debug, Clone)]
pub struct NoteDescriptor {
    pub id: String,
    pub path: String,
    pub order: u32,
    pub title: Option<String>,
    pub schema: Option<String>,
    pub features: Option<Value>,
    pub raw: Value,
}

impl Manifest {
    pub fn parse(value: Value) -> Result<Self, UmError> {
        let object = value
            .as_object()
            .ok_or_else(|| invalid_manifest("manifest is not a JSON object"))?;
        let format = object
            .get("format")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_manifest("missing string field `format`"))?
            .to_owned();
        if format != "um" {
            return Err(UmError::new(
                ErrorCode::UnsupportedType,
                format!("unsupported container format {format:?}"),
            ));
        }
        let version = object
            .get("version")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid_manifest("missing integer field `version`"))?;
        if version != 1 {
            return Err(UmError::new(
                ErrorCode::UnsupportedVersion,
                format!("unsupported UM container version {version}"),
            )
            .with_hint("This CLI supports UM Document Container Format 1.0."));
        }
        // On-disk reality writes `type: "notepad"`; the spec text says
        // `"notebook"`. Accept both — reality wins (governance rule), the
        // divergence is recorded in the issues journal.
        let container_type = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_manifest("missing string field `type`"))?
            .to_owned();
        if container_type != "notepad" && container_type != "notebook" {
            return Err(UmError::new(
                ErrorCode::UnsupportedType,
                format!("unsupported container type {container_type:?}"),
            ));
        }
        let notes_value = object
            .get("notes")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_manifest("missing array field `notes`"))?;
        let mut notes = Vec::with_capacity(notes_value.len());
        for note_value in notes_value {
            notes.push(NoteDescriptor::parse(note_value)?);
        }
        Ok(Self {
            format,
            version: version as u32,
            container_type,
            notes,
            features: object.get("features").cloned(),
            raw: value,
        })
    }
}

impl NoteDescriptor {
    pub fn parse(value: &Value) -> Result<Self, UmError> {
        let object = value
            .as_object()
            .ok_or_else(|| note_descriptor_invalid("note descriptor is not an object"))?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| note_descriptor_invalid("note descriptor missing string `id`"))?
            .to_owned();
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| note_descriptor_invalid("note descriptor missing string `path`"))?
            .to_owned();
        let order = object
            .get("order")
            .and_then(Value::as_u64)
            .ok_or_else(|| note_descriptor_invalid("note descriptor missing integer `order`"))?;
        if order > u32::MAX as u64 {
            return Err(note_descriptor_invalid(format!(
                "note descriptor order {order} exceeds u32"
            )));
        }
        if path != format!("notes/{id}.json") && !path.ends_with(".json") {
            return Err(note_descriptor_invalid(format!(
                "note descriptor path {path:?} is not a JSON payload path"
            )));
        }
        Ok(Self {
            id,
            path,
            order: order as u32,
            title: object
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned),
            schema: object
                .get("schema")
                .and_then(Value::as_str)
                .map(str::to_owned),
            features: object.get("features").cloned(),
            raw: value.clone(),
        })
    }

    /// The descriptor as an output value (unknown fields preserved).
    pub fn to_output(&self) -> Value {
        self.raw.clone()
    }

    /// `features.block_ids == 1` marks documents whose IDs are guaranteed
    /// (spec §4.6); only `insum migrate-ids` writes the marker.
    pub fn block_ids_enabled(&self) -> bool {
        self.features
            .as_ref()
            .and_then(|f| f.get("block_ids"))
            .and_then(Value::as_u64)
            == Some(1)
    }
}

/// A parsed note payload: legacy raw `noteDoc` or a document inside an
/// `inscriptum-note` envelope (spec §4.6).
#[derive(Debug, Clone)]
pub struct NoteDoc {
    /// The ProseMirror-like document (`type: "noteDoc"`).
    pub doc: Value,
    /// True when the payload came wrapped in an `inscriptum-note` envelope.
    pub envelope: bool,
    /// Envelope-level `features`, if present.
    pub features: Option<Value>,
}

impl NoteDoc {
    /// Consume the parsed payload (ownership — no deep copies on the read
    /// path, review P-03). Envelope version is checked (review K-11).
    pub fn parse(payload: Value) -> Result<Self, UmError> {
        let envelope_format = payload
            .get("format")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(format) = envelope_format else {
            validate_doc(&payload)?;
            return Ok(Self {
                doc: payload,
                envelope: false,
                features: None,
            });
        };
        if format != "inscriptum-note" {
            return Err(note_doc_invalid(format!(
                "unknown note envelope format {format:?}"
            )));
        }
        match payload.get("version").and_then(Value::as_u64) {
            Some(1) => {}
            other => {
                return Err(UmError::new(
                    ErrorCode::UnsupportedVersion,
                    format!("unsupported inscriptum-note envelope version {other:?} (expected 1)"),
                )
                .with_hint("Re-save the note with a current build, or migrate the envelope."));
            }
        }
        let mut map = match payload {
            Value::Object(map) => map,
            _ => return Err(note_doc_invalid("envelope is not a JSON object")),
        };
        let doc = map
            .remove("document")
            .ok_or_else(|| note_doc_invalid("envelope missing `document`"))?;
        validate_doc(&doc)?;
        Ok(Self {
            doc,
            envelope: true,
            features: map.get("features").cloned(),
        })
    }
}

fn validate_doc(doc: &Value) -> Result<(), UmError> {
    let object = doc
        .as_object()
        .ok_or_else(|| note_doc_invalid("document is not a JSON object"))?;
    let doc_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| note_doc_invalid("document missing string `type`"))?;
    if doc_type != "noteDoc" && doc_type != "doc" {
        return Err(note_doc_invalid(format!(
            "unexpected root node type {doc_type:?} (expected noteDoc/doc)"
        )));
    }
    if !object.contains_key("content") {
        return Err(note_doc_invalid("document missing `content`"));
    }
    Ok(())
}

/// One block in the flat preorder listing (`tree`, search, §6.2).
#[derive(Debug, Clone)]
pub struct BlockEntry {
    pub pointer: String,
    pub id: Option<String>,
    pub node_type: String,
    pub parent_pointer: Option<String>,
    pub depth: usize,
    pub index: usize,
    pub attrs: Value,
    /// Text of nodes whose direct content is inline; never aggregates
    /// children (spec §6.2/§6.4).
    pub text: Option<String>,
    /// True when `text` was dropped because the content contains an inline
    /// node type outside the known dictionary (spec §4.7).
    pub text_incomplete: bool,
    pub children_truncated: bool,
}

/// The flat block listing of one document, plus the completeness signals.
#[derive(Debug, Default)]
pub struct Traversal {
    pub blocks: Vec<BlockEntry>,
    /// True when the document is deeper than the enforced JSON depth limit
    /// (UM_MAX_DEPTH) — callers must fail with `LIMIT_EXCEEDED` (spec §5.5).
    pub depth_exceeded: bool,
    /// True when some block lost its text because of an unknown inline node
    /// type — search/tree consumers must not claim full completeness
    /// (spec §4.7, §3.5 «никаких тихих потерь»).
    pub unknown_inline: bool,
}

/// Walk the document in document order (preorder), producing the flat block
/// listing. `max_depth` of `Some(d)` stops descending below depth `d`;
/// parents at the cut get `children_truncated`. `max_json_depth` is the
/// enforced UM_MAX_DEPTH: walking stops (with `depth_exceeded`) beyond it.
pub fn traverse(doc: &Value, max_depth: Option<usize>, max_json_depth: usize) -> Traversal {
    let mut traversal = Traversal::default();
    let Some(content) = doc.get("content").and_then(Value::as_array) else {
        return traversal;
    };
    for (index, node) in content.iter().enumerate() {
        walk(
            node,
            None,
            0,
            index,
            max_depth,
            max_json_depth,
            &mut traversal.blocks,
            0,
            &mut traversal.depth_exceeded,
            &mut traversal.unknown_inline,
        );
    }
    traversal
}

#[allow(clippy::too_many_arguments)]
fn walk(
    node: &Value,
    parent_pointer: Option<&str>,
    depth: usize,
    index: usize,
    max_depth: Option<usize>,
    max_json_depth: usize,
    blocks: &mut Vec<BlockEntry>,
    json_depth: usize,
    depth_exceeded: &mut bool,
    unknown_inline: &mut bool,
) {
    if json_depth > max_json_depth {
        *depth_exceeded = true;
        return;
    }
    let object = match node.as_object() {
        Some(object) => object,
        None => return,
    };
    let node_type = object.get("type").and_then(Value::as_str).unwrap_or("");
    if INLINE_NODE_TYPES.contains(&node_type) {
        return; // inline nodes are not blocks
    }

    let pointer = match parent_pointer {
        None => format!("/content/{index}"),
        Some(parent) => format!("{parent}/content/{index}"),
    };
    let id = object
        .get("attrs")
        .and_then(|attrs| attrs.get(ID_ATTR))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let attrs = match object.get("attrs") {
        Some(Value::Object(map)) => {
            let mut copy = map.clone();
            copy.remove(ID_ATTR);
            Value::Object(copy)
        }
        _ => Value::Object(Map::new()),
    };
    let (text, text_incomplete) = inline_text(object);
    if text_incomplete {
        *unknown_inline = true;
    }
    let mut children_truncated = false;

    // `--depth n` shows levels 0..n-1; a parent at the cut is emitted and
    // marked `children_truncated` instead of its descendants (spec §6.2).
    let descend = match max_depth {
        None => true,
        Some(max) => depth + 1 < max,
    };
    if !descend {
        children_truncated = has_block_children(object);
    }

    // Preorder: the block itself precedes its children in the listing.
    blocks.push(BlockEntry {
        pointer: pointer.clone(),
        id,
        node_type: node_type.to_owned(),
        parent_pointer: parent_pointer.map(str::to_owned),
        depth,
        index,
        attrs,
        text,
        text_incomplete,
        children_truncated,
    });
    if !descend {
        return;
    }
    if let Some(children) = object.get("content").and_then(Value::as_array) {
        for (child_index, child) in children.iter().enumerate() {
            walk(
                child,
                Some(&pointer),
                depth + 1,
                child_index,
                max_depth,
                max_json_depth,
                blocks,
                json_depth + 1,
                depth_exceeded,
                unknown_inline,
            );
        }
    }
}

/// Direct inline text of a node, if its content consists of inline nodes.
/// Parent containers never aggregate descendant text (spec §6.2). The flag
/// reports unknown inline node types: their text is unknowable without a
/// schema artifact, so partial text is withheld instead of silently
/// returned (spec §4.7, §3.5).
fn inline_text(object: &Map<String, Value>) -> (Option<String>, bool) {
    let Some(children) = object.get("content").and_then(Value::as_array) else {
        return (None, false);
    };
    if children.is_empty() {
        return (None, false);
    }
    let mut text = String::new();
    for child in children {
        let Some(child_type) = child.get("type").and_then(Value::as_str) else {
            return (None, true);
        };
        match child_type {
            "text" => match child.get("text").and_then(Value::as_str) {
                Some(t) => text.push_str(t),
                None => return (None, true),
            },
            "hardBreak" => text.push('\n'),
            // the wiki link displays as its label: the label IS the visible
            // text of the paragraph (um-format dictionary, plugin 0.13)
            "wikiLink" => match child
                .get("attrs")
                .and_then(|attrs| attrs.get("text"))
                .and_then(Value::as_str)
            {
                Some(label) => text.push_str(label),
                // no label: the display text is unknown — withhold
                None => return (None, true),
            },
            // a known structural child: this node is a container and has no
            // text of its own (normal, not a loss)
            t if KNOWN_NODE_TYPES.contains(&t) => return (None, false),
            // unknown node type: its text is unknowable without a schema
            // artifact — withhold instead of silently dropping (§4.7, §3.5)
            _ => return (None, true),
        }
    }
    (Some(text), false)
}

fn has_block_children(object: &Map<String, Value>) -> bool {
    object
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|children| {
            children.iter().any(|child| {
                let child_type = child.get("type").and_then(Value::as_str).unwrap_or("");
                !INLINE_NODE_TYPES.contains(&child_type)
            })
        })
}

/// Title of a note per spec §6.4: manifest title, else the first top-level
/// heading's text, else null. `noteTitle` counts as a heading-equivalent —
/// it is the semantic title node of title-schema pages.
pub fn note_title(manifest_title: Option<&str>, doc: &Value) -> Option<String> {
    if let Some(title) = manifest_title {
        return Some(title.to_owned());
    }
    let content = doc.get("content").and_then(Value::as_array)?;
    content
        .iter()
        .find(|block| {
            matches!(
                block.get("type").and_then(Value::as_str),
                Some("heading") | Some("noteTitle")
            )
        })
        .and_then(|first| {
            first
                .get("content")
                .and_then(Value::as_array)
                .map(|inline| inline_text_join(inline))
        })
        .filter(|title| !title.is_empty())
}

/// Concatenate inline text without node-type discrimination (used for the
/// title fallback over a heading's inline content).
fn inline_text_join(inline: &[Value]) -> String {
    inline
        .iter()
        .filter_map(|node| node.get("text").and_then(Value::as_str))
        .collect()
}

fn invalid_manifest(message: impl Into<String>) -> UmError {
    UmError::new(ErrorCode::ManifestInvalid, message.into())
}

fn note_descriptor_invalid(message: impl Into<String>) -> UmError {
    UmError::new(ErrorCode::NoteDescriptorInvalid, message.into())
}

fn note_doc_invalid(message: impl Into<String>) -> UmError {
    UmError::new(ErrorCode::NoteDocInvalid, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_doc() -> Value {
        json!({
            "type": "noteDoc",
            "content": [
                {"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "Title"}]},
                {"type": "bulletList", "content": [
                    {"type": "listItem", "content": [
                        {"type": "paragraph", "content": [{"type": "text", "text": "Item"}]}
                    ]}
                ]},
                {"type": "paragraph", "attrs": {"id": "a1b2c3d4e5"}, "content": [
                    {"type": "text", "text": "Hello "},
                    {"type": "text", "marks": [{"type": "strong"}], "text": "world"}
                ]}
            ]
        })
    }

    #[test]
    fn traversal_is_flat_preorder_with_pointers() {
        let blocks = traverse(&sample_doc(), None, 128).blocks;
        let pointers: Vec<&str> = blocks.iter().map(|b| b.pointer.as_str()).collect();
        assert_eq!(
            pointers,
            [
                "/content/0",
                "/content/1",
                "/content/1/content/0",
                "/content/1/content/0/content/0",
                "/content/2"
            ]
        );
        let heading = &blocks[0];
        assert_eq!(heading.node_type, "heading");
        assert_eq!(heading.text.as_deref(), Some("Title"));
        assert_eq!(heading.parent_pointer, None);
        let item_paragraph = &blocks[3];
        assert_eq!(
            item_paragraph.parent_pointer.as_deref(),
            Some("/content/1/content/0")
        );
        assert_eq!(item_paragraph.depth, 2);
        assert_eq!(item_paragraph.text.as_deref(), Some("Item"));
    }

    #[test]
    fn text_does_not_duplicate_on_ancestors() {
        let blocks = traverse(&sample_doc(), None, 128).blocks;
        // bulletList and listItem have no aggregated text
        assert!(blocks[1].text.is_none());
        assert!(blocks[2].text.is_none());
        // paragraph keeps its full inline text including marked parts
        assert_eq!(blocks[4].text.as_deref(), Some("Hello world"));
    }

    #[test]
    fn wikilink_label_is_visible_text() {
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "paragraph", "content": [
                    {"type": "text", "text": "см. "},
                    {"type": "wikiLink", "attrs": {"path": "notes/plan.um", "text": "план"}},
                    {"type": "text", "text": " и дальше"}
                ]}
            ]
        });
        let traversal = traverse(&doc, None, 128);
        assert!(!traversal.unknown_inline, "wikiLink is a known inline atom");
        let paragraph = &traversal.blocks[0];
        assert_eq!(paragraph.text.as_deref(), Some("см. план и дальше"));
        // a wikiLink without a label withholds the paragraph text
        let broken = json!({
            "type": "noteDoc",
            "content": [
                {"type": "paragraph", "content": [
                    {"type": "text", "text": "see "},
                    {"type": "wikiLink", "attrs": {"path": "x"}}
                ]}
            ]
        });
        let t2 = traverse(&broken, None, 128);
        assert!(t2.unknown_inline);
        assert_eq!(t2.blocks[0].text, None);
    }

    #[test]
    fn id_is_lifted_and_removed_from_attrs() {
        let blocks = traverse(&sample_doc(), None, 128).blocks;
        assert_eq!(blocks[4].id.as_deref(), Some("a1b2c3d4e5"));
        assert_eq!(blocks[4].attrs, json!({}));
        assert_eq!(blocks[0].attrs, json!({"level": 2}));
    }

    #[test]
    fn depth_limit_marks_truncation() {
        // --depth 1: only the top level is emitted; block parents at the cut
        // are marked children_truncated.
        let blocks = traverse(&sample_doc(), Some(1), 128).blocks;
        assert_eq!(blocks.len(), 3);
        assert!(blocks.iter().all(|b| b.depth == 0));
        let list = blocks.iter().find(|b| b.pointer == "/content/1").unwrap();
        assert!(list.children_truncated);
        let heading = blocks.iter().find(|b| b.pointer == "/content/0").unwrap();
        // inline-only parents have no children to truncate
        assert!(!heading.children_truncated);
        // --depth 2 shows two levels
        let blocks2 = traverse(&sample_doc(), Some(2), 128).blocks;
        assert!(blocks2.iter().any(|b| b.depth == 1));
        let item = blocks2
            .iter()
            .find(|b| b.pointer == "/content/1/content/0")
            .unwrap();
        assert!(item.children_truncated);
    }

    #[test]
    fn title_rule_prefers_manifest_then_heading() {
        assert_eq!(
            note_title(Some("Manifest"), &sample_doc()).as_deref(),
            Some("Manifest")
        );
        assert_eq!(note_title(None, &sample_doc()).as_deref(), Some("Title"));
        let bare = json!({"type": "noteDoc", "content": [
            {"type": "paragraph", "content": [{"type": "text", "text": "No heading"}]}
        ]});
        assert_eq!(note_title(None, &bare), None);
    }
}
