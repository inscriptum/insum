//! Shared CLI plumbing: path rendering, directory walking, mtime formatting,
//! note sources.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Value, json};

use insum_core::container::classify_extension;
use insum_core::model::note_title;
use insum_core::{ErrorCode, Limits, NoteDescriptor, NoteFile, UmContainer, UmError};

/// Paths in JSON use `/` regardless of the OS (spec §5.2).
pub fn render_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    text.strip_prefix("./").unwrap_or(&text).to_owned()
}

/// `YYYY-MM-DDTHH:MM:SSZ` (UTC) or null when the mtime is unavailable.
pub fn mtime_iso(metadata: &std::fs::Metadata) -> Option<String> {
    let modified: SystemTime = metadata.modified().ok()?;
    let secs = modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let days = (secs / 86_400) as i64;
    let time = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    ))
}

/// Howard Hinnant's civil-from-days algorithm (public domain).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// Walk the given paths and collect `.um`/`.note` files (spec §5.2):
/// recursive, no symlink following, hidden files and `node_modules` skipped
/// unless `hidden` (the toggle never re-enables `node_modules`).
pub fn collect_files(paths: &[PathBuf], hidden: bool) -> Result<Vec<PathBuf>, UmError> {
    let mut files = Vec::new();
    for path in paths {
        let metadata = std::fs::symlink_metadata(path).map_err(|err| {
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
            for entry in walkdir::WalkDir::new(path)
                .follow_links(false)
                .sort_by_file_name()
                .into_iter()
                .filter_map(Result::ok)
            {
                if !entry.file_type().is_file() {
                    continue; // directories and symlinks are not entries
                }
                let entry_path = entry.path();
                // skip rules apply below the traversal root only (the root
                // itself is user-specified; temp dirs are often hidden)
                let below_root = entry_path.strip_prefix(path).unwrap_or(entry_path);
                if should_skip(below_root, hidden) {
                    continue;
                }
                if classify_extension(entry_path).is_some() {
                    files.push(entry.into_path());
                }
            }
        } else if !metadata.is_symlink() && classify_extension(path).is_some() {
            files.push(path.clone());
        }
    }
    files.sort_by_key(|path| render_path(path));
    files.dedup_by_key(|path| render_path(path));
    Ok(files)
}

/// Hidden segments and `node_modules` are skipped (spec §5.2).
fn should_skip(path: &Path, hidden: bool) -> bool {
    path.components().any(|component| {
        let segment = component.as_os_str().to_string_lossy();
        segment == "node_modules" || (!hidden && segment.starts_with('.'))
    })
}

/// One note ready for output: identity, revision, document, features.
pub struct NoteView {
    pub note_id: Option<String>,
    pub note_title: Option<String>,
    pub note_rev: String,
    pub doc: Value,
    pub features: Option<Value>,
    pub block_ids_enabled: bool,
}

impl NoteView {
    fn from_descriptor(descriptor: &NoteDescriptor, doc: &Value, note_rev: String) -> Self {
        Self {
            note_id: Some(descriptor.id.clone()),
            note_title: note_title(descriptor.title.as_deref(), doc),
            note_rev,
            doc: doc.clone(),
            features: descriptor.features.clone(),
            block_ids_enabled: descriptor.block_ids_enabled(),
        }
    }
}

/// Open a `.um`/`.note` path and read the requested notes in manifest order.
pub fn read_views(
    path: &Path,
    note_filter: Option<&str>,
    limits: &Limits,
) -> Result<Vec<NoteView>, UmError> {
    match classify_extension(path) {
        Some("um") => {
            let mut container = UmContainer::open(path, limits)?;
            if let Some(filter) = note_filter {
                let descriptor = container.find_note(filter)?.clone();
                let note_rev = container.note_rev(&descriptor, limits)?;
                let note_doc = container.read_note(&descriptor, limits)?;
                return Ok(vec![NoteView::from_descriptor(
                    &descriptor,
                    &note_doc.doc,
                    note_rev,
                )]);
            }
            let descriptors: Vec<NoteDescriptor> =
                container.notes_in_order().into_iter().cloned().collect();
            let mut views = Vec::with_capacity(descriptors.len());
            for descriptor in &descriptors {
                let note_rev = container.note_rev(descriptor, limits)?;
                let note_doc = container.read_note(descriptor, limits)?;
                views.push(NoteView::from_descriptor(
                    descriptor,
                    &note_doc.doc,
                    note_rev,
                ));
            }
            Ok(views)
        }
        Some("note") => {
            if note_filter.is_some() {
                return Err(UmError::usage(
                    "--note is not allowed for a .note file (it holds a single document)",
                ));
            }
            let note_file = NoteFile::open(path, limits)?;
            let features = note_file.note.features.clone();
            let block_ids_enabled = features
                .as_ref()
                .and_then(|f| f.get("block_ids"))
                .and_then(Value::as_u64)
                == Some(1);
            Ok(vec![NoteView {
                note_id: None,
                note_title: note_title(None, &note_file.note.doc),
                note_rev: note_file.note_rev.clone(),
                doc: note_file.note.doc.clone(),
                features,
                block_ids_enabled,
            }])
        }
        _ => Err(UmError::new(
            ErrorCode::PathNotFound,
            format!("no .um or .note document at {}", path.display()),
        )),
    }
}

/// Compact JSON for an error inside `skipped`/`invalid` entries.
pub fn error_value(err: &UmError) -> Value {
    let mut error = json!({
        "code": err.code.as_str(),
        "message": err.message,
    });
    if let Some(hint) = &err.hint {
        error["hint"] = json!(hint);
    }
    error
}

/// What a command produces for stdout: one JSON document or the annotated
/// text projection (spec §5.3, §6.5).
pub enum Output {
    Json(Value),
    Text(String),
}
