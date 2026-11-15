//! `insum ls` — listing of files and notes.
//!
//! A directory listing classifies files without computing `file_rev` and
//! without opening note payloads; a `.um` file listing reads
//! the central directory and the manifest, but never decompresses payload.
//! The opt-in `--with-properties` / `--with-rev` flags buy per-note
//! property summaries and note revisions at the cost of reading and
//! decompressing every payload; without them every code path below is
//! byte-identical to the plain listing (regression-tested).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use insum_core::container::{argument_stat_error, peek_um_manifest};
use insum_core::{
    ErrorCode, Limits, NoteDescriptor, NoteFile, UmContainer, UmError, collect_properties,
};

use crate::support::{error_value, hidden_below, mtime_iso, render_path};

pub fn run(
    path: Option<String>,
    hidden: bool,
    with_properties: bool,
    with_rev: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    let path = PathBuf::from(path.unwrap_or_else(|| ".".to_owned()));
    // the explicit argument is opened directly (even a symlink); traversal
    // inside directories still never follows symlinks
    let metadata = std::fs::metadata(&path).map_err(|err| argument_stat_error(&path, &err))?;

    if metadata.is_dir() {
        list_directory(&path, hidden, with_properties, with_rev, limits)
    } else if metadata.is_file() {
        list_file(&path, with_properties, with_rev, limits)
    } else {
        Err(UmError::new(
            ErrorCode::PathNotFound,
            format!("not a file or directory: {}", path.display()),
        ))
    }
}

/// Whether the flags demand payload reads (the augmented paths).
fn deep(with_properties: bool, with_rev: bool) -> bool {
    with_properties || with_rev
}

fn list_directory(
    dir: &Path,
    hidden: bool,
    with_properties: bool,
    with_rev: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    let mut entries = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
    {
        match entry {
            Ok(entry) => {
                if !entry.file_type().is_file() {
                    continue; // directories and symlinks are not entries
                }
                let path = entry.into_path();
                // hidden files and node_modules are skipped by default;
                // --hidden re-enables hidden files but not node_modules. The
                // check applies below the traversal root only: the root
                // itself is user-specified (temp dirs are often hidden).
                if hidden_below(dir, &path, hidden) {
                    continue;
                }
                paths.push(path);
            }
            Err(err) => {
                // an unreadable location cannot even be classified
                skipped.push(json!({
                    "path": err.path().map(render_path).unwrap_or_default(),
                    "error": {"code": "IO_ERROR", "message": err.to_string()},
                }));
            }
        }
    }
    paths.sort_by_key(|path| render_path(path));

    for path in paths {
        let rendered = render_path(&path);
        let file_metadata = std::fs::metadata(&path);
        let bytes = file_metadata.as_ref().map(|m| m.len()).unwrap_or(0);
        let kind = match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("um") => Some("notebook"),
            Some(ext) if ext.eq_ignore_ascii_case("note") => Some("note"),
            _ => None,
        };
        let Some(kind) = kind else {
            continue; // traversal only reports.um/.note
        };

        let result = match kind {
            "notebook" if deep(with_properties, with_rev) => notebook_entry_deep(
                &path,
                &rendered,
                bytes,
                &file_metadata,
                with_properties,
                with_rev,
                limits,
            ),
            // central directory + manifest only; payloads stay closed
            "notebook" => peek_um_manifest(&path, limits).map(|(manifest, parts)| {
                json!({
                    "path": rendered,
                    "kind": "notebook",
                    "bytes": bytes,
                    "mtime": mtime_json(&file_metadata),
                    "notes": manifest.notes.len(),
                    "parts": parts,
                })
            }),
            "note" if deep(with_properties, with_rev) => note_entry_deep(
                &path,
                &rendered,
                bytes,
                &file_metadata,
                with_properties,
                with_rev,
                limits,
            ),
            _ => Ok(json!({
                "path": rendered,
                "kind": "note",
                "bytes": bytes,
                "mtime": mtime_json(&file_metadata),
            })),
        };

        match result {
            Ok(entry) => entries.push(entry),
            Err(err) => entries.push(json!({
                "path": rendered,
                "kind": "invalid",
                "bytes": bytes,
                "mtime": Value::Null,
                "error": error_value(&err),
            })),
        }
    }

    let complete = skipped.is_empty();
    Ok(json!({
        "api_version": 1,
        "path": render_path(dir),
        "kind": "dir",
        "entries": entries,
        "skipped": skipped,
        "complete": complete,
    }))
}

