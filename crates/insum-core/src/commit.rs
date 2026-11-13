//! Shared write-path plumbing for the transactional commands
//! (`migrate-ids`, `apply`): source-safety checks, the hidden staged file,
//! the pre-replace verification and the raw-copy container rebuild
//! (§9.3, §12). Business logic stays in the command modules; everything
//! here is the common commit discipline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use zip::write::{PreparedZipFile, SimpleFileOptions};

use crate::container::{UmContainer, format_rev, hex};
use crate::error::{ErrorCode, UmError};

/// §9.3 MUST: the write path refuses a symlink source and any non-regular
/// source — the link is never followed to its target and never silently
/// replaced by a regular file. Runs before any other work, so dry-run and
/// commit refuse alike (plan == commit).
pub(crate) fn ensure_writable_source(path: &Path) -> Result<(), UmError> {
    let meta = std::fs::symlink_metadata(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            crate::container::torn_error(
                "absent",
                path,
                "the file is absent (possibly being rewritten)".to_owned(),
            )
        } else {
            UmError::new(
                ErrorCode::IoError,
                format!("cannot stat {}: {err}", path.display()),
            )
        }
    })?;
    if meta.file_type().is_symlink() {
        return Err(UmError::new(
            ErrorCode::IoError,
            format!(
                "{} is a symlink; the write path refuses to follow or replace it",
                path.display()
            ),
        )
        .with_hint(
            "Run the write command on the regular file the link points to — replacing the link would move the note path onto a regular file.",
        ));
    }
    if !meta.is_file() {
        return Err(UmError::new(
            ErrorCode::IoError,
            format!(
                "{} is not a regular file; the write path refuses it",
                path.display()
            ),
        )
        .with_hint("The write commands maintain regular .um/.note files only."));
    }
    Ok(())
}

/// §9.3 MUST: immediately before the rename — re-verify that the source is
/// still a regular file (never a symlink) and that its revision is
/// unchanged, and take the permissions the replacement must keep (§9.3:
/// preserve the permissions of the source).
pub(crate) fn pre_replace_verification(
    path: &Path,
    expected_rev: &str,
    what: &str,
) -> Result<u32, UmError> {
    let meta = std::fs::symlink_metadata(path).map_err(|err| {
        // the same disappearing-source window must classify the same way no
        // matter which check of this phase hits it first (D-7): an absent
        // source is the torn-write class — exactly what ensure_writable_source
        // and the readers report — never a plain IO_ERROR
        if err.kind() == std::io::ErrorKind::NotFound {
            crate::container::torn_error(
                "absent",
                path,
                "the file is absent (possibly being rewritten)".to_owned(),
            )
        } else {
            UmError::new(ErrorCode::IoError, format!("cannot stat {path:?}: {err}"))
        }
    })?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(UmError::new(
            ErrorCode::IoError,
            format!(
                "{} stopped being a regular file before the replace; the write refuses",
                path.display()
            ),
        )
        .with_hint(
            "Remove the symlink or fix the directory layout, then re-run the write command.",
        ));
    }
    let rev = crate::container::file_sha256(path)?;
    if rev != expected_rev {
        return Err(revision_mismatch(expected_rev, &rev, what));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(meta.permissions().mode())
    }
    #[cfg(not(unix))]
    {
        Ok(0)
    }
}

/// A revision guard refusal (§11 `NOTE_REVISION_MISMATCH`): the base the
/// caller built its plan on is no longer the current state.
pub(crate) fn revision_mismatch(expected: &str, actual: &str, what: &str) -> UmError {
    UmError::new(
        ErrorCode::NoteRevisionMismatch,
        format!("{what} changed after the plan was built"),
    )
    .with_hint("Re-read the note (`insum tree` or `insum block get`) and rebuild the operation package from the current revision.")
    .with_details(serde_json::json!({ "expected": expected, "actual": actual }))
}

/// A hidden, exclusively created temp file next to the target (§12: temp
/// artifacts are hidden and live beside the original). The name carries a
/// random suffix and the file is created with `create_new` — a planted
/// symlink or a pre-existing file is never followed or reused (M-4/M-5);
/// on unix it is created 0600. A target whose name cannot carry the
/// name-based suffix (file-name component caps) is staged under the
/// name-independent hidden prefix `.insum-migrate-tmp-<hex>` in the same
/// directory (D-6) — the same guarantees, none of the length. The file is
/// unlinked on drop — on every error path and on panic unwinding (m-2) —
/// unless it became the target through a successful rename.
pub(crate) struct StagedFile {
    path: PathBuf,
    armed: bool,
}

