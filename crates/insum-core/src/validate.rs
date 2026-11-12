//! Note validation — structural checks and coded diagnostics.
//!
//! Stage A implements Container and Structural JSON checks plus ID checks
//! for documents marked `features.block_ids`. With an applicable schema
//! artifact (§6.6) the document additionally goes through the
//! ProseMirror-parity check (`pmschema`): content expressions, mark sets
//! and attribute semantics as the editor's real schema judges them. The
//! structural classification of unknown node types (dictionary warnings)
//! is superseded by the parity result when an artifact applies.

use serde::Serialize;
use serde_json::Value;

use crate::UmError;
use crate::error::ErrorCode;
use crate::limits::Limits;
use crate::model::{INLINE_NODE_TYPES, KNOWN_NODE_TYPES};
use crate::pmschema::PmSchema;

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
    validate_note_doc_with_schema(doc, block_ids_enabled, limits, None, None)
}

/// The structural walk plus, when `schema` is `Some`, the
/// ProseMirror-parity check of the whole document (§6.6 "schema" level).
/// The parity result supersedes the structural unknown-type warnings.
///
/// `addressable` is the artifact's addressable type set (§4.7): when given,
/// a missing ID is an error only for blocks of those types — structural
/// types the artifact does not declare addressable (e.g. the observed
/// on-disk `noteSummary`) are exempt. Grammar and uniqueness of the ids
/// that exist are checked regardless. `None` keeps the conservative
/// structural universe (every non-inline block requires an id) for the
/// level where no artifact judges the document.
pub fn validate_note_doc_with_schema(
    doc: &Value,
    block_ids_enabled: bool,
    limits: &Limits,
    schema: Option<&PmSchema>,
    addressable: Option<&std::collections::BTreeSet<String>>,
) -> Result<NoteValidation, UmError> {
    let mut result = NoteValidation::default();
    walk_structure(doc, 0, limits, &mut result)?;
    if block_ids_enabled {
        validate_block_ids(doc, limits, addressable, &mut result);
    }
    if let Some(schema) = schema {
        let parity = schema.check_document(doc);
        // the parity check judges types against the artifact — drop the
        // dictionary-based structural warnings it supersedes
        result
            .warnings
            .retain(|warning| warning.code != "UNKNOWN_NODE_TYPE");
        result.errors.extend(parity.errors);
        result.warnings.extend(parity.warnings);
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
            // The observed dictionary is the structural knowledge: unknown
            // types stay warnings (preserved as-is). With an applicable
            // artifact the parity check classifies them properly and these
            // warnings are superseded (dropped there).
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

    if map.get("type").and_then(Value::as_str) == Some("text") {
        if !map.get("text").is_some_and(Value::is_string) {
            result.errors.push(Diag {
                code: "NOTE_DOC_INVALID",
                message: "text node is missing string `text`".to_owned(),
            });
        } else if map["text"].as_str().unwrap().is_empty() {
            // prosemirror-model rejects empty text nodes outright
            result.errors.push(Diag {
                code: "NOTE_DOC_INVALID",
                message:
                    "text node has an empty `text` (the editor schema rejects empty text nodes)"
                        .to_owned(),
            });
        }
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

/// ID checks for documents marked `features.block_ids`: ids match
/// `^[a-z0-9]{8,12}$` and are unique; a missing ID is an error for
/// `addressable` types only (see [`validate_note_doc_with_schema`]).
fn validate_block_ids(
    doc: &Value,
    limits: &Limits,
    addressable: Option<&std::collections::BTreeSet<String>>,
    result: &mut NoteValidation,
) {
    let blocks = crate::traverse(doc, None, limits.max_depth);
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for block in &blocks.blocks {
        match &block.id {
            None => {
                // §4.7 artifact-driven: without the addressable set the
                // structural level cannot judge per-type addressability and
                // keeps the conservative every-non-inline-block rule.
                let required = addressable.is_none_or(|set| set.contains(&block.node_type));
                if required {
                    result.errors.push(Diag {
                        code: "BLOCK_ID_MISSING",
                        message: format!(
                            "block at {} has no id though block_ids is enabled",
                            block.pointer
                        ),
                    });
                }
            }
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
    fn fixture_schema() -> crate::pmschema::PmSchema {
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
                "noteTitle": {"content": "inline*", "groups": ["block"], "attrs": {},
                              "marks": [], "addressable": false, "inline_content": true,
                              "searchable": true},
                "paragraph": {"content": "inline*", "groups": ["block"],
                              "attrs": {"id": {"default": null}}, "marks": ["bold"],
                              "addressable": true, "inline_content": true, "searchable": true},
                "text": {"content": null, "groups": ["inline"], "attrs": {},
                         "marks": [], "addressable": false, "inline_content": true,
                         "searchable": false}
            },
            "marks": {"bold": {"group": null, "attrs": {}}}
        });
        crate::pmschema::PmSchema::build(&crate::schema::SchemaArtifact::parse(raw).unwrap())
            .unwrap()
    }

    #[test]
    fn schema_level_unknown_node_type_is_an_error() {
        let limits = Limits::defaults();
        let schema = fixture_schema();
        let doc = json!({
            "type": "noteDoc",
            "content": [{"type": "exoticWidget", "content": [{"type": "text", "text": "?"}]}]
        });
        let result =
            validate_note_doc_with_schema(&doc, false, &limits, Some(&schema), None).unwrap();
        assert!(!result.valid());
        assert!(result.errors.iter().any(|e| e.code == "UNKNOWN_NODE_TYPE"));
        // the same document without an artifact stays a warning
        let structural = validate_note_doc(&doc, false, &limits).unwrap();
        assert!(structural.valid());
    }

    #[test]
    fn schema_level_unknown_attr_is_a_warning() {
        let limits = Limits::defaults();
        let schema = fixture_schema();
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
                {"type": "paragraph", "attrs": {"id": "abcdefghij", "color": "red"},
                 "content": [{"type": "text", "text": "hi", "marks": [{"type": "bold"}]}]}
            ]
        });
        let result =
            validate_note_doc_with_schema(&doc, false, &limits, Some(&schema), None).unwrap();
        assert!(result.valid(), "warnings do not flip validity");
        let codes: Vec<&str> = result.warnings.iter().map(|w| w.code).collect();
        assert!(codes.contains(&"SCHEMA_ATTR_UNKNOWN"), "{codes:?}");
    }

    #[test]
    fn schema_level_unknown_and_restricted_marks_are_errors() {
        let limits = Limits::defaults();
        let schema = fixture_schema();
        // strike is not in the artifact's mark inventory at all
        let unknown_mark = json!({
            "type": "noteDoc",
            "content": [
                {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
                {"type": "paragraph", "content": [
                    {"type": "text", "text": "hi", "marks": [{"type": "strike"}]}
                ]}
            ]
        });
        let result =
            validate_note_doc_with_schema(&unknown_mark, false, &limits, Some(&schema), None)
                .unwrap();
        assert!(
            !result.valid(),
            "unknown mark types fail like prosemirror fromJSON"
        );
        let codes: Vec<&str> = result.errors.iter().map(|e| e.code).collect();
        assert!(codes.contains(&"UNKNOWN_MARK_TYPE"), "{codes:?}");

        // the paragraph mark set is [bold] — a bold title child would fail
        let restricted = json!({
            "type": "noteDoc",
            "content": [
                {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
                {"type": "paragraph", "content": [
                    {"type": "text", "text": "hi", "marks": [{"type": "bold"}]}
                ]}
            ]
        });
        let result =
            validate_note_doc_with_schema(&restricted, false, &limits, Some(&schema), None)
                .unwrap();
        assert!(result.valid());
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
        let result =
            validate_note_doc_with_schema(&doc, false, &limits, Some(&schema), None).unwrap();
        assert!(result.valid());
        let codes: Vec<&str> = result.warnings.iter().map(|w| w.code).collect();
        assert!(codes.contains(&"SCHEMA_NODE_LEGACY"), "{codes:?}");
    }

    #[test]
    fn schema_level_root_mismatch_is_a_warning() {
        let limits = Limits::defaults();
        let schema = fixture_schema();
        // the root type is known to the artifact but differs from top_node
        let doc = json!({
            "type": "paragraph",
            "content": [{"type": "text", "text": "x"}]
        });
        let result =
            validate_note_doc_with_schema(&doc, false, &limits, Some(&schema), None).unwrap();
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
                {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
                {"type": "paragraph", "attrs": {"id": "abcdefghij"},
                 "content": [{"type": "text", "text": "ok", "marks": [{"type": "bold"}]}]}
            ]
        });
        let result =
            validate_note_doc_with_schema(&doc, false, &limits, Some(&schema), None).unwrap();
        assert!(result.valid());
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    /// B-1: the id-required universe follows the artifact (§4.7). A
    /// structural type the artifact does not declare addressable
    /// (`noteSummary` on real title pages) is exempt from BLOCK_ID_MISSING,
    /// while an addressable type without an id still fails. Grammar and
    /// uniqueness of the ids that exist are checked regardless of type.
    #[test]
    fn block_id_missing_follows_the_artifact_addressable_set() {
        let limits = Limits::defaults();
        let addressable: std::collections::BTreeSet<String> =
            ["paragraph"].iter().map(|s| s.to_string()).collect();

        // an undeclared noteSummary without an id is exempt
        let summary_only = json!({
            "type": "noteDoc",
            "content": [
                {"type": "noteSummary", "content": [{"type": "text", "text": "old"}]}
            ]
        });
        let result =
            validate_note_doc_with_schema(&summary_only, true, &limits, None, Some(&addressable))
                .unwrap();
        assert!(result.valid(), "{:?}", result.errors);

        // an addressable paragraph without an id is still required
        let paragraph_only = json!({
            "type": "noteDoc",
            "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "p"}]}
            ]
        });
        let result =
            validate_note_doc_with_schema(&paragraph_only, true, &limits, None, Some(&addressable))
                .unwrap();
        let codes: Vec<&str> = result.errors.iter().map(|e| e.code).collect();
        assert!(codes.contains(&"BLOCK_ID_MISSING"), "{codes:?}");

        // grammar of an existing id is checked regardless of the type
        let bad_grammar = json!({
            "type": "noteDoc",
            "content": [
                {"type": "noteSummary", "attrs": {"id": "BAD!!"}, "content": [{"type": "text", "text": "x"}]}
            ]
        });
        let result =
            validate_note_doc_with_schema(&bad_grammar, true, &limits, None, Some(&addressable))
                .unwrap();
        let codes: Vec<&str> = result.errors.iter().map(|e| e.code).collect();
        assert!(
            codes.contains(&"BLOCK_ID_INVALID"),
            "grammar as before: {codes:?}"
        );
    }
}