fn mtime_json(metadata: &Result<std::fs::Metadata, std::io::Error>) -> Value {
    match metadata {
        Ok(metadata) => mtime_iso(metadata)
            .map(Value::String)
            .unwrap_or(Value::Null),
        Err(_) => Value::Null,
    }
}

/// Directory notebook entry with the opt-in per-note details: every payload
/// is read once (revision and properties in the same pass when both flags
/// are set). A payload that fails to read turns the entry `invalid` — the
/// file-level best-effort semantics of the directory listing.
fn notebook_entry_deep(
    path: &Path,
    rendered: &str,
    bytes: u64,
    file_metadata: &Result<std::fs::Metadata, std::io::Error>,
    with_properties: bool,
    with_rev: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    let mut container = UmContainer::open(path, limits)?;
    let descriptors: Vec<NoteDescriptor> =
        container.notes_in_order().into_iter().cloned().collect();
    let mut details = Vec::new();
    for descriptor in &descriptors {
        let mut detail = json!({ "id": descriptor.id });
        if with_rev {
            let parsed = container.read_note_with_rev(descriptor, limits)?;
            detail["note_rev"] = json!(parsed.note_rev);
            if with_properties {
                let (summary, incomplete) = properties_summary(&parsed.note.doc, limits)?;
                detail["properties"] = Value::Array(summary);
                if incomplete {
                    detail["properties_incomplete"] = json!(true);
                }
            }
        } else {
            let note = container.read_note(descriptor, limits)?;
            let (summary, incomplete) = properties_summary(&note.doc, limits)?;
            detail["properties"] = Value::Array(summary);
            if incomplete {
                detail["properties_incomplete"] = json!(true);
            }
        }
        details.push(detail);
    }
    let (manifest, parts) = (container.manifest.clone(), container.parts.clone());
    Ok(json!({
        "path": rendered,
        "kind": "notebook",
        "bytes": bytes,
        "mtime": mtime_json(file_metadata),
        "notes": manifest.notes.len(),
        "parts": parts,
        "note_details": details,
    }))
}

/// Directory `.note` entry with the opt-in summary/revision: the file is
/// opened (once), so an unreadable `.note` becomes an `invalid` entry.
fn note_entry_deep(
    path: &Path,
    rendered: &str,
    bytes: u64,
    file_metadata: &Result<std::fs::Metadata, std::io::Error>,
    with_properties: bool,
    with_rev: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    let note_file = NoteFile::open(path, limits, with_rev)?;
    let mut entry = json!({
        "path": rendered,
        "kind": "note",
        "bytes": bytes,
        "mtime": mtime_json(file_metadata),
    });
    if with_rev {
        entry["note_rev"] = json!(note_file.note_rev);
    }
    if with_properties {
        let (summary, incomplete) = properties_summary(&note_file.note.doc, limits)?;
        entry["properties"] = Value::Array(summary);
        if incomplete {
            entry["properties_incomplete"] = json!(true);
        }
    }
    Ok(entry)
}

/// Compact per-note property summary for `ls --with-properties`: one
/// `{name, value}` pair per value node; `null` marks withheld text (unknown
/// inline node type) or an unrepresented part — never a silent loss.
/// `incomplete` is true when any text was withheld or a row carried
/// unexpected children.
fn properties_summary(doc: &Value, limits: &Limits) -> Result<(Vec<Value>, bool), UmError> {
    let collected = collect_properties(doc, limits.max_depth);
    if collected.depth_exceeded {
        return Err(UmError::new(
            ErrorCode::LimitExceeded,
            format!("document deeper than UM_MAX_DEPTH ({})", limits.max_depth),
        ));
    }
    let incomplete = collected.unknown_inline
        || collected
            .rows
            .iter()
            .any(|row| !row.shape_warnings.is_empty());
    let mut summary = Vec::new();
    for row in &collected.rows {
        let name = row.name.as_ref().and_then(|name| name.text.clone());
        if row.values.is_empty() {
            summary.push(json!({ "name": name, "value": Value::Null }));
            continue;
        }
        for value in &row.values {
            summary.push(json!({ "name": name, "value": value.text }));
        }
    }
    Ok((summary, incomplete))
}