impl StagedFile {
    pub(crate) fn create(target: &Path) -> Result<(Self, std::fs::File), UmError> {
        let parent = target.parent().unwrap_or(Path::new("."));
        let name = target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "insum".to_owned());
        // D-6: the name-based component (".", the target name,
        // ".migrate-tmp-" and 16 hex digits) is 30 bytes longer than the
        // target's own name, and file-name components are length-capped — a
        // long target failed the create with ENAMETOOLONG while the dry-run
        // was green. Such a name (and any create failure that is not a
        // collision) falls back to the name-independent hidden prefix in the
        // SAME directory, with the same guarantees: create_new, 0600, never
        // following a planted name.
        const COMPONENT_CAP: usize = 255;
        const FALLBACK_PREFIX: &str = ".insum-migrate-tmp";
        let named_prefix = format!(".{name}.migrate-tmp");
        // 17 = "-" + 16 hex digits of the random suffix
        let mut prefix = if named_prefix.len() + 17 > COMPONENT_CAP {
            FALLBACK_PREFIX.to_owned()
        } else {
            named_prefix
        };
        let mut using_fallback = prefix == FALLBACK_PREFIX;
        use rand::Rng;
        for _ in 0..64 {
            let suffix: u64 = rand::rng().random();
            let candidate = parent.join(format!("{prefix}-{suffix:016x}"));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600); // private until the rename (§12)
            }
            match options.open(&candidate) {
                Ok(file) => {
                    return Ok((
                        Self {
                            path: candidate,
                            armed: true,
                        },
                        file,
                    ));
                }
                // the name exists (or a planted symlink collides): pick
                // another random suffix, never follow what is there
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    if !using_fallback {
                        // the name-based component can exceed a file-name
                        // limit even when the target itself fits (D-6) —
                        // retry once with the name-independent prefix
                        using_fallback = true;
                        prefix = FALLBACK_PREFIX.to_owned();
                        continue;
                    }
                    return Err(UmError::new(
                        ErrorCode::IoError,
                        format!(
                            "cannot create the temp file next to {}: {err}",
                            target.display()
                        ),
                    ));
                }
            }
        }
        Err(UmError::new(
            ErrorCode::IoError,
            "cannot create a unique temp file next to the target".to_owned(),
        ))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Preserve the source permissions (§9.3, unix mode), atomically rename
    /// the staged file over the target, then fsync the parent directory
    /// (best-effort, where the OS supports it — §9.3).
    pub(crate) fn commit_to(mut self, target: &Path, mode: u32) -> Result<(), UmError> {
        // Permissions are a unix concept (§9.3); on other platforms the mode
        // argument is intentionally not applied.
        #[cfg(not(unix))]
        let _ = mode;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(mode & 0o7777))
                .map_err(|err| {
                    UmError::new(
                        ErrorCode::IoError,
                        format!("cannot preserve the file permissions: {err}"),
                    )
                })?;
        }
        std::fs::rename(&self.path, target).map_err(|err| {
            UmError::new(
                ErrorCode::IoError,
                format!("cannot replace the file atomically: {err}"),
            )
        })?;
        self.armed = false;
        #[cfg(unix)]
        {
            if let Some(parent) = target.parent()
                && let Ok(dir) = std::fs::File::open(parent)
            {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Tee-writer over the staged file: every written byte also feeds a
/// SHA-256, so the whole-file revision falls out of the streaming write
/// pass instead of a second full pass over the staged bytes (PRF-3).
/// Seek is forwarded honestly; the rebuild below only ever appends, so
/// the digest tracks the exact file content.
struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
}

impl<W> HashingWriter<W>
where
    W: std::io::Write + std::io::Seek,
{
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }
}

