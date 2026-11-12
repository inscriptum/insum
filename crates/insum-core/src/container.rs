//! Reading `.um` containers and `.note` documents: ZIP safety, revisions,
//! note payload access. Read-only — nothing here writes.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::canon;
use crate::error::{ErrorCode, UmError};
use crate::limits::Limits;
use crate::model::{Manifest, NoteDescriptor, NoteDoc};

/// A `.um` container opened for reading. `file_rev` is computed from the
/// file bytes; note payloads are decompressed on demand only (
/// central directory + manifest without unpacking payload).
pub struct UmContainer {
    archive: ZipArchive<File>,
    /// Held open since container open — `file_rev` streams through
    /// THIS descriptor (seek 0 + read), never re-opens the path, so a
    /// concurrent file replacement cannot mix generations between payload
    /// reads and the file hash.
    file_handle: File,
    /// Kept for error messages only (never re-opened).
    path: PathBuf,
    pub manifest: Manifest,
    /// Computed on demand (consumers that never output it never
    /// pay for the full-file hash).
    file_rev_cache: Option<String>,
    pub bytes: u64,
    /// Top-level archive directories beyond `manifest.json` and `notes/`
    /// (e.g. `assets`), sorted — reported by `ls <file>` as `parts`.
    pub parts: Vec<String>,
}

impl UmContainer {
    pub fn open(path: &Path, limits: &Limits) -> Result<Self, UmError> {
        let (file, bytes) = open_non_empty(path)?;
        let mut archive =
            ZipArchive::new(file).map_err(|err| classify_zip_open_error(path, err))?;
        check_archive_safety(&mut archive, limits)?;

        let manifest = Manifest::parse(read_manifest_value(path, &mut archive, limits)?)?;
        let mut parts: Vec<String> = archive
            .file_names()
            .filter_map(|name| name.ok())
            .filter_map(|name| name.split('/').next().map(str::to_owned))
            .filter(|top| top != "manifest.json" && top != "notes")
            .collect();
        parts.sort();
        parts.dedup();

        let file_handle = File::open(path).map_err(|err| path_error(path, err))?;
        Ok(Self {
            archive,
            file_handle,
            path: path.to_owned(),
            manifest,
            file_rev_cache: None,
            bytes,
            parts,
        })
    }

    /// `file_rev` — SHA-256 of all file bytes, computed on first
    /// request through the descriptor held since open (no re-open,
    /// seek(0) + read), then cached.
    pub fn file_rev(&mut self) -> Result<String, UmError> {
        if self.file_rev_cache.is_none() {
            let mut handle = &self.file_handle;
            handle
                .seek(SeekFrom::Start(0))
                .map_err(|err| path_error(&self.path, err))?;
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let read = handle.read(&mut buffer).map_err(|err| {
                    UmError::new(
                        ErrorCode::IoError,
                        format!("cannot hash {}: {err}", self.path.display()),
                    )
                })?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            self.file_rev_cache = Some(format_rev(hex(&hasher.finalize())));
        }
        Ok(self.file_rev_cache.clone().expect("just cached"))
    }

    /// The archive over the descriptor held since open. The write path uses
    /// it to rebuild the container from the in-lock generation: raw entries
    /// are copied through THIS handle — never through a fresh open of the
    /// path, which a non-cooperative writer could have swapped (M-1/SEC-3).
    pub(crate) fn zip_archive(&mut self) -> &mut ZipArchive<File> {
        &mut self.archive
    }

    /// Note descriptors sorted by `order`, then `id`.
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
    /// the limit before decompression. No revision is computed —
    /// the light path for consumers that never output revisions.
    pub fn read_note(
        &mut self,
        note: &NoteDescriptor,
        limits: &Limits,
    ) -> Result<NoteDoc, UmError> {
        self.read_note_raw(note, limits)
    }

