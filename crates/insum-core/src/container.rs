//! Reading `.um` containers and `.note` documents: ZIP safety, revisions,
//! note payload access. Read-only — nothing here writes (spec §2.1, §12).

use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::canon;
use crate::error::{ErrorCode, UmError};
use crate::limits::Limits;
use crate::model::{Manifest, NoteDescriptor, NoteDoc};

/// A `.um` container opened for reading. `file_rev` is computed from the
/// file bytes; note payloads are decompressed on demand only (spec §6.1:
/// central directory + manifest without unpacking payload).
pub struct UmContainer {
    archive: ZipArchive<File>,
    pub manifest: Manifest,
    pub file_rev: String,
    pub bytes: u64,
    /// Top-level archive directories beyond `manifest.json` and `notes/`
    /// (e.g. `assets`), sorted — reported by `ls <file>` as `parts`.
    pub parts: Vec<String>,
}

impl UmContainer {
    pub fn open(path: &Path, limits: &Limits) -> Result<Self, UmError> {
        let bytes = std::fs::metadata(path)
            .map_err(|err| path_error(path, err))?
            .len();
        let file_rev = hash_file(path)?;
        let mut archive = ZipArchive::new(File::open(path).map_err(|err| path_error(path, err))?)
            .map_err(|err| {
            UmError::new(
                ErrorCode::NotAZip,
                format!("file with .um extension is not a valid ZIP archive: {err}"),
            )
            .with_hint("Check the file type or restore a valid UM container.")
        })?;
        check_archive_safety(&mut archive, limits)?;

        let manifest = Manifest::parse(read_manifest_value(&mut archive)?)?;
        let mut parts: Vec<String> = archive
            .file_names()
            .filter_map(|name| name.ok())
            .filter_map(|name| name.split('/').next().map(str::to_owned))
            .filter(|top| top != "manifest.json" && top != "notes")
            .collect();
        parts.sort();
        parts.dedup();

        Ok(Self {
            archive,
            manifest,
            file_rev,
            bytes,
            parts,
        })
    }

    /// Note descriptors sorted by `order`, then `id` (spec §5.3).
    pub fn notes_in_order(&self) -> Vec<&NoteDescriptor> {
        let mut notes: Vec<&NoteDescriptor> = self.manifest.notes.iter().collect();
        notes.sort_by_key(|note| (note.order, note.id.clone()));
        notes
    }

    pub fn find_note(&self, note_id: &str) -> Result<&NoteDescriptor, UmError> {
        self.manifest
            .notes
            .iter()
            .find(|note| note.id == note_id)
            .ok_or_else(|| {
                UmError::new(
                    ErrorCode::NoteNotFound,
                    format!("note {note_id:?} is not in the manifest"),
                )
            })
    }

    /// Read and parse one note payload. The entry size is checked against
    /// the limit before decompression (spec §12).
    pub fn read_note(
        &mut self,
        note: &NoteDescriptor,
        limits: &Limits,
    ) -> Result<NoteDoc, UmError> {
        if note.path.contains("..") || note.path.starts_with('/') || note.path.contains('\\') {
            return Err(UmError::new(
                ErrorCode::ArchiveEntryNameInvalid,
                format!("unsafe note payload path {:?}", note.path),
            ));
        }
        let mut file = self.archive.by_name(&note.path).map_err(|_| {
            UmError::new(
                ErrorCode::NoteFileMissing,
                format!("archive entry {:?} is missing", note.path),
            )
        })?;
        if file.size() > limits.max_entry_bytes as u64 {
            return Err(limit_exceeded(format!(
                "note payload {} exceeds UM_MAX_ENTRY_BYTES",
                note.path
            )));
        }
        let mut payload = String::with_capacity(file.size() as usize);
        file.read_to_string(&mut payload).map_err(|err| {
            UmError::new(
                ErrorCode::NoteJsonInvalid,
                format!("note payload is not UTF-8: {err}"),
            )
        })?;
        let value: Value = serde_json::from_str(&payload).map_err(|err| {
            UmError::new(
                ErrorCode::NoteJsonInvalid,
                format!("note payload is not JSON: {err}"),
            )
        })?;
        NoteDoc::parse(&value)
    }

