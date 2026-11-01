//! Data model: manifests, note documents, block traversal (spec §4.2, §4.6,
//! and the on-disk dictionary `~/insum/.instruments/um-format.md`).

use serde_json::{Map, Value};

use crate::error::{ErrorCode, UmError};

/// Node types whose content is inline (text-level), per the observed
/// dictionary. Every other node found in a `content` array is a block.
pub const INLINE_NODE_TYPES: [&str; 2] = ["text", "hardBreak"];

/// Reserved attr that holds the block ID once migration/annotation happened
/// (spec §4.4); it is reported separately, not inside `attrs`.
pub const ID_ATTR: &str = "id";

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
    pub fn parse(payload: &Value) -> Result<Self, UmError> {
        if let Some(format) = payload.get("format").and_then(Value::as_str) {
            if format != "inscriptum-note" {
                return Err(note_doc_invalid(format!(
                    "unknown note envelope format {format:?}"
                )));
            }
            let doc = payload
                .get("document")
                .ok_or_else(|| note_doc_invalid("envelope missing `document`"))?;
            validate_doc(doc)?;
            Ok(Self {
                doc: doc.clone(),
                envelope: true,
                features: payload.get("features").cloned(),
            })
        } else {
            validate_doc(payload)?;
            Ok(Self {
                doc: payload.clone(),
                envelope: false,
                features: None,
            })
        }
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
    pub children_truncated: bool,
}

/// Walk the document in document order (preorder), producing the flat block
/// listing. `max_depth` of `Some(d)` stops descending below depth `d`;
/// parents at the cut get `children_truncated`.
pub fn traverse(doc: &Value, max_depth: Option<usize>, max_json_depth: usize) -> Vec<BlockEntry> {
    let mut blocks = Vec::new();
    let content = doc
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (index, node) in content.iter().enumerate() {
        walk(
            node,
            None,
            0,
            index,
            max_depth,
            max_json_depth,
            &mut blocks,
            0,
        );
    }
    blocks
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
) -> bool {
    if json_depth > max_json_depth {
        // The caller turns this into LIMIT_EXCEEDED; stop walking.
        return false;
    }
    let object = match node.as_object() {
        Some(object) => object,
        None => return true,
    };
    let node_type = object.get("type").and_then(Value::as_str).unwrap_or("");
    if INLINE_NODE_TYPES.contains(&node_type) {
        return true; // inline nodes are not blocks
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
    let text = inline_text(object);
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
        children_truncated,
    });
    if !descend {
        return true;
    }
    if let Some(children) = object.get("content").and_then(Value::as_array) {
        for (child_index, child) in children.iter().enumerate() {
            let ok = walk(
                child,
                Some(&pointer),
                depth + 1,
                child_index,
                max_depth,
                max_json_depth,
                blocks,
                json_depth + 1,
            );
            if !ok {
                return false;
            }
        }
    }
    true
}

/// Direct inline text of a node, if its content consists of inline nodes.
/// Parent containers never aggregate descendant text (spec §6.2).
fn inline_text(object: &Map<String, Value>) -> Option<String> {
    let children = object.get("content")?.as_array()?;
    if children.is_empty() {
        return None;
    }
    let mut text = String::new();
    for child in children {
        let child_type = child.get("type").and_then(Value::as_str)?;
        match child_type {
            "text" => text.push_str(child.get("text").and_then(Value::as_str)?),
            "hardBreak" => text.push('\n'),
            _ => return None, // a block child: this node has no own text
        }
    }
    Some(text)
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
/// heading's text, else null.
pub fn note_title(manifest_title: Option<&str>, doc: &Value) -> Option<String> {
    if let Some(title) = manifest_title {
        return Some(title.to_owned());
    }
    doc.get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.iter().next())
        .filter(|first| first.get("type").and_then(Value::as_str) == Some("heading"))
        .and_then(|first| {
            first
                .get("content")
                .and_then(Value::as_array)
                .map(|inline| inline_text_join(inline))
        })
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
        let blocks = traverse(&sample_doc(), None, 128);
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
        let blocks = traverse(&sample_doc(), None, 128);
        // bulletList and listItem have no aggregated text
        assert!(blocks[1].text.is_none());
        assert!(blocks[2].text.is_none());
        // paragraph keeps its full inline text including marked parts
        assert_eq!(blocks[4].text.as_deref(), Some("Hello world"));
    }

    #[test]
    fn id_is_lifted_and_removed_from_attrs() {
        let blocks = traverse(&sample_doc(), None, 128);
        assert_eq!(blocks[4].id.as_deref(), Some("a1b2c3d4e5"));
        assert_eq!(blocks[4].attrs, json!({}));
        assert_eq!(blocks[0].attrs, json!({"level": 2}));
    }

    #[test]
    fn depth_limit_marks_truncation() {
        // --depth 1: only the top level is emitted; block parents at the cut
        // are marked children_truncated.
        let blocks = traverse(&sample_doc(), Some(1), 128);
        assert_eq!(blocks.len(), 3);
        assert!(blocks.iter().all(|b| b.depth == 0));
        let list = blocks.iter().find(|b| b.pointer == "/content/1").unwrap();
        assert!(list.children_truncated);
        let heading = blocks.iter().find(|b| b.pointer == "/content/0").unwrap();
        // inline-only parents have no children to truncate
        assert!(!heading.children_truncated);
        // --depth 2 shows two levels
        let blocks2 = traverse(&sample_doc(), Some(2), 128);
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
