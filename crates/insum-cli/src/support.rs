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

/// Hidden segments and `node_modules` are skipped (spec §5.2). The check
/// applies to segments below the traversal root only: the root itself is
/// user-specified (and temp directories are often hidden).
pub fn hidden_below(root: &Path, path: &Path, hidden: bool) -> bool {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .any(|component| {
            let segment = component.as_os_str().to_string_lossy();
            segment == "node_modules" || (!hidden && segment.starts_with('.'))
        })
}

/// A location that could not even be enumerated: best-effort commands report
/// it and lower `complete` (spec §11).
pub struct WalkError {
    pub path: String,
    pub message: String,
}

/// Walk the given paths and collect `.um`/`.note` files (spec §5.2):
/// recursive, hidden files and `node_modules` skipped unless `hidden` (the
/// toggle never re-enables `node_modules`). Explicitly named arguments are
/// resolved like `ls` resolves them — a symlinked directory or file named
/// on the command line is followed; the no-follow rule governs traversal
/// *inside* directories, not user-provided arguments. Silently dropping an
/// explicit argument would turn into a false "0 matches / valid" answer.
pub fn collect_files(
    paths: &[PathBuf],
    hidden: bool,
) -> Result<(Vec<PathBuf>, Vec<WalkError>), UmError> {
    let mut files = Vec::new();
    let mut walk_errors = Vec::new();
    for path in paths {
        // follow symlinks for the argument itself (ls parity, review C-04)
        let metadata = std::fs::metadata(path).map_err(|err| {
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
        // resolve once so WalkDir does not see a symlink root
        let walk_root: PathBuf = if metadata.is_dir() {
            std::fs::canonicalize(path)?
        } else {
            path.clone()
        };
        if metadata.is_dir() {
            for entry in walkdir::WalkDir::new(&walk_root)
                .follow_links(false)
                .sort_by_file_name()
                .into_iter()
            {
                match entry {
                    Ok(entry) => {
                        if !entry.file_type().is_file() {
                            continue; // directories and symlinks are not entries
                        }
                        let entry_path = entry.path();
                        // the skip rules compare against the WALK root (the
                        // canonicalized one): the argument itself is user-
                        // provided and never filtered (review C-04 fallout —
                        // /var → /private/var and hidden temp dirs otherwise
                        // filtered the whole corpus)
                        if hidden_below(&walk_root, entry_path, hidden) {
                            continue;
                        }
                        if classify_extension(entry_path).is_some() {
                            files.push(entry.into_path());
                        }
                    }
                    Err(err) => walk_errors.push(WalkError {
                        path: err.path().map(render_path).unwrap_or_default(),
                        message: err.to_string(),
                    }),
                }
            }
        } else if classify_extension(path).is_some() {
            files.push(path.clone());
        }
    }
    files.sort_by_key(|path| render_path(path));
    files.dedup_by_key(|path| render_path(path));
    Ok((files, walk_errors))
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
    fn from_descriptor(descriptor: &NoteDescriptor, doc: Value, note_rev: String) -> Self {
        Self {
            note_id: Some(descriptor.id.clone()),
            note_title: note_title(descriptor.title.as_deref(), &doc),
            note_rev,
            doc,
            features: descriptor.features.clone(),
            block_ids_enabled: descriptor.block_ids_enabled(),
        }
    }
}

/// The result of opening one document path: the requested notes in manifest
/// order, one decompression per payload.
pub struct SourceRead {
    pub views: Vec<NoteView>,
}

/// Whether to compute note revisions while reading (review P-05: search
/// matches text and never uses revisions — hashing every payload is pure
/// overhead there).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Revs {
    Compute,
    Skip,
}

/// Open a `.um`/`.note` path and read the requested notes in manifest order.
/// `filters` selects notes by id (repeatable `--note`); `None` reads all.
///
/// NEW-8 (documented): repeated `--note` flags select WHICH notes are read,
/// but the output order always follows the manifest (order, then id) — not
/// the order of the flags. That keeps every response deterministic per §3.11
/// regardless of argument order.
pub fn read_notes(
    path: &Path,
    filters: Option<&[String]>,
    limits: &Limits,
    revs: Revs,
) -> Result<SourceRead, UmError> {
    match classify_extension(path) {
        Some("um") => {
            let mut container = UmContainer::open(path, limits)?;
            if let Some(filters) = filters {
                let mut views = Vec::with_capacity(filters.len());
                for filter in filters {
                    let descriptor = container.find_note(filter)?.clone();
                    views.push(read_view(&mut container, &descriptor, limits, revs)?);
                }
                return Ok(SourceRead { views });
            }
            let descriptors: Vec<NoteDescriptor> =
                container.notes_in_order().into_iter().cloned().collect();
            let mut views = Vec::with_capacity(descriptors.len());
            for descriptor in &descriptors {
                views.push(read_view(&mut container, descriptor, limits, revs)?);
            }
            Ok(SourceRead { views })
        }
        Some("note") => {
            if filters.is_some() {
                return Err(UmError::usage(
                    "--note is not allowed for a .note file (it holds a single document)",
                ));
            }
            let note_file = NoteFile::open(path, limits, revs == Revs::Compute)?;
            let features = note_file.note.features.clone();
            let block_ids_enabled = features
                .as_ref()
                .and_then(|f| f.get("block_ids"))
                .and_then(Value::as_u64)
                == Some(1);
            Ok(SourceRead {
                views: vec![NoteView {
                    note_id: None,
                    note_title: note_title(None, &note_file.note.doc),
                    note_rev: note_file.note_rev,
                    doc: note_file.note.doc,
                    features,
                    block_ids_enabled,
                }],
            })
        }
        _ => Err(UmError::new(
            ErrorCode::PathNotFound,
            format!("no .um or .note document at {}", path.display()),
        )),
    }
}

/// Read one note into a view; revisions are computed in the same
/// decompression pass when requested (review P-02/P-05).
fn read_view(
    container: &mut UmContainer,
    descriptor: &NoteDescriptor,
    limits: &Limits,
    revs: Revs,
) -> Result<NoteView, UmError> {
    match revs {
        Revs::Compute => {
            let parsed = container.read_note_with_rev(descriptor, limits)?;
            Ok(NoteView::from_descriptor(
                descriptor,
                parsed.note.doc,
                parsed.note_rev,
            ))
        }
        Revs::Skip => {
            // P-05: the light path — parse only, no SHA-256/canonical_json
            let note_doc = container.read_note(descriptor, limits)?;
            Ok(NoteView::from_descriptor(
                descriptor,
                note_doc.doc,
                String::new(),
            ))
        }
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

/// Lazy note reader over one document path: the container/manifest opens
/// once, payloads are parsed on demand — a pagination page only pays for the
/// notes it actually emits (review P-02) while multi-note walks stay
/// complete (review NEW-1/NEW-2).
pub struct SourceReader {
    limits: Limits,
    revs: Revs,
    container: Option<UmContainer>,
    order: Vec<NoteDescriptor>,
    cache: Vec<Option<NoteView>>,
    /// NEW-7: light metadata only — the document lives exclusively in
    /// `single`, never cloned.
    note_meta: Option<NoteMeta>,
    single: Option<NoteView>,
}

/// File-level metadata of a `.note` document (no document copy).
struct NoteMeta {
    file_rev: String,
}

impl SourceReader {
    pub fn open(
        path: &Path,
        filters: Option<&[String]>,
        limits: &Limits,
        revs: Revs,
    ) -> Result<Self, UmError> {
        match classify_extension(path) {
            Some("um") => {
                let container = UmContainer::open(path, limits)?;
                let mut order: Vec<NoteDescriptor> =
                    container.notes_in_order().into_iter().cloned().collect();
                if let Some(filters) = filters {
                    // manifest order preserved (§5.3); unknown ids are errors
                    for filter in filters {
                        if !order.iter().any(|note| &note.id == filter) {
                            return Err(container.find_note(filter).unwrap_err());
                        }
                    }
                    order.retain(|note| filters.contains(&note.id));
                }
                let cache: Vec<Option<NoteView>> = (0..order.len()).map(|_| None).collect();
                Ok(Self {
                    limits: *limits,
                    revs,
                    container: Some(container),
                    order,
                    cache,
                    note_meta: None,
                    single: None,
                })
            }
            Some("note") => {
                if filters.is_some() {
                    return Err(UmError::usage(
                        "--note is not allowed for a .note file (it holds a single document)",
                    ));
                }
                // single note: eager open is the lazy path; the document is
                // MOVED into `single` (NEW-7: no second copy)
                let note_file = NoteFile::open(path, limits, revs == Revs::Compute)?;
                let features = note_file.note.features.clone();
                let block_ids_enabled = features
                    .as_ref()
                    .and_then(|f| f.get("block_ids"))
                    .and_then(Value::as_u64)
                    == Some(1);
                let single = NoteView {
                    note_id: None,
                    note_title: note_title(None, &note_file.note.doc),
                    note_rev: note_file.note_rev.clone(),
                    doc: note_file.note.doc,
                    features,
                    block_ids_enabled,
                };
                let note_meta = NoteMeta {
                    file_rev: note_file.file_rev,
                };
                Ok(Self {
                    limits: *limits,
                    revs,
                    container: None,
                    order: Vec::new(),
                    cache: Vec::new(),
                    note_meta: Some(note_meta),
                    single: Some(single),
                })
            }
            _ => Err(UmError::new(
                ErrorCode::PathNotFound,
                format!("no .um or .note document at {}", path.display()),
            )),
        }
    }

    /// Number of exposed notes (filtered sequence, manifest order).
    ///
    /// NEW-8 (documented): repeated `--note` flags select which notes are
    /// exposed; the order stays manifest (order, then id), not flag order.
    pub fn len(&self) -> usize {
        if self.container.is_some() {
            self.order.len()
        } else {
            1
        }
    }

    /// Read (and cache) the note at `pos`; revisions computed per `Revs`.
    pub fn read_at(&mut self, pos: usize) -> Result<&NoteView, UmError> {
        if self.container.is_some() {
            if pos >= self.order.len() {
                return Err(UmError::new(
                    ErrorCode::NoteNotFound,
                    format!("note position {pos} is out of range"),
                ));
            }
            if self.cache[pos].is_none() {
                let descriptor = self.order[pos].clone();
                let mut container = self.container.take().expect("container present");
                let view = read_view(&mut container, &descriptor, &self.limits, self.revs)?;
                self.container = Some(container);
                self.cache[pos] = Some(view);
            }
            Ok(self.cache[pos].as_ref().expect("just cached"))
        } else {
            Ok(self.single.as_ref().expect("single note"))
        }
    }

    /// The file-level revision, computed on demand (review P-05: consumers
    /// that never output it never pay for it).
    pub fn file_rev(&mut self) -> Result<String, UmError> {
        if let Some(container) = self.container.as_mut() {
            // NEW-6: the hash streams through the descriptor held since open
            // — no second open, no TOCTOU window between payload reads and
            // the file hash.
            return container.file_rev();
        }
        let meta = self.note_meta.as_ref().expect("note meta");
        // NEW-6: the re-open trap is removed — the rev was computed from the
        // same bytes as the document (§5.4: both `.note` revs hash one read).
        // A Skip-path consumer (search/validate) never calls this.
        if meta.file_rev.is_empty() {
            return Err(UmError::internal(
                "file revision was not computed (light read path)",
            ));
        }
        Ok(meta.file_rev.clone())
    }
}
