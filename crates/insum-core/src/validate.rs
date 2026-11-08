//! Note validation — structural checks and coded diagnostics.
//!
//! Stage A implements Container and Structural JSON checks plus ID checks
//! for documents marked `features.block_ids`. With an applicable schema
//! artifact (§6.6) the same walk also performs inventory checks against
//! the artifact: node types, attribute keys, mark types and the document
//! root. Full validation stays out of reach until the differential corpus
//! proves parity (§4.8) — the CLI never reports a level it does not run.

use serde::Serialize;
use serde_json::Value;

use crate::UmError;
use crate::error::ErrorCode;
use crate::limits::Limits;
use crate::model::{INLINE_NODE_TYPES, KNOWN_NODE_TYPES};
use crate::schema::SchemaArtifact;

/// A single diagnostic: `code` is a stable registry name, `message` is
/// human-facing. Severity follows: a rule is either an
/// error or a warning; `--strict` promotes warnings.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Diag {
    pub code: &'static str,
    pub message: String,
}

/// The validation result of one note.
#[derive(Debug, Default)]
pub struct NoteValidation {
    pub errors: Vec<Diag>,
    pub warnings: Vec<Diag>,
}

impl NoteValidation {
    pub fn valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Validate one note document: structure,
/// unknown node types (warning), and ID rules for documents marked
/// `features.block_ids`.
pub fn validate_note_doc(
    doc: &Value,
    block_ids_enabled: bool,
    limits: &Limits,
) -> Result<NoteValidation, UmError> {
    validate_note_doc_with_schema(doc, block_ids_enabled, limits, None)
}

/// The same walk with an applicable schema artifact: when `schema` is
/// `Some`, inventory checks run against it and unknown node types become
/// errors instead of warnings (§6.6 "schema" level).
pub fn validate_note_doc_with_schema(
    doc: &Value,
    block_ids_enabled: bool,
    limits: &Limits,
    schema: Option<&SchemaArtifact>,
) -> Result<NoteValidation, UmError> {
    let mut result = NoteValidation::default();
    walk_structure(doc, 0, limits, &mut result, schema, true)?;
    if block_ids_enabled {
        validate_block_ids(doc, limits, &mut result);
    }
    Ok(result)
}

fn walk_structure(
    node: &Value,
    depth: usize,
    limits: &Limits,
    result: &mut NoteValidation,
    schema: Option<&SchemaArtifact>,
    is_root: bool,
) -> Result<(), UmError> {
    if depth > limits.max_depth {
        return Err(UmError::new(
            ErrorCode::LimitExceeded,
            format!("document deeper than UM_MAX_DEPTH ({})", limits.max_depth),
        ));
    }
    let Value::Object(map) = node else {
        result.errors.push(Diag {
            code: "NOTE_DOC_INVALID",
            message: "node is not a JSON object".to_owned(),
        });
        return Ok(());
    };

    match map.get("type") {
        Some(Value::String(node_type)) => {
            match schema {
                // With an applicable artifact the artifact is the authority
                // for what the current editor accepts. A type it does not
                // declare splits two ways: a legacy type observed in real
                // on-disk documents (document version semantics are outside
                // the artifact, §4.7) is a warning; anything else cannot
                // round-trip through the editor at all and is an error.
                Some(schema) if !schema.nodes.contains_key(node_type.as_str()) => {
                    if KNOWN_NODE_TYPES.contains(&node_type.as_str())
                        || INLINE_NODE_TYPES.contains(&node_type.as_str())
                    {
                        result.warnings.push(Diag {
                            code: "SCHEMA_NODE_LEGACY",
                            message: format!(
                                "node type {node_type:?} is not declared in the applicable schema artifact but is a known legacy type (document version semantics are outside the artifact)"
                            ),
                        });
                    } else {
                        result.errors.push(Diag {
                            code: "UNKNOWN_NODE_TYPE",
                            message: format!(
                                "node type {node_type:?} is not declared in the applicable schema artifact"
                            ),
                        });
                    }
                }
                Some(schema) => {
                    validate_node_inventory(node_type, map, schema, result);
                }
                // Without an artifact the observed dictionary is the only
                // knowledge: unknown types stay warnings (preserved as-is).
                None => {
                    if !KNOWN_NODE_TYPES.contains(&node_type.as_str())
                        && !INLINE_NODE_TYPES.contains(&node_type.as_str())
                    {
                        result.warnings.push(Diag {
                            code: "UNKNOWN_NODE_TYPE",
                            message: format!("unknown node type {node_type:?} (preserved as-is)"),
                        });
                    }
                }
            }
            if is_root
                && let Some(top_node) = schema.and_then(|s| s.top_node.as_deref())
                && node_type != top_node
            {
                result.warnings.push(Diag {
                            code: "SCHEMA_ROOT_MISMATCH",
                            message: format!(
                                "document root type {node_type:?} differs from the artifact top_node {top_node:?} (document families are application semantics, §4.7)"
                            ),
                        });
            }
        }
        Some(_) => result.errors.push(Diag {
            code: "NOTE_DOC_INVALID",
            message: "node `type` is not a string".to_owned(),
        }),
        None => result.errors.push(Diag {
            code: "NOTE_DOC_INVALID",
            message: "node is missing string `type`".to_owned(),
        }),
    }

    if map.get("type").and_then(Value::as_str) == Some("text")
        && !map.get("text").is_some_and(Value::is_string)
    {
        result.errors.push(Diag {
            code: "NOTE_DOC_INVALID",
            message: "text node is missing string `text`".to_owned(),
        });
    }

    match map.get("attrs") {
        Some(Value::Object(_)) | None => {}
        Some(_) => result.errors.push(Diag {
            code: "NOTE_DOC_INVALID",
            message: "`attrs` is present but is not an object".to_owned(),
        }),
    }

    match map.get("marks") {
        Some(Value::Array(marks)) => {
            for mark in marks {
                if !mark.is_object() {
                    result.errors.push(Diag {
                        code: "NOTE_DOC_INVALID",
                        message: "`marks` contains a non-object entry".to_owned(),
                    });
                    break;
                }
                if let Some(schema) = schema
                    && let Some(mark_type) = mark.get("type").and_then(Value::as_str)
                    && !schema.marks.contains_key(mark_type)
                {
                    result.warnings.push(Diag {
                                code: "SCHEMA_MARK_UNKNOWN",
                                message: format!(
                                    "mark type {mark_type:?} is not declared in the applicable schema artifact (the editor drops it)"
                                ),
                            });
                }
            }
        }
        Some(_) => result.errors.push(Diag {
            code: "NOTE_DOC_INVALID",
            message: "`marks` is present but is not an array".to_owned(),
        }),
        None => {}
    }

    match map.get("content") {
        Some(Value::Array(children)) => {
            for child in children {
                walk_structure(child, depth + 1, limits, result, schema, false)?;
            }
        }
        Some(_) => result.errors.push(Diag {
            code: "NOTE_DOC_INVALID",
            message: "`content` is present but is not an array".to_owned(),
        }),
        None => {}
    }

    Ok(())
}

/// Attribute inventory against the artifact: an attribute the artifact does
/// not declare is dropped by the editor on load — a warning, promotable by
/// `--strict`. Per-node mark rules from the artifact are deliberately not
/// enforced (partial capability; the differential corpus owns that proof).
fn validate_node_inventory(
    node_type: &str,
    map: &serde_json::Map<String, Value>,
    schema: &SchemaArtifact,
    result: &mut NoteValidation,
) {
    let spec = &schema.nodes[node_type];
    if let Some(Value::Object(attrs)) = map.get("attrs") {
        for key in attrs.keys() {
            if !spec.attrs.contains_key(key) {
                result.warnings.push(Diag {
                    code: "SCHEMA_ATTR_UNKNOWN",
                    message: format!(
                        "attr {key:?} on node type {node_type:?} is not declared in the applicable schema artifact (the editor drops it)"
                    ),
                });
            }
        }
    }
}

/// ID checks for documents marked `features.block_ids`: every
/// addressable block has an ID, IDs match `^[a-z0-9]{8,12}$` and are unique.
fn validate_block_ids(doc: &Value, limits: &Limits, result: &mut NoteValidation) {
    let blocks = crate::traverse(doc, None, limits.max_depth);
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for block in &blocks.blocks {
        match &block.id {
            None => result.errors.push(Diag {
                code: "BLOCK_ID_MISSING",
                message: format!(
                    "block at {} has no id though block_ids is enabled",
                    block.pointer
                ),
            }),
            Some(id) => {
                if !is_valid_id(id) {
                    result.errors.push(Diag {
                        code: "BLOCK_ID_INVALID",
                        message: format!(
                            "id {id:?} at {} does not match ^[a-z0-9]{{8,12}}$",
                            block.pointer
                        ),
                    });
                }
                if !seen.insert(id) {
                    result.errors.push(Diag {
                        code: "BLOCK_ID_DUPLICATE",
                        message: format!(
                            "id {id:?} at {} duplicates an earlier block",
                            block.pointer
                        ),
                    });
                }
            }
        }
    }
}

fn is_valid_id(id: &str) -> bool {
    (8..=12).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn broken_structures_are_errors() {
        let limits = Limits::defaults();
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "paragraph", "attrs": "not-an-object", "content": [
                    {"type": "text", "text": "ok"}
                ]},
                {"type": 42},
                {"type": "text"},
                {"type": "paragraph", "marks": "strong"}
            ]
        });
        let result = validate_note_doc(&doc, false, &limits).unwrap();
        let codes: Vec<&str> = result.errors.iter().map(|e| e.code).collect();
        assert!(codes.contains(&"NOTE_DOC_INVALID"), "codes: {codes:?}");
        assert!(!result.valid());
    }

    #[test]
    fn unknown_types_warn_valid_stays_true() {
        let limits = Limits::defaults();
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "exoticMention", "content": [{"type": "text", "text": "?"}]}
            ]
        });
        let result = validate_note_doc(&doc, false, &limits).unwrap();
        assert!(result.valid());
        assert_eq!(result.warnings[0].code, "UNKNOWN_NODE_TYPE");
    }

    #[test]
    fn block_id_rules_apply_only_when_marked() {
        let limits = Limits::defaults();
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "paragraph", "attrs": {"id": "SHORT"}, "content": []}
            ]
        });
        let unmarked = validate_note_doc(&doc, false, &limits).unwrap();
        assert!(unmarked.valid());
        let marked = validate_note_doc(&doc, true, &limits).unwrap();
        let codes: Vec<&str> = marked.errors.iter().map(|e| e.code).collect();
        assert!(codes.contains(&"BLOCK_ID_INVALID"));
    }

    /// A minimal artifact fixture: noteDoc root, paragraph with `id` attr,
    /// bold mark.
    fn fixture_schema() -> crate::schema::SchemaArtifact {
        let raw = json!({
            "schema_id": "inscriptum",
            "schema_version": 1,
            "schema_hash": "sha256:00",
            "top_node": "noteDoc",
            "validation_capability": "partial",
            "nodes": {
                "noteDoc": {"content": "noteTitle block*", "groups": [], "attrs": {},
                            "marks": [], "addressable": false, "inline_content": false,
                            "searchable": false},
                "paragraph": {"content": "inline*", "groups": ["block"],
                              "attrs": {"id": {"default": null}}, "marks": ["bold"],
                              "addressable": true, "inline_content": true, "searchable": true},
                "text": {"content": null, "groups": ["inline"], "attrs": {},
                         "marks": [], "addressable": false, "inline_content": true,
                         "searchable": false}
            },
            "marks": {"bold": {"group": null, "attrs": {}}}
        });
        crate::schema::SchemaArtifact::parse(raw).unwrap()
    }

    #[test]
    fn schema_level_unknown_node_type_is_an_error() {
        let limits = Limits::defaults();
        let schema = fixture_schema();
        let doc = json!({
            "type": "noteDoc",
            "content": [{"type": "exoticWidget", "content": [{"type": "text", "text": "?"}]}]
        });
        let result = validate_note_doc_with_schema(&doc, false, &limits, Some(&schema)).unwrap();
        assert!(!result.valid());
        assert!(result.errors.iter().any(|e| e.code == "UNKNOWN_NODE_TYPE"));
        // the same document without an artifact stays a warning
        let structural = validate_note_doc(&doc, false, &limits).unwrap();
        assert!(structural.valid());
    }

    #[test]
    fn schema_level_unknown_attr_and_mark_are_warnings() {
        let limits = Limits::defaults();
        let schema = fixture_schema();
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "paragraph", "attrs": {"id": "abcdefghij", "color": "red"},
                 "marks": [{"type": "bold"}, {"type": "strike"}],
                 "content": [{"type": "text", "text": "hi", "marks": [{"type": "bold"}]}]}
            ]
        });
        let result = validate_note_doc_with_schema(&doc, false, &limits, Some(&schema)).unwrap();
        assert!(result.valid(), "warnings do not flip validity");
        let codes: Vec<&str> = result.warnings.iter().map(|w| w.code).collect();
        assert!(codes.contains(&"SCHEMA_ATTR_UNKNOWN"), "{codes:?}");
        assert!(codes.contains(&"SCHEMA_MARK_UNKNOWN"), "{codes:?}");
    }

    #[test]
    fn schema_level_legacy_type_is_a_warning_not_an_error() {
        let limits = Limits::defaults();
        let schema = fixture_schema();
        // noteSummary is observed in real on-disk documents but the current
        // artifact does not declare it (title v3 replaced it) — document
        // version semantics the artifact cannot judge.
        let doc = json!({
            "type": "noteDoc",
            "content": [{"type": "noteSummary", "content": [{"type": "text", "text": "old"}]}]
        });
        let result = validate_note_doc_with_schema(&doc, false, &limits, Some(&schema)).unwrap();
        assert!(result.valid());
        let codes: Vec<&str> = result.warnings.iter().map(|w| w.code).collect();
        assert!(codes.contains(&"SCHEMA_NODE_LEGACY"), "{codes:?}");
    }

    #[test]
    fn schema_level_root_mismatch_is_a_warning() {
        let limits = Limits::defaults();
        let schema = fixture_schema();
        let doc = json!({"type": "doc", "content": []});
        let result = validate_note_doc_with_schema(&doc, false, &limits, Some(&schema)).unwrap();
        let codes: Vec<&str> = result.warnings.iter().map(|w| w.code).collect();
        assert!(codes.contains(&"SCHEMA_ROOT_MISMATCH"), "{codes:?}");
    }

    #[test]
    fn schema_level_known_inventory_is_clean() {
        let limits = Limits::defaults();
        let schema = fixture_schema();
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "paragraph", "attrs": {"id": "abcdefghij"},
                 "content": [{"type": "text", "text": "ok", "marks": [{"type": "bold"}]}]}
            ]
        });
        let result = validate_note_doc_with_schema(&doc, false, &limits, Some(&schema)).unwrap();
        assert!(result.valid());
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }
}