impl<W: std::io::Write> std::io::Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.hasher.update(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl<W: std::io::Write + std::io::Seek> std::io::Seek for HashingWriter<W> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Rebuild the `.um` container with the given payload replacements: every
/// replaced entry is re-deflated under the same name and declared mtime,
/// every other entry is copied raw (byte-identical, order and timestamps
/// preserved). The archive is the one held open since the recheck — raw
/// entries are copied from the in-lock generation, never from a fresh open
/// of the path (M-1: a non-cooperative writer cannot mix generations into
/// the commit). The result is streamed into the staged file next to the
/// target (m-17: no whole-container buffer, no extra staging copy) and
/// fsynced there (§9.3). Returns the whole-file revision taken from the
/// tee-hash of the streamed bytes (PRF-3) — identical to a fresh
/// `file_sha256` over the staged file, without re-reading it.
///
/// WHY prepared entries: `ZipWriter::start_file` patches the local header
/// in place once the compressed size is known (seek-back + rewrite), which
/// a linear tee cannot observe correctly. `add_prepared_file` writes the
/// header complete from the pre-compressed entry, so every write of this
/// rebuild is append-only and the tee digest is exact.
pub(crate) fn rewrite_um_entries(
    container: &mut UmContainer,
    replacements: &HashMap<String, Vec<u8>>,
    into: std::fs::File,
) -> Result<String, UmError> {
    // rebuild: walk the source archive in order through the held handle
    let archive = container.zip_archive();
    let mut writer = zip::ZipWriter::new(HashingWriter::new(into));
    for index in 0..archive.len() {
        let (name, last_modified) = {
            let raw = archive.by_index_raw(index).map_err(|err| {
                UmError::new(
                    ErrorCode::ArchiveEntryNameInvalid,
                    format!("cannot read entry {index}: {err}"),
                )
            })?;
            let name = raw
                .name()
                .map_err(|err| {
                    UmError::new(
                        ErrorCode::ArchiveEntryNameInvalid,
                        format!("cannot decode entry {index} name: {err}"),
                    )
                })?
                .into_owned();
            (name, raw.last_modified().unwrap_or_default())
        };
        let name_str: &str = name.as_ref();
        if let Some(bytes) = replacements.get(name_str) {
            // replace the entry: same name, same declared mtime, fresh deflate
            let options = SimpleFileOptions::default().last_modified_time(last_modified);
            let prepared = PreparedZipFile::from_data(name_str, options, bytes).map_err(|err| {
                UmError::internal(format!("cannot compress entry {name:?}: {err}"))
            })?;
            writer
                .add_prepared_file(prepared)
                .map_err(|err| UmError::internal(format!("cannot write entry {name:?}: {err}")))?;
        } else {
            // untouched entry: raw copy keeps the compressed bytes verbatim
            let raw = archive.by_index_raw(index).map_err(|err| {
                UmError::new(
                    ErrorCode::ArchiveEntryNameInvalid,
                    format!("cannot read entry {index}: {err}"),
                )
            })?;
            writer
                .raw_copy_file(raw)
                .map_err(|err| UmError::internal(format!("cannot copy entry {name:?}: {err}")))?;
        }
    }
    let hashed = writer
        .finish()
        .map_err(|err| UmError::internal(format!("cannot finish the new container: {err}")))?;
    let digest = hex(&hashed.hasher.finalize());
    // §9.3: fsync the temp file before it can become the target; any error
    // here leaves the staged file to the Drop guard for cleanup (m-2)
    hashed.inner.sync_all().map_err(|err| {
        UmError::new(
            ErrorCode::IoError,
            format!("cannot sync the staged file: {err}"),
        )
    })?;
    Ok(format_rev(digest))
}

/// Serialize one JSON document for a note payload (compact — the payload
/// form the read path re-parses).
pub(crate) fn payload_bytes(doc: &Value) -> Result<Vec<u8>, UmError> {
    serde_json::to_vec(doc)
        .map_err(|err| UmError::internal(format!("note payload does not serialize: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{UmContainer, file_sha256};
    use crate::limits::Limits;

    /// PRF-3 invariant: the tee digest collected while streaming the staged
    /// container equals a fresh full hash over those very bytes — for a
    /// rewrite that mixes a re-deflated entry with raw copies.
    #[test]
    fn rewrite_digest_matches_the_staged_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.um");
        insum_test_utils::write_um(
            &path,
            &[
                insum_test_utils::FixtureNote::title_page(
                    &insum_test_utils::ulid_seq(1),
                    "Alpha",
                    "first",
                    &[],
                ),
                insum_test_utils::FixtureNote::plain_page(
                    &insum_test_utils::ulid_seq(2),
                    "Beta",
                    &[],
                ),
            ],
        )
        .unwrap();
        let limits = Limits::defaults();
        let mut container = UmContainer::open(&path, &limits).unwrap();

        // replace the first note's payload (exercises the prepared-entry
        // path); every other entry goes through the raw-copy path
        let descriptor = container
            .find_note(&insum_test_utils::ulid_seq(1))
            .unwrap()
            .clone();
        let mut replacements = HashMap::new();
        let mut doc = container.read_note(&descriptor, &limits).unwrap().doc;
        doc["content"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "type": "paragraph",
                "attrs": {"id": "replaced1"},
                "content": [{"type": "text", "text": "rewritten"}]
            }));
        replacements.insert(descriptor.path.clone(), payload_bytes(&doc).unwrap());

        let (staged, file) = StagedFile::create(&path).unwrap();
        let rev = rewrite_um_entries(&mut container, &replacements, file).unwrap();

        // the tee digest must equal a re-read of the staged bytes...
        assert_eq!(rev, file_sha256(staged.path()).unwrap());
        // ...and the staged container must be a readable, valid rebuild
        let mut staged_container = UmContainer::open(staged.path(), &limits).unwrap();
        let staged_descriptor = staged_container
            .find_note_by_path(&descriptor.path)
            .unwrap()
            .clone();
        let staged_note = staged_container
            .read_note(&staged_descriptor, &limits)
            .unwrap();
        assert_eq!(
            staged_note.doc["content"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["attrs"]["id"],
            "replaced1",
            "the replaced payload is in the staged container"
        );
    }
}