    /// `note_rev` per spec §5.4: SHA-256 over length-prefixed components —
    /// the domain `UM-NOTE-REV-1`, the uncompressed payload bytes and the
    /// canonical (RFC 8785-style) JSON of the note's manifest descriptor.
    pub fn note_rev(&mut self, note: &NoteDescriptor, limits: &Limits) -> Result<String, UmError> {
        let note_doc = self.read_note(note, limits)?;
        let payload = serde_json::to_vec(&note_doc.doc)
            .map_err(|err| UmError::internal(format!("payload re-serialization failed: {err}")))?;
        let descriptor = canon::canonical_json(&note.raw);
        Ok(note_rev_hex(&payload, descriptor.as_bytes()))
    }
}

/// `note_rev` over already-collected components (shared with `.note` files).
pub fn note_rev_hex(payload_bytes: &[u8], canonical_descriptor: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hash_component(&mut hasher, b"UM-NOTE-REV-1");
    hash_component(&mut hasher, payload_bytes);
    hash_component(&mut hasher, canonical_descriptor);
    hex(&hasher.finalize())
}

/// Length-prefix a component so concatenation stays unambiguous (spec §5.4).
fn hash_component(hasher: &mut Sha256, component: &[u8]) {
    hasher.update((component.len() as u64).to_le_bytes());
    hasher.update(component);
}

/// `note_rev` for a `.note` file: SHA-256 of the whole file bytes
/// (legacy document or full envelope, spec §5.4).
pub fn note_rev_for_file_bytes(file_bytes: &[u8]) -> String {
    sha256_hex(file_bytes)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Stream-hash the whole file — `file_rev` covers every byte (spec §5.4)
/// without holding the file in memory.
fn hash_file(path: &Path) -> Result<String, UmError> {
    let mut file = File::open(path).map_err(|err| path_error(path, err))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| path_error(path, err))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn read_manifest_value(archive: &mut ZipArchive<File>) -> Result<Value, UmError> {
    let mut entry = archive.by_name("manifest.json").map_err(|_| {
        UmError::new(
            ErrorCode::ManifestMissing,
            "no manifest.json at archive root",
        )
    })?;
    let mut text = String::with_capacity(entry.size() as usize);
    entry.read_to_string(&mut text).map_err(|err| {
        UmError::new(
            ErrorCode::ManifestInvalid,
            format!("manifest is not UTF-8: {err}"),
        )
    })?;
    serde_json::from_str(&text).map_err(|err| {
        UmError::new(
            ErrorCode::ManifestInvalid,
            format!("manifest is not valid JSON: {err}"),
        )
    })
}

/// ZIP safety per spec §12: unsafe or duplicate entry names are rejected;
/// entry sizes and totals are checked against the actual central directory.
fn check_archive_safety(archive: &mut ZipArchive<File>, limits: &Limits) -> Result<(), UmError> {
    if archive.len() > limits.max_archive_entries {
        return Err(limit_exceeded(format!(
            "archive has {} entries, UM_MAX_ARCHIVE_ENTRIES is {}",
            archive.len(),
            limits.max_archive_entries
        )));
    }
    let mut total: u64 = 0;
    let mut seen: Vec<String> = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(|err| {
            UmError::new(
                ErrorCode::ArchiveEntryNameInvalid,
                format!("cannot read entry {index}: {err}"),
            )
        })?;
        let name = entry
            .name()
            .map_err(|err| {
                UmError::new(
                    ErrorCode::ArchiveEntryNameInvalid,
                    format!("cannot decode name of entry {index}: {err}"),
                )
            })?
            .to_string();
        check_entry_name(&name)?;
        total = total.saturating_add(entry.size());
        if seen.contains(&name) {
            return Err(UmError::new(
                ErrorCode::ArchiveDuplicateEntry,
                format!("duplicate archive entry {name:?}"),
            ));
        }
        seen.push(name);
    }
    if total > limits.max_total_bytes as u64 {
        return Err(limit_exceeded(format!(
            "uncompressed archive size exceeds UM_MAX_TOTAL_BYTES ({total} bytes)"
        )));
    }
    Ok(())
}