    /// Read one note payload and compute its `note_rev` in the same pass.
    ///
    /// `note_rev` per: SHA-256 over length-prefixed components —
    /// the domain `UM-NOTE-REV-1`, the RAW uncompressed payload bytes (not a
    /// re-serialization) and the canonical (RFC 8785-style) JSON of the
    /// note's manifest descriptor.
    pub fn read_note_with_rev(
        &mut self,
        note: &NoteDescriptor,
        limits: &Limits,
    ) -> Result<ParsedNote, UmError> {
        let payload = self.read_note_payload(note, limits)?;
        let note_doc = NoteDoc::parse(parse_json_limited(
            &payload,
            limits.max_depth,
            ErrorCode::NoteJsonInvalid,
        )?)?;
        let descriptor = canon::canonical_json(&note.raw);
        let note_rev = format_rev(note_rev_hex(payload.as_bytes(), descriptor.as_bytes()));
        Ok(ParsedNote {
            note: note_doc,
            note_rev,
        })
    }

    /// Decompress and return the RAW payload bytes (no parsing, no hashing).
    fn read_note_payload(
        &mut self,
        note: &NoteDescriptor,
        limits: &Limits,
    ) -> Result<String, UmError> {
        if note.path.starts_with('/')
            || note.path.contains('\\')
            || note.path.split('/').any(|segment| segment == "..")
        {
            return Err(UmError::new(
                ErrorCode::ArchiveEntryNameInvalid,
                format!("unsafe note payload path {:?}", note.path),
            ));
        }
        let mut file = self.archive.by_name(&note.path).map_err(|err| {
            if matches!(err, zip::result::ZipError::FileNotFound) {
                UmError::new(
                    ErrorCode::NoteFileMissing,
                    format!("archive entry {:?} is missing", note.path),
                )
            } else {
                // the central directory opened but the entry cannot be
                // produced — a cut between directory and payload
                torn_error(
                    "truncated",
                    &self.path,
                    format!("archive entry {:?} cannot be read: {err}", note.path),
                )
            }
        })?;
        if file.size() > limits.max_entry_bytes as u64 {
            return Err(limit_exceeded(format!(
                "note payload {} exceeds UM_MAX_ENTRY_BYTES",
                note.path
            )));
        }
        let mut payload_bytes = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut payload_bytes).map_err(|err| {
            torn_error(
                "truncated",
                &self.path,
                format!("note payload {} cannot be decompressed: {err}", note.path),
            )
        })?;
        let payload = String::from_utf8(payload_bytes).map_err(|_| {
            UmError::new(
                ErrorCode::NoteJsonInvalid,
                format!("note payload is not UTF-8: {}", note.path),
            )
        })?;
        Ok(payload)
    }

    /// Parse the payload into a document (bounded depth).
    fn read_note_raw(
        &mut self,
        note: &NoteDescriptor,
        limits: &Limits,
    ) -> Result<NoteDoc, UmError> {
        let payload = self.read_note_payload(note, limits)?;
        let value = parse_json_limited(&payload, limits.max_depth, ErrorCode::NoteJsonInvalid)?;
        NoteDoc::parse(value)
    }
}

/// A note payload together with its logical revision.
pub struct ParsedNote {
    pub note: NoteDoc,
    pub note_rev: String,
}

/// Parse JSON with the depth limit enforced on the raw text before parsing
/// (so hostile nesting fails as `LIMIT_EXCEEDED`, not as a parse error) and
/// with serde_json's own recursion limit disabled afterwards.
fn parse_json_limited(
    text: &str,
    max_depth: usize,
    parse_error: ErrorCode,
) -> Result<Value, UmError> {
    if json_depth_exceeds(text, max_depth) {
        return Err(limit_exceeded(format!(
            "JSON depth exceeds UM_MAX_DEPTH ({max_depth})"
        )));
    }
    let deserializer = &mut serde_json::Deserializer::from_str(text);
    deserializer.disable_recursion_limit();
    Value::deserialize(deserializer)
        .map_err(|err| UmError::new(parse_error, format!("payload is not valid JSON: {err}")))
}

