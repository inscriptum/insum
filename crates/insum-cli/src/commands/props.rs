//! `insum props` — read-only view of a note's properties (`noteProperty`
//! rows).
//!
//! A property row holds one `notePropertyName` and zero or more
//! `notePropertyValue` nodes; the projection flattens it into one entry per
//! value node, each carrying the node ids and pointers of the pair plus the
//! row's own address. Without `--note` the target is the title note of a
//! `.um` container (the first note in manifest order) or the single
//! document of a `.note` file.

use std::path::Path;

use serde_json::{Value, json};

use insum_core::{ErrorCode, Limits, UmError, collect_properties};

use crate::support::{Revs, SourceReader, render_path};

pub fn run(
    path: &Path,
    note: Option<&str>,
    get: Option<&str>,
    format: &str,
    limits: &Limits,
) -> Result<Value, UmError> {
    if format != "json" {
        return Err(UmError::usage(format!(
            "unsupported --format {format:?} (props emits one JSON document)"
        )));
    }
    let filters = note.map(|value| vec![value.to_owned()]);
    let mut reader = SourceReader::open(path, filters.as_deref(), limits, Revs::Compute)?;
    if reader.len() == 0 {
        return Err(UmError::new(
            ErrorCode::NoteNotFound,
            "the container has no notes",
        ));
    }
    let view = reader.read_at(0)?;

    let collected = collect_properties(&view.doc, limits.max_depth);
    if collected.depth_exceeded {
        return Err(UmError::new(
            ErrorCode::LimitExceeded,
            format!("document deeper than UM_MAX_DEPTH ({})", limits.max_depth),
        ));
    }

    let mut warnings: Vec<Value> = Vec::new();
    for row in &collected.rows {
        for message in &row.shape_warnings {
            warnings.push(json!({
                "code": "PROPERTY_ROW_SHAPE",
                "message": message,
            }));
        }
    }
    if collected.unknown_inline {
        // text of unknown inline nodes is withheld — no silent losses
        warnings.push(json!({
            "code": "UNKNOWN_NODE_TYPE",
            "message": "some property name/value nodes contain unknown inline node types; their text is not extracted",
        }));
    }

    let entries = entry_values(&collected);
    if collected.rows.is_empty() {
        // an explicit code, not an empty list: "no properties" must not be
        // confusable with "read the wrong note", and it is a NORMAL state
        // (plain pages, title-v2 pages) — the hint says so
        return Err(UmError::new(
            ErrorCode::PropertyNotFound,
            "the note has no noteProperty rows",
        )
        .with_hint(
            "A page without noteProperty nodes is a normal state (plain pages and title-v2 pages have none), not corruption. Use `insum ls --with-properties` for a scan that tolerates empty pages.",
        )
        .with_details(json!({ "note_id": view.note_id })));
    }
    let properties = match get {
        None => entries,
        Some(name) => vec![select_by_name(&entries, name)?],
    };

    Ok(json!({
        "api_version": 1,
        "path": render_path(path),
        "note_id": view.note_id,
        "note_title": view.note_title,
        "note_rev": view.note_rev,
        "properties": properties,
        "complete": true,
        "warnings": warnings,
    }))
}

/// Flatten the rows: one entry per `notePropertyValue` node (a row without
/// value nodes emits one entry with all value fields null). The `name`
/// fields repeat across the values of one row.
fn entry_values(collected: &insum_core::NoteProperties) -> Vec<Value> {
    let mut entries = Vec::new();
    for row in &collected.rows {
        let name = row.name.as_ref();
        let (name_text, name_id, name_pointer) = match name {
            Some(name) => (
                name.text.clone(),
                name.id.clone(),
                Some(name.pointer.clone()),
            ),
            None => (None, None, None),
        };
        if row.values.is_empty() {
            entries.push(json!({
                "name": name_text,
                "name_id": name_id,
                "name_pointer": name_pointer,
                "value": Value::Null,
                "value_id": Value::Null,
                "value_pointer": Value::Null,
                "property_id": row.id,
                "property_pointer": row.pointer,
            }));
            continue;
        }
        for value in &row.values {
            entries.push(json!({
                "name": name_text,
                "name_id": name_id,
                "name_pointer": name_pointer,
                "value": value.text,
                "value_id": value.id,
                "value_pointer": value.pointer,
                "property_id": row.id,
                "property_pointer": row.pointer,
            }));
        }
    }
    entries
}

/// `--get`: exact name match. A missed name and an ambiguous (duplicated)
/// name are both PROPERTY_NOT_FOUND — the duplicate case is signalled
/// structurally (details), mirroring the note-title resolver's ambiguity
/// convention (no silent first match). A note without any row is rejected
/// before this point.
fn select_by_name(entries: &[Value], name: &str) -> Result<Value, UmError> {
    let matches: Vec<&Value> = entries
        .iter()
        .filter(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
        .collect();
    match matches.len() {
        0 => Err(property_absent(name)),
        1 => Ok(matches[0].clone()),
        _ => {
            let pointers: Vec<&str> = matches
                .iter()
                .filter_map(|entry| entry.get("property_pointer").and_then(Value::as_str))
                .collect();
            Err(UmError::new(
                ErrorCode::PropertyNotFound,
                format!(
                    "property name {name:?} is ambiguous: {} noteProperty rows match (pointers: {})",
                    matches.len(),
                    pointers.join(", ")
                ),
            )
            .with_hint(
                "The note contains several rows with this name; read the full `insum props` listing and address a row by its pointer.",
            )
            .with_details(json!({ "ambiguous": true, "pointers": pointers })))
        }
    }
}

fn property_absent(name: &str) -> UmError {
    UmError::new(
        ErrorCode::PropertyNotFound,
        format!("property {name:?} not found"),
    )
    .with_hint(
        "`--get` matches the exact property name; list all properties with `insum props <file>`. A page without noteProperty rows is a normal state (plain pages and title-v2 pages have none), not corruption.",
    )
}