fn list_file(
    path: &Path,
    with_properties: bool,
    with_rev: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    let rendered = render_path(path);
    let metadata = std::fs::metadata(path).map_err(|err| {
        UmError::new(
            ErrorCode::IoError,
            format!("cannot stat {}: {err}", path.display()),
        )
    })?;
    let bytes = metadata.len();
    let mtime = mtime_iso(&metadata)
        .map(Value::String)
        .unwrap_or(Value::Null);
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());

    match extension.as_deref() {
        Some("um") => {
            let mut container = UmContainer::open(path, limits)?;
            let notes: Vec<Value> = if deep(with_properties, with_rev) {
                note_descriptors_deep(&mut container, with_properties, with_rev, limits)?
            } else {
                container
                    .notes_in_order()
                    .iter()
                    .map(|descriptor| descriptor.to_output())
                    .collect()
            };
            Ok(json!({
                "api_version": 1,
                "path": rendered,
                "kind": "notebook",
                "bytes": bytes,
                "mtime": mtime,
                "file_rev": container.file_rev()?,
                "format": container.manifest.format,
                "type": container.manifest.container_type,
                "features": container.manifest.features,
                "parts": container.parts,
                "notes": notes,
            }))
        }
        Some("note") => {
            let note_file = NoteFile::open(path, limits, true)?;
            let mut entry = json!({
                "api_version": 1,
                "path": rendered,
                "kind": "note",
                "bytes": bytes,
                "mtime": mtime,
                "file_rev": note_file.file_rev,
                "note_rev": note_file.note_rev,
                "legacy": !note_file.note.envelope,
            });
            if note_file.note.envelope {
                entry["format"] = json!("inscriptum-note");
                entry["features"] = note_file.features.clone().unwrap_or(Value::Null);
                entry["schema"] = note_file.schema.clone().unwrap_or(Value::Null);
            }
            if with_properties {
                let (summary, incomplete) = properties_summary(&note_file.note.doc, limits)?;
                entry["properties"] = Value::Array(summary);
                if incomplete {
                    entry["properties_incomplete"] = json!(true);
                }
            }
            Ok(entry)
        }
        _ => Err(UmError::new(
            ErrorCode::PathNotFound,
            format!("not a .um or .note file: {rendered}"),
        )),
    }
}

/// The descriptor list of a `.um` file listing, augmented with the opt-in
/// `note_rev` (payload + descriptor per the revision rule) and the property
/// summary; each payload is read exactly once. Unlike the directory
/// listing, a file listing is fail-fast: a payload that cannot be read
/// fails the command.
fn note_descriptors_deep(
    container: &mut UmContainer,
    with_properties: bool,
    with_rev: bool,
    limits: &Limits,
) -> Result<Vec<Value>, UmError> {
    let descriptors: Vec<NoteDescriptor> =
        container.notes_in_order().into_iter().cloned().collect();
    let mut notes = Vec::with_capacity(descriptors.len());
    for descriptor in &descriptors {
        let mut value = descriptor.to_output();
        if with_rev {
            let parsed = container.read_note_with_rev(descriptor, limits)?;
            value["note_rev"] = json!(parsed.note_rev);
            if with_properties {
                let (summary, incomplete) = properties_summary(&parsed.note.doc, limits)?;
                value["properties"] = Value::Array(summary);
                if incomplete {
                    value["properties_incomplete"] = json!(true);
                }
            }
        } else {
            let note = container.read_note(descriptor, limits)?;
            let (summary, incomplete) = properties_summary(&note.doc, limits)?;
            value["properties"] = Value::Array(summary);
            if incomplete {
                value["properties_incomplete"] = json!(true);
            }
        }
        notes.push(value);
    }
    Ok(notes)
}