/// Single-pass scan of the raw JSON text tracking `[`/`{` nesting outside
/// strings — cheap and recursion-free.
fn json_depth_exceeds(text: &str, max_depth: usize) -> bool {
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escaped = false;
    for ch in text.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else {
                match ch {
                    '\\' => escaped = true,
                    '"' => in_string = false,
                    _ => {}
                }
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '[' | '{' => {
                depth += 1;
                if depth > max_depth {
                    return true;
                }
            }
            ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

/// The wire format of every revision (// examples: `sha256:` +
/// 64 hex characters). Internal hashing stays on raw bytes.
pub fn format_rev(hex: String) -> String {
    format!("sha256:{hex}")
}

/// `note_rev` per — the normative formula:
/// `SHA-256("UM-NOTE-REV-1\x00" + BE64(len) + payload + BE64(len) + descriptor)`
/// where the descriptor is the canonical (RFC 8785-style) JSON of the note's
/// manifest entry.'s "length before value" wording does not fix the byte
/// order; does (big-endian) and wins as the more precise contract.
pub fn note_rev_hex(payload_bytes: &[u8], canonical_descriptor: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"UM-NOTE-REV-1\x00");
    hasher.update((payload_bytes.len() as u64).to_be_bytes());
    hasher.update(payload_bytes);
    hasher.update((canonical_descriptor.len() as u64).to_be_bytes());
    hasher.update(canonical_descriptor);
    hex(&hasher.finalize())
}

/// `note_rev` for a `.note` file: SHA-256 of the whole file bytes
/// (legacy document or full envelope).
pub fn note_rev_for_file_bytes(file_bytes: &[u8]) -> String {
    format_rev(sha256_hex(file_bytes))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// SHA-256 of a file's CURRENT bytes, streamed from a fresh open. Unlike
/// [`UmContainer::file_rev`] (which reads through the handle held since
/// open and therefore sees the generation that handle was opened on), this
/// re-reads the path — the write path's pre-replace revision re-check
/// (§9.3) must see the file the way any other process would.
pub fn file_sha256(path: &Path) -> Result<String, UmError> {
    let mut file = File::open(path).map_err(|err| path_error(path, err))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|err| {
            UmError::new(
                ErrorCode::IoError,
                format!("cannot hash {}: {err}", path.display()),
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format_rev(hex(&hasher.finalize())))
}

/// Read a file with a hard byte cap (limits are enforced on the
/// actual bytes, races with growing files included).
fn read_bounded(path: &Path, limits: &Limits) -> Result<Vec<u8>, UmError> {
    let file = File::open(path).map_err(|err| path_error(path, err))?;
    let mut buffer = Vec::new();
    let mut taken = file.take(limits.max_entry_bytes as u64 + 1);
    taken
        .read_to_end(&mut buffer)
        .map_err(|err| path_error(path, err))?;
    if buffer.is_empty() {
        return Err(torn_error(
            "empty",
            path,
            "container file is empty (write in progress or interrupted)".to_owned(),
        ));
    }
    if buffer.len() > limits.max_entry_bytes {
        return Err(limit_exceeded(format!(
            "{} exceeds UM_MAX_ENTRY_BYTES ({} bytes)",
            path.display(),
            limits.max_entry_bytes
        )));
    }
    Ok(buffer)
}

fn hex(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn read_manifest_value(
    path: &Path,
    archive: &mut ZipArchive<File>,
    limits: &Limits,
) -> Result<Value, UmError> {
    let _depth = limits.max_depth;
    let mut entry = archive.by_name("manifest.json").map_err(|_| {
        UmError::new(
            ErrorCode::ManifestMissing,
            "no manifest.json at archive root",
        )
    })?;
    // capacity is bounded: the declared entry size is attacker-controlled
    let mut bytes = Vec::with_capacity(entry.size().min(1 << 20) as usize);
    entry.read_to_end(&mut bytes).map_err(|err| {
        // decompression stopped mid-stream: the archive was cut off between
        // the central directory (parsed fine) and this entry's payload
        torn_error(
            "truncated",
            path,
            format!("manifest.json cannot be decompressed: {err}"),
        )
    })?;
    let text = String::from_utf8(bytes)
        .map_err(|_| UmError::new(ErrorCode::ManifestInvalid, "manifest is not UTF-8"))?;
    parse_json_limited(&text, limits.max_depth, ErrorCode::ManifestInvalid)
}

/// ZIP safety per: unsafe or duplicate entry names are rejected;
/// entry sizes and totals are checked against the actual central directory.
fn check_archive_safety(archive: &mut ZipArchive<File>, limits: &Limits) -> Result<(), UmError> {
    if archive.len() > limits.max_archive_entries {
        return Err(limit_exceeded(format!(
            "archive has {} entries, UM_MAX_ARCHIVE_ENTRIES is {}",
            archive.len(),
            limits.max_archive_entries
        )));
    }
    // zip 9 keeps entries in a name-keyed map, so duplicates collapse during
    // lookup — the explicit probe is the only reliable rejection
    if archive.has_duplicate_names() {
        return Err(UmError::new(
            ErrorCode::ArchiveDuplicateEntry,
            "archive contains duplicate entry names",
        ));
    }
    // entry names, sizes and the uncompressed total — all from the actual
    // central directory data
    let mut total: u64 = 0;
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

/// READ_TORN — the container is absent, empty, or cut off mid-write.
///
/// A reader hitting this is almost certainly racing a non-atomic writer
/// (e.g. um-build removes the target and re-zips in place): the failure is
/// transient and a short retry usually succeeds. The `cause` detail keeps
/// the three classes machine-distinguishable; this must never surface as
/// NOTE_NOT_FOUND, which would send agents hunting for a wrong note title.
pub fn torn_error(cause: &str, path: &Path, message: String) -> UmError {
    UmError::new(
        ErrorCode::ReadTorn,
        format!("{}: {}", path.display(), message),
    )
    .with_hint(
        "The container is likely being rewritten by a non-atomic writer. \
         Retry shortly; if it persists, restore the file from a backup.",
    )
    .with_details(serde_json::json!({ "cause": cause }))
}

fn path_error(path: &Path, err: std::io::Error) -> UmError {
    if err.kind() == std::io::ErrorKind::NotFound {
        torn_error(
            "absent",
            path,
            "container file is absent (possibly being rewritten)".to_owned(),
        )
    } else {
        UmError::new(
            ErrorCode::IoError,
            format!("cannot read {}: {err}", path.display()),
        )
    }
}

/// Classify a `stat` failure of a CLI path argument. An absent `.um`/`.note`
/// is the same transient writer window the container readers report
/// (READ_TORN, cause `absent`); anything else keeps the plain path
/// diagnosis. Readers deliberately do NOT consult sidecar locks
/// (`insum_core::lock`): today no writer creates them (um-build is
/// non-cooperative, `apply` is a later stage), a live lease does not make
/// the on-disk state unreadable, and READ_TORN detection covers the
/// millisecond commit window — see lock-protocol.um and the task Result.
pub fn argument_stat_error(path: &Path, err: &std::io::Error) -> UmError {
    if err.kind() == std::io::ErrorKind::NotFound {
        if classify_extension(path).is_some() {
            torn_error(
                "absent",
                path,
                "container file is absent (possibly being rewritten)".to_owned(),
            )
        } else {
            UmError::new(
                ErrorCode::PathNotFound,
                format!("path not found: {}", path.display()),
            )
        }
    } else {
        UmError::new(
            ErrorCode::IoError,
            format!("cannot stat {}: {err}", path.display()),
        )
    }
}

/// Distinguish a torn ZIP from a file that was never a ZIP at all.
///
/// Writers stream ZIPs: local headers (`PK\x03\x04`) come first, the
/// central directory and EOCD (`PK\x05\x06`) last. A file that starts with
/// a ZIP signature but fails to open is a cut-off archive; a file without
/// any ZIP signature was never an archive (NOT_A_ZIP, unchanged semantics).
/// A non-empty `.um` shorter than the 4-byte local-header signature cannot
/// be a legitimate artifact either — it is a write caught in its first
/// bytes.
fn classify_zip_open_error(path: &Path, err: zip::result::ZipError) -> UmError {
    let looks_like_zip_start = match std::fs::File::open(path) {
        Ok(mut file) => {
            let mut head = [0u8; 4];
            match file.read_exact(&mut head) {
                Ok(()) => {
                    head == *b"PK\x03\x04" || head == *b"PK\x01\x02" || head == *b"PK\x05\x06"
                }
                Err(_) => {
                    // 1–3 bytes: shorter than the local-header signature —
                    // never a legitimate artifact, always a cut-off write
                    return torn_error(
                        "truncated",
                        path,
                        format!("ZIP container is incomplete: {err}"),
                    );
                }
            }
        }
        Err(_) => false,
    };
    if looks_like_zip_start {
        torn_error(
            "truncated",
            path,
            format!("ZIP container is incomplete or corrupt: {err}"),
        )
    } else {
        UmError::new(
            ErrorCode::NotAZip,
            format!("file with .um extension is not a valid ZIP archive: {err}"),
        )
        .with_hint("Check the file type or restore a valid UM container.")
    }
}

/// Open a file and fail with READ_TORN when it is absent or empty.
fn open_non_empty(path: &Path) -> Result<(File, u64), UmError> {
    let bytes = std::fs::metadata(path)
        .map_err(|err| path_error(path, err))?
        .len();
    if bytes == 0 {
        return Err(torn_error(
            "empty",
            path,
            "container file is empty (write in progress or interrupted)".to_owned(),
        ));
    }
    let file = File::open(path).map_err(|err| path_error(path, err))?;
    Ok((file, bytes))
}

/// A `.note` document opened for reading (legacy raw doc or envelope).
pub struct NoteFile {
    pub note: NoteDoc,
    /// Envelope metadata — extracted point-wise so the whole
    /// payload is never cloned on the read path.
    pub envelope_format: Option<String>,
    pub features: Option<Value>,
    pub schema: Option<Value>,
    pub path: PathBuf,
    pub file_rev: String,
    /// `note_rev` for `.note` is the hash of the whole file.
    pub note_rev: String,
    pub bytes: u64,
}

impl NoteFile {
    pub fn open(path: &Path, limits: &Limits, with_revs: bool) -> Result<Self, UmError> {
        // Read with a hard cap instead of stat-then-read: a FIFO or a file
        // growing between stat and read cannot inflate the allocation.
        let raw = read_bounded(path, limits)?;
        Self::from_raw(path, &raw, limits, with_revs)
    }

    /// Open like [`NoteFile::open`] and also return the bounded raw bytes
    /// the revisions were computed from. The write path uses this to
    /// rebuild the envelope from the same generation it re-checked — one
    /// bounded read, no unconditional re-read of the file (m-5).
    pub fn open_with_raw(
        path: &Path,
        limits: &Limits,
        with_revs: bool,
    ) -> Result<(Self, Vec<u8>), UmError> {
        let raw = read_bounded(path, limits)?;
        let note_file = Self::from_raw(path, &raw, limits, with_revs)?;
        Ok((note_file, raw))
    }

    /// Parse the bounded bytes [`NoteFile::open`] read.
    fn from_raw(
        path: &Path,
        raw: &[u8],
        limits: &Limits,
        with_revs: bool,
    ) -> Result<Self, UmError> {
        let bytes = raw.len() as u64;
        //: for.note both revisions hash the whole file — one digest
        // serves both; skipped entirely on the light path
        let (file_rev, note_rev) = if with_revs {
            let digest = format_rev(sha256_hex(raw));
            (digest.clone(), digest)
        } else {
            (String::new(), String::new())
        };
        let text = String::from_utf8(raw.to_vec()).map_err(|err| {
            UmError::new(
                ErrorCode::NoteJsonInvalid,
                format!("note is not UTF-8: {err}"),
            )
        })?;
        let value = parse_json_limited(&text, limits.max_depth, ErrorCode::NoteJsonInvalid)?;
        // envelope metadata extracted point-wise (no whole-payload clone)
        let envelope_format = value
            .get("format")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let features = value.get("features").cloned();
        let schema = value.get("schema").cloned();
        let note = NoteDoc::parse(value)?;
        Ok(Self {
            note,
            envelope_format,
            features,
            schema,
            path: path.to_owned(),
            file_rev,
            note_rev,
            bytes,
        })
    }
}

/// Read a `.um` container's manifest without computing `file_rev` and
/// without touching note payloads — the fast path for directory listing
///.
pub fn peek_um_manifest(path: &Path, limits: &Limits) -> Result<(Manifest, Vec<String>), UmError> {
    let (file, _bytes) = open_non_empty(path)?;
    let mut archive = ZipArchive::new(file).map_err(|err| classify_zip_open_error(path, err))?;
    check_archive_safety(&mut archive, limits)?;
    let manifest = Manifest::parse(read_manifest_value(path, &mut archive, limits)?)?;
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
/// per
pub fn classify_extension(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "um" => Some("um"),
        "note" => Some("note"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insum_test_utils::{FixtureNote, ulid_seq, write_legacy_note, write_um};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Wall-clock ticks repeat on this platform granularity — tests running
    /// in parallel need a collision-free scratch directory name.
    fn scratch_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "insum-torn-{}-{}-{}",
            tag,
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixture() -> (PathBuf, Vec<u8>) {
        let dir = scratch_dir("fixture");
        let path = dir.join("fixture.um");
        write_um(
            &path,
            &[
                FixtureNote::title_page(&ulid_seq(1), "Alpha", "first", &[]),
                FixtureNote::plain_page(&ulid_seq(2), "Beta", &[]),
            ],
        )
        .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        (path, bytes)
    }

    fn assert_torn(err: UmError, cause: &str) {
        assert_eq!(err.code, ErrorCode::ReadTorn, "code: {}", err.message);
        assert_eq!(
            err.details
                .as_ref()
                .and_then(|d| d.get("cause"))
                .and_then(|c| c.as_str()),
            Some(cause),
            "cause mismatch: {err:?}"
        );
        assert!(err.hint.is_some(), "READ_TORN carries a hint");
    }

    /// Every truncation of a valid `.um` — including the 1–3 byte stubs —
    /// reports READ_TORN with a cause, never NOTE_NOT_FOUND /
    /// NOTE_FILE_MISSING / MANIFEST_MISSING. (The uncut file opens fine —
    /// covered by healthy_container_still_reads.)
    #[test]
    fn truncated_um_reports_read_torn_at_every_cut() {
        let (path, bytes) = fixture();
        let len = bytes.len();
        for cut in 0..len {
            std::fs::write(&path, &bytes[..cut]).unwrap();
            let limits = Limits::defaults();
            let err = UmContainer::open(&path, &limits).map(|_| ()).unwrap_err();
            let expected_cause = if cut == 0 { "empty" } else { "truncated" };
            assert_torn(err, expected_cause);
            let err = peek_um_manifest(&path, &limits).map(|_| ()).unwrap_err();
            assert_torn(err, expected_cause);
        }
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// A central directory that survived while a payload did not: trash the
    /// deflate stream of the first note entry (0xFF is never a valid block
    /// header), so decompression fails mid-stream — the note read must
    /// report READ_TORN, not a misleading per-note code.
    #[test]
    fn corrupted_payload_reports_read_torn() {
        let (path, bytes) = fixture();
        let entry = format!("notes/{}.json", ulid_seq(1));
        let corrupted = trash_local_payload(&bytes, &entry);
        std::fs::write(&path, &corrupted).unwrap();
        let limits = Limits::defaults();
        let mut container = UmContainer::open(&path, &limits).unwrap();
        let note = container.notes_in_order()[0].clone();
        let err = container.read_note(&note, &limits).unwrap_err();
        assert_torn(err, "truncated");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// Locate the named entry's local header and overwrite the first bytes
    /// of its compressed data with 0xFF.
    fn trash_local_payload(bytes: &[u8], entry: &str) -> Vec<u8> {
        let mut i = 0;
        while let Some(pos) = find(&bytes[i..], b"PK\x03\x04") {
            let header = i + pos;
            let name_len = u16::from_le_bytes([bytes[header + 26], bytes[header + 27]]) as usize;
            let extra_len = u16::from_le_bytes([bytes[header + 28], bytes[header + 29]]) as usize;
            let name = String::from_utf8_lossy(&bytes[header + 30..header + 30 + name_len]);
            if name == entry {
                let data = header + 30 + name_len + extra_len;
                let mut out = bytes.to_vec();
                for offset in 2..18 {
                    out[data + offset] = 0xFF;
                }
                return out;
            }
            i = header + 4;
        }
        panic!("local header for {entry} not found in fixture");
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    #[test]
    fn absent_and_empty_report_read_torn() {
        let dir = scratch_dir("absent");
        let limits = Limits::defaults();

        let absent = dir.join("missing.um");
        assert_torn(
            UmContainer::open(&absent, &limits).map(|_| ()).unwrap_err(),
            "absent",
        );
        assert_torn(
            peek_um_manifest(&absent, &limits).map(|_| ()).unwrap_err(),
            "absent",
        );

        let empty = dir.join("empty.um");
        std::fs::write(&empty, b"").unwrap();
        assert_torn(
            UmContainer::open(&empty, &limits).map(|_| ()).unwrap_err(),
            "empty",
        );

        let absent_note = dir.join("missing.note");
        assert_torn(
            NoteFile::open(&absent_note, &limits, true)
                .map(|_| ())
                .unwrap_err(),
            "absent",
        );

        let empty_note = dir.join("empty.note");
        std::fs::write(&empty_note, b"").unwrap();
        assert_torn(
            NoteFile::open(&empty_note, &limits, true)
                .map(|_| ())
                .unwrap_err(),
            "empty",
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A file that was never a ZIP keeps NOT_A_ZIP — the new code must not
    /// swallow the "wrong file type" diagnosis.
    #[test]
    fn not_a_zip_is_preserved() {
        let dir = scratch_dir("notzip");
        let path = dir.join("texty.um");
        std::fs::write(
            &path,
            b"# this is a markdown file, definitely not a zip archive\n".repeat(4),
        )
        .unwrap();
        let limits = Limits::defaults();
        let err = UmContainer::open(&path, &limits).map(|_| ()).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotAZip);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A manifest that is valid ZIP but not UTF-8 keeps MANIFEST_INVALID —
    /// READ_TORN must stay scoped to container-integrity failures, not
    /// swallow the content diagnosis (a permanent error must not advise a
    /// retry).
    #[test]
    fn manifest_encoding_error_is_preserved() {
        let (path, bytes) = fixture();
        // corrupt inside the manifest payload: find its local header and
        // flip bytes of the stored (deflate) stream — decompression may
        // still succeed; the honest check is a valid zip with a non-UTF-8
        // manifest entry, so build one directly
        let dir = path.parent().unwrap().to_path_buf();
        let bad = dir.join("bad-manifest.um");
        {
            use std::io::Write;
            let file = std::fs::File::create(&bad).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .last_modified_time(zip::DateTime::default());
            writer.start_file("manifest.json", options).unwrap();
            writer
                .write_all(&[0x7B, 0x22, 0x66, 0xFF, 0xFE, 0x22, 0x3A, 0x31, 0x7D])
                .unwrap();
            writer.finish().unwrap();
        }
        let limits = Limits::defaults();
        let err = UmContainer::open(&bad, &limits).map(|_| ()).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::ManifestInvalid,
            "not READ_TORN: {err:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
        let _ = bytes;
    }

    /// A valid container still opens and its note payload still parses —
    /// the resilience rework must not disturb the healthy path.
    #[test]
    fn healthy_container_still_reads() {
        let (path, _bytes) = fixture();
        let limits = Limits::defaults();
        let mut container = UmContainer::open(&path, &limits).unwrap();
        let note = container.notes_in_order()[0].clone();
        container.read_note(&note, &limits).unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// `.note` corruption (not UTF-8) keeps NOTE_JSON_INVALID — only the
    /// container-integrity classes move to READ_TORN.
    #[test]
    fn note_encoding_error_is_preserved() {
        let dir = scratch_dir("noteutf");
        let path = dir.join("bad.note");
        std::fs::write(&path, [0xFF, 0xFE, 0x00, 0x01]).unwrap();
        let limits = Limits::defaults();
        let err = NoteFile::open(&path, &limits, true)
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NoteJsonInvalid);
        let legacy = dir.join("legacy.note");
        write_legacy_note(
            &legacy,
            &serde_json::json!({ "type": "noteDoc", "content": [] }),
        )
        .unwrap();
        NoteFile::open(&legacy, &limits, true).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
