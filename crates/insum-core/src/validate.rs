//! Note validation — structural checks (§10.1) and coded diagnostics (§10.2).
//!
//! Stage A implements Container and Structural JSON checks plus ID checks
//! for documents marked `features.block_ids`; Schema checks require a schema
//! artifact and arrive with the schema-cli task (§6.6).

use serde::Serialize;
use serde_json::Value;

use crate::UmError;
use crate::error::ErrorCode;
use crate::limits::Limits;
use crate::model::{INLINE_NODE_TYPES, KNOWN_NODE_TYPES};

/// A single diagnostic: `code` is a stable §11 registry name, `message` is
/// human-facing (spec §11). Severity follows §10.2: a rule is either an
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

/// Validate one note document: structure (§10.1 Structural JSON checks),
/// unknown node types (warning, §10.2), and ID rules for documents marked
/// `features.block_ids` (§6.6, §10.1 ID checks).
pub fn validate_note_doc(
    doc: &Value,
    block_ids_enabled: bool,
    limits: &Limits,
) -> Result<NoteValidation, UmError> {
    let mut result = NoteValidation::default();
    walk_structure(doc, 0, limits, &mut result)?;
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
            if !KNOWN_NODE_TYPES.contains(&node_type.as_str())
                && !INLINE_NODE_TYPES.contains(&node_type.as_str())
            {
                result.warnings.push(Diag {
                    code: "UNKNOWN_NODE_TYPE",
                    message: format!("unknown node type {node_type:?} (preserved as-is)"),
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
                walk_structure(child, depth + 1, limits, result)?;
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

/// ID checks for documents marked `features.block_ids` (§10.1): every
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
}
