//! `insum ls` — listing of files and notes (spec §6.1).
//!
//! A directory listing classifies files without computing `file_rev` and
//! without opening note payloads (§15.1 №3, №12); a `.um` file listing reads
//! the central directory and the manifest, but never decompresses payload.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use insum_core::container::peek_um_manifest;
use insum_core::{ErrorCode, Limits, NoteFile, UmContainer, UmError};

use crate::support::{error_value, hidden_below, mtime_iso, render_path};

pub fn run(path: Option<String>, hidden: bool, limits: &Limits) -> Result<Value, UmError> {
    let path = PathBuf::from(path.unwrap_or_else(|| ".".to_owned()));
    let metadata = std::fs::symlink_metadata(&path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            UmError::new(
                ErrorCode::PathNotFound,
                format!("path not found: {}", path.display()),
            )
        } else {
            UmError::new(
                ErrorCode::IoError,
                format!("cannot stat {}: {err}", path.display()),
            )
        }
    })?;

    if metadata.is_dir() {
        list_directory(&path, hidden, limits)
    } else if metadata.is_file() {
        list_file(&path, limits)
    } else {
        Err(UmError::new(
            ErrorCode::PathNotFound,
            format!("not a file or directory: {}", path.display()),
        ))
    }
}

fn list_directory(dir: &Path, hidden: bool, limits: &Limits) -> Result<Value, UmError> {
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
                // hidden files and node_modules are skipped by default (§5.2);
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
            continue; // traversal only reports .um/.note (spec §5.2)
        };

        let result = match kind {
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

fn list_file(path: &Path, limits: &Limits) -> Result<Value, UmError> {
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
            let container = UmContainer::open(path, limits)?;
            Ok(json!({
                "api_version": 1,
                "path": rendered,
                "kind": "notebook",
                "bytes": bytes,
                "mtime": mtime,
                "file_rev": container.file_rev,
                "format": container.manifest.format,
                "type": container.manifest.container_type,
                "features": container.manifest.features,
                "parts": container.parts,
                "notes": container
                    .notes_in_order()
                    .iter()
                    .map(|descriptor| descriptor.to_output())
                    .collect::<Vec<_>>(),
            }))
        }
        Some("note") => {
            let note_file = NoteFile::open(path, limits)?;
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
                entry["features"] = note_file
                    .raw
                    .get("features")
                    .cloned()
                    .unwrap_or(Value::Null);
                entry["schema"] = note_file.raw.get("schema").cloned().unwrap_or(Value::Null);
            }
            Ok(entry)
        }
        _ => Err(UmError::new(
            ErrorCode::PathNotFound,
            format!("not a .um or .note file: {rendered}"),
        )),
    }
}