fn check_entry_name(name: &str) -> Result<(), UmError> {
    let invalid = |reason: String| -> UmError {
        UmError::new(
            ErrorCode::ArchiveEntryNameInvalid,
            format!("unsafe archive entry name {name:?}: {reason}"),
        )
    };
    if name.starts_with('/') || name.starts_with('\\') {
        return Err(invalid("absolute path".to_owned()));
    }
    if name.contains('\\') {
        return Err(invalid("backslash in path".to_owned()));
    }
    if name
        .split('/')
        .any(|segment| segment.is_empty() && !name.ends_with('/'))
    {
        return Err(invalid("empty path segment".to_owned()));
    }
    if name.split('/').any(|segment| segment == "..") {
        return Err(invalid("`..` segment".to_owned()));
    }
    if name.contains('\0') {
        return Err(invalid("NUL byte".to_owned()));
    }
    Ok(())
}

fn limit_exceeded(message: String) -> UmError {
    UmError::new(ErrorCode::LimitExceeded, message)
}

fn path_error(path: &Path, err: std::io::Error) -> UmError {
    if err.kind() == std::io::ErrorKind::NotFound {
        UmError::new(
            ErrorCode::PathNotFound,
            format!("path not found: {}", path.display()),
        )
    } else {
        UmError::new(
            ErrorCode::IoError,
            format!("cannot read {}: {err}", path.display()),
        )
    }
}

/// A `.note` document opened for reading (legacy raw doc or envelope).
pub struct NoteFile {
    pub note: NoteDoc,
    /// The raw JSON value (envelope metadata lives here, spec §4.6).
    pub raw: Value,
    pub file_rev: String,
    /// `note_rev` for `.note` is the hash of the whole file (spec §5.4).
    pub note_rev: String,
    pub bytes: u64,
}

impl NoteFile {
    pub fn open(path: &Path, limits: &Limits) -> Result<Self, UmError> {
        let bytes = std::fs::metadata(path)
            .map_err(|err| path_error(path, err))?
            .len();
        if bytes as usize > limits.max_entry_bytes {
            return Err(limit_exceeded(format!(
                "{} exceeds UM_MAX_ENTRY_BYTES ({bytes} bytes)",
                path.display()
            )));
        }
        let raw = std::fs::read(path).map_err(|err| path_error(path, err))?;
        let file_rev = sha256_hex(&raw);
        let note_rev = note_rev_for_file_bytes(&raw);
        let text = String::from_utf8(raw).map_err(|err| {
            UmError::new(
                ErrorCode::NoteJsonInvalid,
                format!("note is not UTF-8: {err}"),
            )
        })?;
        let value: Value = serde_json::from_str(&text).map_err(|err| {
            UmError::new(
                ErrorCode::NoteJsonInvalid,
                format!("note is not valid JSON: {err}"),
            )
        })?;
        let note = NoteDoc::parse(&value.clone())?;
        Ok(Self {
            note,
            raw: value,
            file_rev,
            note_rev,
            bytes,
        })
    }
}

/// Read a `.um` container's manifest without computing `file_rev` and
/// without touching note payloads — the fast path for directory listing
/// (spec §6.1, acceptance §15.1 №3, №12).
pub fn peek_um_manifest(path: &Path, limits: &Limits) -> Result<(Manifest, Vec<String>), UmError> {
    let mut archive = ZipArchive::new(File::open(path).map_err(|err| path_error(path, err))?)
        .map_err(|err| {
            UmError::new(
                ErrorCode::NotAZip,
                format!("file with .um extension is not a valid ZIP archive: {err}"),
            )
            .with_hint("Check the file type or restore a valid UM container.")
        })?;
    check_archive_safety(&mut archive, limits)?;
    let manifest = Manifest::parse(read_manifest_value(&mut archive)?)?;
    let mut parts: Vec<String> = archive
        .file_names()
        .filter_map(|name| name.ok())
        .filter_map(|name| name.split('/').next().map(str::to_owned))
        .filter(|top| top != "manifest.json" && top != "notes")
        .collect();
    parts.sort();
    parts.dedup();
    Ok((manifest, parts))
}

/// Classify a path for `ls`/traversal: `.um` and `.note` (case-insensitive),
/// per spec §5.2.
pub fn classify_extension(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "um" => Some("um"),
        "note" => Some("note"),
        _ => None,
    }
}
