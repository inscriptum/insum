//! `insum block get` — full subtree of one block (spec §6.3).
//!
//! Stage A addresses blocks by `--pointer`; `--id` arrives with the migration
//! stage (§2.3) and is rejected here.

use std::path::Path;

use serde_json::{Value, json};

use insum_core::model::{BlockEntry, ID_ATTR};
use insum_core::{ErrorCode, Limits, UmError, pointer};

use crate::support::{read_views, render_path};

pub fn run(
    path: &Path,
    note: Option<&str>,
    pointer: Option<&str>,
    id: Option<&str>,
    raw: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    if id.is_some() {
        return Err(UmError::usage(
            "--id is not available on stage A: block IDs are not guaranteed until migration",
        )
        .with_hint("Address the block with --pointer for now (spec §4.3)."));
    }
    let Some(pointer) = pointer else {
        return Err(UmError::usage(
            "block get requires --pointer <pointer> (stage A)",
        ));
    };
    let rendered = render_path(path);
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match extension.as_deref() {
        Some("um") if note.is_none() => {
            return Err(UmError::usage(
                "block get on a .um container requires --note <id>",
            ));
        }
        Some("note") if note.is_some() => {
            return Err(UmError::usage("--note is not allowed for a .note file"));
        }
        _ => {}
    }

    let mut views = read_views(path, note, limits)?;
    let view = views
        .pop()
        .ok_or_else(|| UmError::new(ErrorCode::NoteNotFound, "the container has no notes"))?;

    // The block must exist as a traversable structural node.
    let all_blocks = insum_core::traverse(&view.doc, None, limits.max_depth);
    let entry: &BlockEntry = all_blocks
        .iter()
        .find(|block| block.pointer == pointer)
        .ok_or_else(|| {
            UmError::new(
                ErrorCode::PointerNotFound,
                format!("JSON Pointer {pointer:?} does not address a block of this note"),
            )
            .with_hint("Run `insum tree` to get valid pointers for this note revision.")
        })?;

    // Full node JSON at the pointer, with `attrs.id` lifted out (spec §8.2);
    // `--raw` keeps the original document form.
    let node = pointer::resolve(&view.doc, pointer).map_err(|_| {
        UmError::new(
            ErrorCode::PointerNotFound,
            format!("JSON Pointer {pointer:?} does not resolve in the document"),
        )
    })?;
    let node = if raw {
        node.clone()
    } else {
        strip_ids(node, 0, limits.max_depth)?
    };
    let parent_id = entry
        .parent_pointer
        .as_deref()
        .and_then(|parent| pointer::resolve(&view.doc, parent).ok())
        .and_then(|parent| {
            parent
                .get("attrs")?
                .get(ID_ATTR)?
                .as_str()
                .map(str::to_owned)
        });

    let warnings = collect_unknown_types(&node, limits);
    Ok(json!({
        "api_version": 1,
        "path": rendered,
        "note_id": view.note_id,
        "note_rev": view.note_rev,
        "block": {
            "pointer": entry.pointer,
            "id": entry.id,
            "parent_pointer": entry.parent_pointer,
            "parent_id": parent_id,
            "index": entry.index,
            "node": node,
        },
        "warnings": warnings,
    }))
}

/// Remove `attrs.id` recursively (wire form: the ID travels separately).
fn strip_ids(node: &Value, depth: usize, max_depth: usize) -> Result<Value, UmError> {
    if depth > max_depth {
        return Err(UmError::new(
            ErrorCode::LimitExceeded,
            format!("document deeper than UM_MAX_DEPTH ({max_depth})"),
        ));
    }
    match node {
        Value::Object(map) => {
            let mut copy = serde_json::Map::new();
            for (key, value) in map {
                if key == "attrs"
                    && let Value::Object(attrs) = value
                {
                    let mut attrs_copy = attrs.clone();
                    attrs_copy.remove(ID_ATTR);
                    copy.insert(key.clone(), Value::Object(attrs_copy));
                    continue;
                }
                copy.insert(key.clone(), strip_ids(value, depth + 1, max_depth)?);
            }
            Ok(Value::Object(copy))
        }
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|item| strip_ids(item, depth + 1, max_depth))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        other => Ok(other.clone()),
    }
}

fn collect_unknown_types(node: &Value, limits: &Limits) -> Vec<String> {
    let mut warnings = Vec::new();
    walk_unknown(node, 0, limits.max_depth, &mut warnings);
    warnings.sort();
    warnings.dedup();
    warnings
}

fn walk_unknown(node: &Value, depth: usize, max_depth: usize, warnings: &mut Vec<String>) {
    if depth > max_depth {
        return;
    }
    match node {
        Value::Object(map) => {
            if let Some(node_type) = map.get("type").and_then(Value::as_str)
                && !insum_core::model::INLINE_NODE_TYPES.contains(&node_type)
                && !KNOWN_TYPES.contains(&node_type)
            {
                warnings.push(format!("unknown node type {node_type:?}"));
            }
            for value in map.values() {
                walk_unknown(value, depth + 1, max_depth, warnings);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk_unknown(item, depth + 1, max_depth, warnings);
            }
        }
        _ => {}
    }
}

/// The node dictionary from `~/insum/.instruments/um-format.md` (observed
/// on-disk types). Types outside it are preserved but reported as unknown.
pub const KNOWN_TYPES: [&str; 27] = [
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
