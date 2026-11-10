//! `insum history` — a readable change history of one `.um`/`.note` file
//! from the git history of the repository containing it (spec §13.1 of the
//! history task).
//!
//! Read-only by construction: `git log` lists the revisions that touched
//! the file, `git show <sha>:<path>` extracts revision blobs; neither the
//! working tree nor the index nor HEAD is touched. Revisions arrive
//! newest-first; each carries a structural per-note diff against its
//! parent revision — blocks are matched positionally by JSON Pointer
//! inside one note (path in the archive + pointer), with `attrs.id` used
//! when present and never required. This positional matching is the
//! documented prototype limitation: moved blocks show up as remove+add.
//!
//! The git binary must be in PATH (documented in the user docs); blobs are
//! bounded by the same limits as regular reads.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use serde_json::{Value, json};

use crate::error::{ErrorCode, UmError};
use crate::limits::Limits;
use crate::model::Manifest;

/// Upper bound of `--limit` (the safety ceiling; the default is smaller).
pub const MAX_REVISIONS: usize = 200;

/// The default number of revisions returned.
pub const DEFAULT_REVISIONS: usize = 20;

/// Context preview of a changed block.
const PREVIEW_CHARS: usize = 120;

/// The notes of one parsed revision blob: (id, title, document).
type RevisionNotes = Vec<(String, Option<String>, Value)>;

#[derive(Debug, Clone, Serialize)]
pub struct Revision {
    /// Hex object name (SHA-1 40 or SHA-256 64 chars) — validated before
    /// it ever reaches a git argument.
    pub sha: String,
    /// Parent object names, first parent first (merge-aware diffs).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<String>,
    /// Commit time, epoch seconds (from git — no local clock).
    pub timestamp: i64,
    pub author: String,
    pub subject: String,
}

/// A commit sha must be plain hex of a known digest length: anything else
/// from the log stream is not an object name (a commit subject can carry
/// the field separators) and must never reach `git show` as an argument.
fn is_valid_sha(candidate: &str) -> bool {
    (candidate.len() == 40 || candidate.len() == 64)
        && candidate.bytes().all(|b| b.is_ascii_hexdigit())
}

/// One block-level change inside a note diff.
#[derive(Debug, Clone, Serialize)]
pub struct BlockChange {
    pub pointer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub block_type: String,
    /// First characters of the block's own inline text, for orientation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// The structural diff of one note between two revisions.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NoteDiff {
    pub note_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_title: Option<String>,
    /// The title the note had in the PARENT revision — a rename revision
    /// is findable by either name (C-06).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_title_old: Option<String>,
    #[serde(skip_serializing_if = "not")]
    pub added: bool,
    #[serde(skip_serializing_if = "not")]
    pub removed: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocks_added: Vec<BlockChange>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocks_removed: Vec<BlockChange>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocks_changed: Vec<BlockChange>,
}

fn not(value: &bool) -> bool {
    !value
}

impl NoteDiff {
    fn is_empty(&self) -> bool {
        !self.added
            && !self.removed
            && self.blocks_added.is_empty()
            && self.blocks_removed.is_empty()
            && self.blocks_changed.is_empty()
    }
}

/// One revision with its per-note diff against the parent revision.
#[derive(Debug, Clone, Serialize)]
pub struct RevisionDiff {
    #[serde(flatten)]
    pub revision: Revision,
    /// Notes whose payload changed in this revision (filter applied).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<NoteDiff>,
    /// The blob could not be parsed (corrupt/unsupported history state) —
    /// the revision is listed, the diff is not judged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<Value>,
}

/// The collected history of one file.
#[derive(Debug, Serialize)]
pub struct History {
    /// The git worktree root the file belongs to.
    pub repo_root: String,
    /// The file path relative to the repo root, slash-separated.
    pub file_in_repo: String,
    /// Revisions newest-first.
    pub revisions: Vec<RevisionDiff>,
    pub complete: bool,
    pub warnings: Vec<Value>,
}

fn warning(code: &str, message: String) -> Value {
    serde_json::json!({ "code": code, "message": message })
}

/// Run git in `dir` and capture stdout; a missing binary is a structured
/// IO error with a hint, a git-level failure is returned as raw stderr.
/// The environment is scrubbed of GIT_* variables: an inherited GIT_DIR
/// (hooks, CI wrappers) must not redirect the history read to a foreign
/// repository.
/// Upper bound for git text output (`git log` streams; a forged commit
/// subject can be arbitrarily large).
const GIT_STDOUT_MAX: usize = 8 * 1024 * 1024;

/// Run git reading stdout through a hard `take` bound: an oversized stream
/// is LIMIT_EXCEEDED with the writer killed — never a whole-stream
/// allocation, never a wait on a pipe nobody drains. stderr is discarded
/// (errors carry the exit class; git stderr is attacker-influenced text).
fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, UmError> {
    let mut child = git_command(dir, args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                UmError::new(
                    ErrorCode::IoError,
                    "git binary not found in PATH".to_owned(),
                )
                .with_hint(
                    "insum history reads the git history of the file; install git or add it to PATH.",
                )
            } else {
                UmError::from(err)
            }
        })?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut bytes = Vec::new();
    std::io::Read::take(&mut stdout, GIT_STDOUT_MAX as u64)
        .read_to_end(&mut bytes)
        .map_err(UmError::from)?;
    drop(stdout); // close the pipe: a writer blocked on it can finish
    let oversized = bytes.len() >= GIT_STDOUT_MAX;
    if oversized {
        let _ = child.kill();
    }
    let status = child.wait().map_err(UmError::from)?;
    if oversized {
        return Err(UmError::new(
            ErrorCode::LimitExceeded,
            format!(
                "git {} output exceeds {} bytes",
                args.first().unwrap_or(&"git"),
                GIT_STDOUT_MAX
            ),
        ));
    }
    if !status.success() {
        return Err(UmError::new(
            ErrorCode::IoError,
            format!(
                "git {} failed (exit {})",
                args.join(" "),
                status.code().unwrap_or(-1)
            ),
        ));
    }
    Ok(bytes)
}

/// A scrubbed git invocation: GIT_* removed, PATH/HOME kept (git needs
/// them to find the binary and the system config).
fn git_command(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args);
    command.env_clear();
    for (key, value) in std::env::vars_os() {
        // Windows variables are matched case-insensitively ("Path" is the
        // same variable as "PATH")
        let upper = key.to_string_lossy().to_ascii_uppercase();
        if matches!(
            upper.as_str(),
            "PATH" | "HOME" | "SYSTEMROOT" | "COMSPEC" | "SYSTEMDRIVE" | "TMP" | "TEMP"
        ) {
            command.env(key, value);
        }
    }
    command
}

struct RepoLocation {
    root: PathBuf,
    file_in_repo: String,
}

/// Locate the repository and the file path inside it; `Ok(None)` — the
/// file is not inside a work tree (the caller answers with a warning).
/// The file is resolved against the current directory BEFORE any
/// decomposition (G-01 — a bare name from a repo subdirectory belongs to
/// that subdirectory), and git probes run from the nearest EXISTING
/// ancestor directory (G-02 — the file or its directory may be deleted
/// together with the directory); the peeled components are re-attached to
/// the probe directory's position inside the repository.
fn locate_repo(file: &Path) -> Result<Option<RepoLocation>, UmError> {
    let file = std::path::absolute(file).map_err(UmError::from)?;

    // walk up to the nearest existing directory, remembering what was
    // peeled (deepest first)
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut probe = file.clone();
    while !probe.is_dir() {
        match (probe.file_name(), probe.parent()) {
            (Some(name), Some(parent)) if !name.is_empty() => {
                tail.push(name.to_os_string());
                probe = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let probe_dir = if probe.is_dir() {
        probe.clone()
    } else {
        // the filesystem root: no existing ancestor found
        probe
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| probe.clone())
    };

    // inside a work tree? `rev-parse --is-inside-work-tree` answers
    // "true"/"false" with exit 0 (bare → false) and fails outside any
    // repository — every non-"true" outcome means "no vault" here
    let inside = match git(&probe_dir, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(out) => out,
        // a git-level failure means "no work tree" — expected outcome; a
        // spawn failure (git missing from PATH) is a real IO error and
        // must not masquerade as an empty history (D-04)
        Err(err) if err.message.contains("git binary not found") => return Err(err),
        Err(_) => return Ok(None),
    };
    if String::from_utf8_lossy(&inside).trim() != "true" {
        return Ok(None);
    }
    let root_bytes = git(&probe_dir, &["rev-parse", "--show-toplevel"])?;
    let root_text = String::from_utf8_lossy(&root_bytes).trim().to_owned();
    let root = PathBuf::from(root_text);

    // the probe directory exists — canonicalizing it resolves symlinked
    // prefixes (/var → /private/var)
    let probe_canonical = probe_dir.canonicalize().map_err(UmError::from)?;
    let canonical_root = root.canonicalize().map_err(UmError::from)?;
    let mut relative = probe_canonical
        .strip_prefix(&canonical_root)
        .map_err(|_| {
            UmError::new(
                ErrorCode::IoError,
                format!(
                    "file {} is outside the repository at {}",
                    file.display(),
                    canonical_root.display()
                ),
            )
        })?
        .to_path_buf();
    // re-attach the peeled components (stored deepest first)
    for name in tail.iter().rev() {
        relative = relative.join(name);
    }
    let file_in_repo = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Ok(Some(RepoLocation { root, file_in_repo }))
}

/// List the revisions that touched the file, newest-first, with their
/// parent links. `--full-history` keeps every commit that touched the file
/// on side branches; a record with a non-hex "sha" (a commit subject can
/// carry the field separators) is skipped and reported via the returned
/// skipped count.
fn list_revisions(repo: &RepoLocation, limit: usize) -> Result<(Vec<Revision>, usize), UmError> {
    let format = "%H%x1f%P%x1f%at%x1f%an%x1f%s%x1e";
    let out = git(
        &repo.root,
        &[
            "log",
            "--full-history",
            &format!("--max-count={limit}"),
            &format!("--format={format}"),
            "--",
            // :(literal) — a file name with glob metacharacters must not
            // widen the pathspec to foreign commits (S-04)
            &format!(":(literal){}", repo.file_in_repo),
        ],
    )?;
    let text = String::from_utf8_lossy(&out);
    let mut revisions = Vec::new();
    let mut skipped = 0usize;
    for record in text.split('\u{1e}') {
        let record = record.trim_start_matches(['\n', '\r']).trim();
        if record.is_empty() {
            continue;
        }
        let mut fields = record.split('\u{1f}');
        let sha = fields.next().unwrap_or("").trim().to_owned();
        let parents = fields
            .next()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let timestamp = fields
            .next()
            .unwrap_or("")
            .trim()
            .parse::<i64>()
            .unwrap_or(0);
        let author = fields.next().unwrap_or("").trim().to_owned();
        let subject = fields.next().unwrap_or("").trim().to_owned();
        // every object name that will reach a git argument must be hex:
        // the sha AND the parents (a forged subject can carry separators)
        if !is_valid_sha(&sha) || parents.iter().any(|parent| !is_valid_sha(parent)) {
            skipped += 1;
            continue;
        }
        revisions.push(Revision {
            sha,
            parents,
            timestamp,
            author,
            subject,
        });
    }
    Ok((revisions, skipped))
}

/// Does `<sha>:<path>` exist in the git object database? `git cat-file
/// -e` exits 0 for present, 1 for absent; any other exit is a real
/// failure (F-01 — "path not in that tree" must never surface as
/// IO_ERROR from `git show`).
fn path_in_tree(repo: &RepoLocation, sha: &str) -> Result<bool, UmError> {
    // G-07: `git ls-tree` exits 0 with EMPTY output when the path is
    // absent from the tree and fails only on repository-level problems —
    // the "path absent" and "repo fatal" cases are cleanly separated
    let out = git(&repo.root, &["ls-tree", sha, "--", &repo.file_in_repo])?;
    Ok(!out.is_empty())
}

/// Extract the blob of one revision: `git show <sha>:<path>`. The stream
/// is read through a `take` bound so an oversized blob fails with
/// LIMIT_EXCEEDED instead of allocating it whole.
fn revision_blob(repo: &RepoLocation, sha: &str, limits: &Limits) -> Result<Vec<u8>, UmError> {
    let object = format!("{sha}:{}", repo.file_in_repo);
    let mut child = git_command(&repo.root, &["show", &object])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null()) // drained nowhere → no hang
        .spawn()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                UmError::new(ErrorCode::IoError, "git binary not found in PATH".to_owned())
                    .with_hint(
                        "insum history reads the git history of the file; install git or add it to PATH.",
                    )
            } else {
                UmError::from(err)
            }
        })?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let limit = limits.max_entry_bytes + 1;
    let mut bytes = Vec::new();
    std::io::Read::take(&mut stdout, limit as u64)
        .read_to_end(&mut bytes)
        .map_err(UmError::from)?;
    drop(stdout); // close the pipe: a writer blocked on it can finish
    let oversized = bytes.len() > limits.max_entry_bytes;
    if oversized {
        // the writer is still feeding a blob nobody reads — stop it
        // instead of waiting forever in wait()
        let _ = child.kill();
    }
    let status = child.wait().map_err(UmError::from)?;
    if !status.success() && !oversized {
        return Err(UmError::new(
            ErrorCode::IoError,
            format!("git show {object} failed"),
        ));
    }
    if oversized {
        return Err(UmError::new(
            ErrorCode::LimitExceeded,
            format!(
                "revision blob larger than UM_MAX_ENTRY_BYTES ({})",
                limits.max_entry_bytes
            ),
        ));
    }
    Ok(bytes)
}

/// Read one ZIP entry bounded by the ACTUAL decompressed bytes (the
/// header size is untrusted): more than the limit is LIMIT_EXCEEDED; a
/// missing entry is `Ok(None)`.
fn read_zip_entry(
    archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    name: &str,
    limits: &Limits,
) -> Result<Option<Vec<u8>>, UmError> {
    let entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(_) => return Ok(None),
    };
    let mut bytes = Vec::new();
    entry
        .take((limits.max_entry_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(UmError::from)?;
    if bytes.len() > limits.max_entry_bytes {
        return Err(UmError::new(
            ErrorCode::LimitExceeded,
            format!(
                "entry {name:?} larger than UM_MAX_ENTRY_BYTES ({})",
                limits.max_entry_bytes
            ),
        ));
    }
    Ok(Some(bytes))
}

/// The notes of one revision blob: id → (title, document JSON).
/// `Ok(None)` — the blob is not a parseable .um/.note (listed with a
/// warning, not a crash); bytes before the ZIP/JSON parsing are already
/// bounded by the limits.
fn revision_notes(
    blob: &[u8],
    extension: &str,
    limits: &Limits,
) -> Result<Option<RevisionNotes>, UmError> {
    if extension == "note" {
        let doc: Value = match serde_json::from_slice(blob) {
            Ok(doc) => doc,
            Err(_) => return Ok(None),
        };
        let title = doc
            .get("content")
            .and_then(Value::as_array)
            .and_then(|children| {
                children
                    .iter()
                    .find(|child| child.get("type").and_then(Value::as_str) == Some("noteTitle"))
                    .and_then(note_title_text)
            });
        return Ok(Some(vec![("note".to_owned(), title, doc)]));
    }
    // .um: a ZIP container — parse in memory
    let reader = std::io::Cursor::new(blob);
    let mut archive = match zip::ZipArchive::new(reader) {
        Ok(archive) => archive,
        Err(_) => return Ok(None),
    };
    if archive.len() > limits.max_archive_entries {
        return Err(UmError::new(
            ErrorCode::LimitExceeded,
            format!(
                "revision container holds more than UM_MAX_ARCHIVE_ENTRIES ({}) entries",
                limits.max_archive_entries
            ),
        ));
    }
    let manifest_bytes = read_zip_entry(&mut archive, "manifest.json", limits)?;
    let manifest_bytes = match manifest_bytes {
        Some(bytes) => bytes,
        None => return Ok(None),
    };
    let manifest: Value = match serde_json::from_slice(&manifest_bytes) {
        Ok(manifest) => manifest,
        Err(_) => return Ok(None),
    };
    let manifest = match Manifest::parse(manifest) {
        Ok(manifest) => manifest,
        Err(_) => return Ok(None),
    };
    let mut notes = Vec::new();
    for descriptor in &manifest.notes {
        let payload = match read_zip_entry(&mut archive, &descriptor.path, limits)? {
            Some(bytes) => bytes,
            None => return Ok(None), // inconsistent history blob
        };
        let doc: Value = match serde_json::from_slice(&payload) {
            Ok(doc) => doc,
            Err(_) => return Ok(None),
        };
        notes.push((descriptor.id.clone(), descriptor.title.clone(), doc));
    }
    Ok(Some(notes))
}

/// The text of the first noteTitle child (the .note title source).
fn note_title_text(title_node: &Value) -> Option<String> {
    fn direct_text(node: &Value, out: &mut String) {
        match node {
            Value::String(text) => out.push_str(text),
            Value::Array(items) => items.iter().for_each(|item| direct_text(item, out)),
            Value::Object(map) => {
                if map.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = map.get("text").and_then(Value::as_str) {
                        out.push_str(text);
                    }
                    return;
                }
                for value in map.values() {
                    match value {
                        Value::Array(_) | Value::Object(_) => direct_text(value, out),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    if let Some(content) = title_node.get("content") {
        direct_text(content, &mut out);
    }
    let trimmed = out.trim().to_owned();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn block_id(node: &Value) -> Option<String> {
    node.get("attrs")
        .and_then(|attrs| attrs.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn block_type(node: &Value) -> String {
    node.get("type")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_owned()
}

/// First inline text of the block's own content, bounded preview.
fn block_text(node: &Value) -> Option<String> {
    fn direct_text(node: &Value, out: &mut String) {
        match node {
            Value::String(text) => {
                if out.chars().count() < PREVIEW_CHARS {
                    out.push_str(text);
                }
            }
            Value::Array(items) => {
                for item in items {
                    direct_text(item, out);
                }
            }
            Value::Object(map) => {
                if map.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = map.get("text").and_then(Value::as_str)
                        && out.chars().count() < PREVIEW_CHARS
                    {
                        out.push_str(text);
                    }
                    return; // text nodes own their text; do not descend
                }
                for (_, value) in map {
                    match value {
                        Value::Array(_) | Value::Object(_) => direct_text(value, out),
                        _ => {}
                    }
                    if out.chars().count() >= PREVIEW_CHARS {
                        return;
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    direct_text(node, &mut out);
    let cut: String = out.chars().take(PREVIEW_CHARS).collect();
    if cut.is_empty() {
        None
    } else if cut.chars().count() < out.chars().count() {
        Some(format!("{cut}…"))
    } else {
        Some(cut)
    }
}

fn block_change(node: &Value, pointer: &str) -> BlockChange {
    BlockChange {
        pointer: pointer.to_owned(),
        id: block_id(node),
        block_type: block_type(node),
        text: block_text(node),
    }
}

/// A textblock (paragraph, heading, title, ...): its content is inline —
/// a change inside it is a change of the BLOCK, not of inner nodes.
fn is_textblock(node: &Value) -> bool {
    let Some(Value::Array(children)) = node.get("content") else {
        return false;
    };
    !children.is_empty()
        && children.iter().all(|child| {
            child
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| crate::model::INLINE_NODE_TYPES.contains(&t))
        })
}

/// Structural diff of two note documents: blocks are matched positionally
/// by pointer within the note; equal subtrees are skipped without noise;
/// textblocks are compared whole (an inline edit changes the block).
fn diff_documents(old: &Value, new: &Value, pointer: &str, diff: &mut NoteDiff) {
    if old == new {
        return;
    }
    let (old_content, new_content) = match (old.get("content"), new.get("content")) {
        (Some(Value::Array(old)), Some(Value::Array(new))) => (old, new),
        _ => {
            // not both containers with content: report this pointer changed
            diff.blocks_changed.push(block_change(new, pointer));
            return;
        }
    };
    let shared = old_content.len().min(new_content.len());
    for index in 0..shared {
        let child_pointer = format!("{pointer}/content/{index}");
        let (old_child, new_child) = (&old_content[index], &new_content[index]);
        if old_child == new_child {
            continue;
        }
        if is_textblock(old_child) || is_textblock(new_child) {
            diff.blocks_changed
                .push(block_change(new_child, &child_pointer));
            continue;
        }
        if old_child.get("content").is_some() && new_child.get("content").is_some() {
            // both are containers: decide by whether the node itself also
            // changed (type/attrs/marks) or only the content below
            let strip_content = |node: &Value| {
                let mut copy = node.clone();
                if let Value::Object(map) = &mut copy {
                    map.remove("content");
                }
                copy
            };
            if strip_content(old_child) != strip_content(new_child) {
                diff.blocks_changed
                    .push(block_change(new_child, &child_pointer));
            }
            diff_documents(old_child, new_child, &child_pointer, diff);
        } else {
            diff.blocks_changed
                .push(block_change(new_child, &child_pointer));
        }
    }
    for (index, node) in new_content.iter().enumerate().skip(shared) {
        let child_pointer = format!("{pointer}/content/{index}");
        diff.blocks_added.push(block_change(node, &child_pointer));
    }
    for (index, node) in old_content.iter().enumerate().skip(shared) {
        let child_pointer = format!("{pointer}/content/{index}");
        diff.blocks_removed.push(block_change(node, &child_pointer));
    }
}

/// Structural diff of the note sets of two revisions.
fn diff_note_sets(old: &RevisionNotes, new: &RevisionNotes) -> Vec<NoteDiff> {
    let mut diffs = Vec::new();
    for (id, title, doc) in new {
        match old.iter().find(|(old_id, _, _)| old_id == id) {
            None => {
                let mut diff = NoteDiff {
                    note_id: id.clone(),
                    note_title: title.clone(),
                    added: true,
                    removed: false,
                    ..NoteDiff::default()
                };
                diff_documents(&Value::Object(Default::default()), doc, "", &mut diff);
                diffs.push(diff);
            }
            Some((_, old_title, old_doc)) => {
                let mut diff = NoteDiff {
                    note_id: id.clone(),
                    note_title: title.clone().or_else(|| old_title.clone()),
                    note_title_old: if old_title != title {
                        old_title.clone()
                    } else {
                        None
                    },
                    added: false,
                    removed: false,
                    ..NoteDiff::default()
                };
                diff_documents(old_doc, doc, "", &mut diff);
                if !diff.is_empty() {
                    diffs.push(diff);
                }
            }
        }
    }
    for (id, title, _) in old {
        if !new.iter().any(|(new_id, _, _)| new_id == id) {
            diffs.push(NoteDiff {
                note_id: id.clone(),
                note_title: title.clone(),
                added: false,
                removed: true,
                ..NoteDiff::default()
            });
        }
    }
    diffs
}

/// Collect the history of `file`: revisions newest-first, each with its
/// per-note diff against the parent. A file outside a git repository or
/// without history yields an empty list with an explicit warning.
pub fn collect(
    file: &Path,
    note_filter: Option<&str>,
    limit: usize,
    limits: &Limits,
) -> Result<History, UmError> {
    // A file missing from the working copy may still have history (a
    // deleted or renamed-away file): the git check happens after repo
    // location — PATH_NOT_FOUND is answered only when git has nothing
    // either (F-01 adjacent gap).
    let file_absent = !file.exists();
    let limit = limit.min(MAX_REVISIONS);
    let extension = file
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if extension != "um" && extension != "note" {
        // the read-command convention (K-04): a named file that is not a
        // document is PATH_NOT_FOUND, not a usage error
        return Err(UmError::new(
            ErrorCode::PathNotFound,
            format!(
                "{} is not a .um container or .note document",
                file.display()
            ),
        )
        .with_hint("insum history reads .um containers and .note documents."));
    }

    let Some(repo) = locate_repo(file)? else {
        if file_absent {
            return Err(UmError::new(
                ErrorCode::PathNotFound,
                format!("path not found: {}", file.display()),
            ));
        }
        return Ok(History {
            repo_root: String::new(),
            file_in_repo: String::new(),
            revisions: Vec::new(),
            complete: true,
            warnings: vec![warning(
                "NO_GIT_REPOSITORY",
                format!(
                    "{} is not inside a git work tree; history is empty",
                    file.display()
                ),
            )],
        });
    };

    let mut warnings = Vec::new();
    // one extra revision gives the oldest listed one its real parent
    // an unborn HEAD (no commits yet) is an empty history, not an error:
    // probe cheaply before streaming the log
    let head_probe = git(&repo.root, &["rev-parse", "--verify", "HEAD"]);
    if file_absent && head_probe.is_err() {
        // G-03: an absent path in a repository with no commits never
        // existed — PATH_NOT_FOUND, not a soft empty history
        return Err(UmError::new(
            ErrorCode::PathNotFound,
            format!("path not found: {}", file.display()),
        ));
    }
    if head_probe.is_err() {
        warnings.push(warning(
            "NO_GIT_HISTORY",
            format!(
                "no commits touch {} in this repository; history is empty",
                repo.file_in_repo
            ),
        ));
        return Ok(History {
            repo_root: repo.root.display().to_string(),
            file_in_repo: repo.file_in_repo,
            revisions: Vec::new(),
            complete: true,
            warnings,
        });
    }
    let (listed, skipped) = list_revisions(&repo, limit + 1)?;
    if file_absent && listed.is_empty() {
        // G-03: no commit ever touched this path — it never existed
        return Err(UmError::new(
            ErrorCode::PathNotFound,
            format!(
                "path not found: {} (no commit touches this path)",
                file.display()
            ),
        ));
    }
    if skipped > 0 {
        warnings.push(warning(
            "LOG_RECORDS_SKIPPED",
            format!("{skipped} git log record(s) had a malformed object name and were skipped"),
        ));
    }
    if listed.is_empty() {
        warnings.push(warning(
            "NO_GIT_HISTORY",
            format!(
                "no commits touch {} in this repository; history is empty",
                repo.file_in_repo
            ),
        ));
        return Ok(History {
            repo_root: repo.root.display().to_string(),
            file_in_repo: repo.file_in_repo.clone(),
            revisions: Vec::new(),
            complete: true,
            warnings,
        });
    }

    // the diff of every listed revision runs against its REAL first
    // parent (merge-aware): reuse a listed revision's parsed notes when it
    // is the parent, otherwise fetch and parse the parent blob separately
    let listed_count = listed.len();
    let output_count = listed_count.min(limit);
    // S-06: revisions are processed OLDEST-FIRST in a stream — at most two
    // note sets (the current one and its parent) are alive at once, so the
    // aggregate memory does not grow with the revision count. Diffs are
    // collected oldest-first and reversed for the newest-first output.
    let mut diffs: Vec<RevisionDiff> = Vec::new();
    // the state of the revision processed just before this one: its sha
    // and its parsed notes (`None` = the blob does not parse)
    let mut prev: Option<(String, Option<RevisionNotes>)> = None;
    for index in (0..output_count).rev() {
        let revision = listed[index].clone();
        let sha = revision.sha.clone();
        // the path may be absent from this revision's tree entirely (a
        // deleting revision, F-01 site 2) — that is empty content and the
        // diff against the parent shows removals. `git show` is tried
        // FIRST (the common case is one spawn per revision, G-06); only
        // its failure triggers the absent probe to tell "deleting
        // revision" from a real failure
        let own_parsed = match revision_blob(&repo, &sha, limits)
            .and_then(|blob| revision_notes(&blob, &extension, limits))
        {
            Ok(parsed) => Some(parsed),
            Err(show_err) => {
                if path_in_tree(&repo, &sha)? {
                    // the path exists in this tree: the failure is real
                    return Err(show_err);
                }
                // absent: empty content with an explicit marker
                None
            }
        };
        let mut deleted_here = false;
        let notes = match own_parsed {
            Some(Some(notes)) => notes,
            Some(None) => {
                // this blob does not parse: the revision is listed with a
                // warning, the parent linkage restarts at the next one
                let sha_head = sha[..7.min(sha.len())].to_owned();
                diffs.push(RevisionDiff {
                    revision: revision.clone(),
                    notes: Vec::new(),
                    warning: Some(warning(
                        "BLOB_NOT_PARSEABLE",
                        format!(
                            "revision {sha_head} does not parse as a {} file; its diff is not judged",
                            if extension == "note" { ".note" } else { ".um" }
                        ),
                    )),
                });
                prev = Some((revision.sha.clone(), None));
                continue;
            }
            None => {
                deleted_here = true;
                Vec::new()
            }
        };
        let _ = deleted_here;
        let parent_sha = revision.parents.first().cloned();
        enum ParentNotes {
            Root,
            Parsed(RevisionNotes),
            Unparseable,
        }
        let parent = match &parent_sha {
            // root commit: no parent, no diff
            None => ParentNotes::Root,
            Some(sha) => {
                // the previous processed revision IS this parent in a
                // straight chain — reuse; otherwise fetch fresh (bounded
                // memory beats re-fetch avoidance here). A parent whose
                // tree lacks the path (the file was added after it, or it
                // arrived via a merge's second parent) is an empty parent:
                // the diff shows everything added (F-01 sites 1 and 3)
                if prev.as_ref().is_some_and(|(prev_sha, _)| prev_sha == sha) {
                    match &prev.as_ref().unwrap().1 {
                        Some(notes) => ParentNotes::Parsed(notes.clone()),
                        None => ParentNotes::Unparseable,
                    }
                } else if !path_in_tree(&repo, sha)? {
                    ParentNotes::Parsed(Vec::new())
                } else {
                    // try the blob first (the common case, G-06); on
                    // failure the probe tells a parent tree without the
                    // path (added later / via a second parent — F-01
                    // sites 1 and 3: empty parent, everything added)
                    // from a real failure
                    let parsed = match revision_blob(&repo, sha, limits) {
                        Ok(blob) => revision_notes(&blob, &extension, limits)?,
                        Err(show_err) => {
                            if path_in_tree(&repo, sha)? {
                                return Err(show_err);
                            }
                            Some(Vec::new())
                        }
                    };
                    match parsed {
                        Some(notes) => ParentNotes::Parsed(notes),
                        None => ParentNotes::Unparseable,
                    }
                }
            }
        };
        let mut revision_diff = RevisionDiff {
            revision,
            notes: Vec::new(),
            warning: None,
        };
        if deleted_here {
            revision_diff.warning = Some(warning(
                "FILE_ABSENT_IN_REVISION",
                format!(
                    "revision {} does not contain the file (a deleting revision); the diff against its parent shows the content removed",
                    &sha[..7.min(sha.len())]
                ),
            ));
        }
        match parent {
            // the parent blob does not parse: this revision's diff is not
            // judged — said explicitly, never silently
            ParentNotes::Unparseable => {
                revision_diff.warning = Some(warning(
                    "PARENT_BLOB_NOT_PARSEABLE",
                    format!(
                        "the parent revision {} does not parse as a {} file; the diff against it is not judged",
                        parent_sha
                            .as_deref()
                            .map(|s| s[..7.min(s.len())].to_owned())
                            .unwrap_or_default(),
                        if extension == "note" { ".note" } else { ".um" }
                    ),
                ));
            }
            ParentNotes::Root => {
                if let Some(filter) = note_filter {
                    // root commit with a filter: report whether the note
                    // exists in this revision (context, no block diff) —
                    // by id or by title (C-06)
                    if let Some((id, title, _)) = notes
                        .iter()
                        .find(|(id, title, _)| id == filter || title.as_deref() == Some(filter))
                    {
                        // machine-readable identity stays the real note id;
                        // the title matched the filter — never the reverse (D-05)
                        revision_diff.notes.push(NoteDiff {
                            note_id: id.clone(),
                            note_title: title.clone(),
                            ..NoteDiff::default()
                        });
                    }
                }
            }
            ParentNotes::Parsed(parent_notes) => {
                let mut per_note = diff_note_sets(&parent_notes, &notes);
                if let Some(filter) = note_filter {
                    per_note.retain(|diff| {
                        diff.note_id == filter
                            || diff.note_title.as_deref() == Some(filter)
                            || diff.note_title_old.as_deref() == Some(filter)
                    });
                }
                revision_diff.notes = per_note;
            }
        }
        // remember this revision's notes: in a straight chain it IS the
        // parent of the next processed revision (D-03 — without this the
        // fast-path reuse was dead and every revision spawned its parent
        // blob twice)
        prev = Some((sha, Some(notes)));
        diffs.push(revision_diff);
    }

    if file_absent
        && !warnings
            .iter()
            .any(|w| w["code"] == json!("NO_GIT_HISTORY"))
    {
        warnings.push(warning(
            "FILE_ABSENT_IN_WORKING_COPY",
            format!(
                "{} is missing from the working copy; the history below is from git",
                file.display()
            ),
        ));
    }
    if listed_count > limit {
        warnings.push(warning(
            "HAS_MORE_REVISIONS",
            format!("older revisions exist; raise --limit (cap {MAX_REVISIONS}) to see more"),
        ));
    }
    // the stream ran oldest-first — the contract is newest-first
    diffs.reverse();

    if let Some(filter) = note_filter {
        let matched = diffs
            .iter()
            .any(|revision_diff| revision_diff.notes.iter().any(|note| !note.is_empty()))
            || diffs.is_empty();
        if !matched {
            // the filter matched nothing anywhere: say so explicitly
            warnings.push(warning(
                "NOTE_NOT_FOUND_IN_HISTORY",
                format!(
                    "note {filter:?} has no changes in the listed revisions (check the id or exact title)"
                ),
            ));
        }
    }

    Ok(History {
        repo_root: repo.root.display().to_string(),
        file_in_repo: repo.file_in_repo,
        revisions: diffs,
        complete: true,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::process::Command as Process;

    fn git_in(dir: &Path, args: &[&str]) {
        let status = Process::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git is available in test environments");
        assert!(status.success(), "git {args:?} failed");
    }

    /// A throwaway repository with a .um file committed in three revisions:
    /// v1 title only; v2 adds a paragraph; v3 changes the paragraph and
    /// adds a heading.
    fn three_revision_repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);

        let file = dir.path().join("page.um");
        let v1 = serde_json::json!({
            "format": "um", "version": 1, "type": "notepad",
            "notes": [{"id": "note01", "path": "notes/note01.json", "order": 0,
                       "title": "T", "schema": "plain", "schemaVersion": 3}]
        });
        let doc1 = serde_json::json!({"type": "noteDoc", "content": [
            {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]}
        ]});
        write_um_bytes(&file, &v1, &doc1);
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "v1: create"]);

        let doc2 = serde_json::json!({"type": "noteDoc", "content": [
            {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
            {"type": "paragraph", "content": [{"type": "text", "text": "первый абзац"}]}
        ]});
        write_um_bytes(&file, &v1, &doc2);
        git_in(dir.path(), &["add", "."]);
        git_in(
            dir.path(),
            &["commit", "--quiet", "-m", "v2: add paragraph"],
        );

        let doc3 = serde_json::json!({"type": "noteDoc", "content": [
            {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
            {"type": "paragraph", "content": [{"type": "text", "text": "изменённый абзац"}]},
            {"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "Раздел"}]}
        ]});
        write_um_bytes(&file, &v1, &doc3);
        git_in(dir.path(), &["add", "."]);
        git_in(
            dir.path(),
            &["commit", "--quiet", "-m", "v3: rework content"],
        );

        (dir, file)
    }

    fn write_um_bytes(file: &Path, manifest: &Value, doc: &Value) {
        use zip::write::SimpleFileOptions;
        let manifest_bytes = serde_json::to_vec(manifest).unwrap();
        let doc_bytes = serde_json::to_vec(doc).unwrap();
        let writer = std::fs::File::create(file).unwrap();
        let mut zip = zip::ZipWriter::new(writer);
        zip.start_file("manifest.json", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&manifest_bytes).unwrap();
        zip.start_file("notes/note01.json", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&doc_bytes).unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn revisions_arrive_newest_first_with_structural_diffs() {
        let (_dir, file) = three_revision_repo();
        let history = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert!(history.complete);
        assert_eq!(history.revisions.len(), 3);
        assert_eq!(history.revisions[0].revision.subject, "v3: rework content");
        assert_eq!(history.revisions[2].revision.subject, "v1: create");

        // v3 vs v2: the paragraph changed, the heading was added
        let v3 = &history.revisions[0];
        assert_eq!(v3.notes.len(), 1);
        let note = &v3.notes[0];
        assert_eq!(note.note_id, "note01");
        assert_eq!(note.blocks_changed.len(), 1);
        assert_eq!(note.blocks_changed[0].block_type, "paragraph");
        assert_eq!(
            note.blocks_changed[0].pointer, "/content/1",
            "positional match by pointer"
        );
        assert_eq!(note.blocks_added.len(), 1);
        assert_eq!(note.blocks_added[0].block_type, "heading");
        assert_eq!(note.blocks_added[0].pointer, "/content/2");
        assert!(note.blocks_removed.is_empty());

        // v2 vs v1: the paragraph was added
        let v2 = &history.revisions[1];
        assert_eq!(v2.notes[0].blocks_added.len(), 1);
        assert_eq!(v2.notes[0].blocks_added[0].block_type, "paragraph");

        // v1: the oldest listed revision has no parent diff
        assert!(history.revisions[2].notes.is_empty());
    }

    #[test]
    fn note_filter_selects_one_note() {
        let (_dir, file) = three_revision_repo();
        let history = collect(&file, Some("note01"), MAX_REVISIONS, &Limits::defaults()).unwrap();
        for revision in &history.revisions {
            for note in &revision.notes {
                assert_eq!(note.note_id, "note01");
            }
        }
    }

    #[test]
    fn limit_bounds_the_revision_list() {
        let (_dir, file) = three_revision_repo();
        let history = collect(&file, None, 2, &Limits::defaults()).unwrap();
        assert_eq!(history.revisions.len(), 2);
        assert_eq!(history.revisions[0].revision.subject, "v3: rework content");
    }

    #[test]
    fn note_history_works_for_single_note_files() {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        let file = dir.path().join("single.note");
        std::fs::write(
            &file,
            serde_json::to_vec(&serde_json::json!({
                "type": "noteDoc",
                "content": [{"type": "noteTitle", "content": [{"type": "text", "text": "T"}]}]
            }))
            .unwrap(),
        )
        .unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "note v1"]);
        std::fs::write(
            &file,
            serde_json::to_vec(&serde_json::json!({
                "type": "noteDoc",
                "content": [
                    {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
                    {"type": "paragraph", "content": [{"type": "text", "text": "текст"}]}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "note v2"]);

        let history = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert_eq!(history.revisions.len(), 2);
        let diff = &history.revisions[0];
        assert_eq!(diff.notes[0].note_id, "note");
        assert_eq!(diff.notes[0].blocks_added.len(), 1);
    }

    #[test]
    fn file_outside_git_yields_a_warning_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lonely.um");
        std::fs::write(&file, b"placeholder").unwrap();
        let history = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert!(history.revisions.is_empty());
        assert!(history.complete);
        assert!(
            history
                .warnings
                .iter()
                .any(|w| w["code"] == "NO_GIT_REPOSITORY"),
            "{:?}",
            history.warnings
        );
    }

    #[test]
    fn repository_without_commits_yields_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        let file = dir.path().join("page.um");
        std::fs::write(&file, b"placeholder").unwrap();
        let history = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert!(history.revisions.is_empty());
        assert!(
            history
                .warnings
                .iter()
                .any(|w| w["code"] == "NO_GIT_HISTORY"),
            "{:?}",
            history.warnings
        );
    }

    #[test]
    fn missing_file_is_path_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = collect(&dir.path().join("nope.um"), None, 20, &Limits::defaults()).unwrap_err();
        assert_eq!(err.code, ErrorCode::PathNotFound);
    }

    #[test]
    fn non_um_file_is_a_usage_error() {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        // CI runners have no global git identity — the repo sets its own
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        let file = dir.path().join("readme.md");
        std::fs::write(&file, b"x").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "x"]);
        let err = collect(&file, None, 20, &Limits::defaults()).unwrap_err();
        assert_eq!(err.code, ErrorCode::PathNotFound);
    }

    #[test]
    fn corrupt_history_blob_is_listed_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        let file = dir.path().join("page.um");
        std::fs::write(&file, b"not a zip at all").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "broken blob"]);
        let history = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert_eq!(history.revisions.len(), 1);
        assert!(history.revisions[0].warning.is_some());
        assert!(history.revisions[0].notes.is_empty());
    }

    #[test]
    fn working_copy_changes_do_not_appear_in_history() {
        // read-only guarantee: dirtying the working copy must not change
        // the collected history
        let (dir, file) = three_revision_repo();
        let before = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        std::fs::write(&file, b"dirty working copy").unwrap();
        let after = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert_eq!(
            serde_json::to_string(&before.revisions).unwrap(),
            serde_json::to_string(&after.revisions).unwrap()
        );
        drop(dir);
    }

    #[test]
    fn forged_log_records_never_reach_git_arguments() {
        // a commit subject carrying the field separators forges log
        // records; neither the sha nor the parents may pass validation
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        let file = dir.path().join("page.um");
        std::fs::write(&file, b"placeholder").unwrap();
        git_in(dir.path(), &["add", "."]);
        // the forged subject smuggles an --output argument through both
        // the sha and the parents field
        let forged = "v1\u{1e}--output=PWNED\u{1f}zz\u{1e}";
        git_in(
            dir.path(),
            &["commit", "--quiet", "-m", forged, "--allow-empty"],
        );
        let history = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert!(
            history.revisions.is_empty()
                || history
                    .revisions
                    .iter()
                    .all(|r| is_valid_sha(&r.revision.sha))
        );
        assert!(
            !dir.path().join("PWNED:page.um").exists(),
            "no file may be written through a forged argument"
        );
    }

    #[test]
    fn oversized_revision_blob_fails_fast_not_hang() {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        let file = dir.path().join("page.um");
        // an uncompressed 8 MiB blob — far above the 1 MiB test limit
        std::fs::write(&file, vec![b'x'; 8 * 1024 * 1024]).unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "huge blob"]);

        let mut limits = Limits::defaults();
        limits.max_entry_bytes = 1024 * 1024;
        let started = std::time::Instant::now();
        let err = collect(&file, None, MAX_REVISIONS, &limits).unwrap_err();
        assert_eq!(err.code, ErrorCode::LimitExceeded);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the oversized blob must fail fast, not deadlock: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn unparseable_parent_is_reported_not_silent() {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        let file = dir.path().join("page.um");
        std::fs::write(&file, b"broken history state").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "broken parent"]);
        // the child is a real container so only the PARENT fails to parse
        write_um_bytes(
            &file,
            &serde_json::json!({
                "format": "um", "version": 1, "type": "notepad",
                "notes": [{"id": "note01", "path": "notes/note01.json", "order": 0,
                           "title": "T", "schema": "plain", "schemaVersion": 3}]
            }),
            &serde_json::json!({"type": "noteDoc", "content": [
                {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]}
            ]}),
        );
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "valid child"]);

        let history = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert_eq!(history.revisions.len(), 2, "both revisions are listed");
        // the parent blob is "broken history state" — not a .um at all
        let child = &history.revisions[0];
        assert!(
            child
                .warning
                .as_ref()
                .is_some_and(|w| w["code"] == "PARENT_BLOB_NOT_PARSEABLE"),
            "the silent-loss regression: {:?}",
            child
        );
    }

    #[test]
    fn git_environment_is_scrubbed() {
        // an inherited GIT_DIR must not reach the spawned git process:
        // the scrubbed command carries no GIT_* variables at all
        let command = git_command(Path::new("."), &["log"]);
        for (key, _) in command.get_envs() {
            let name = key.to_string_lossy();
            assert!(
                !name.to_ascii_uppercase().starts_with("GIT_"),
                "GIT_* leaked: {name}"
            );
        }
        assert!(
            command
                .get_envs()
                .any(|(key, _)| key.to_string_lossy().to_ascii_uppercase() == "PATH"),
            "PATH must survive the scrub"
        );
    }

    #[test]
    fn file_added_after_the_root_commit_has_full_history() {
        // F-01 site 1: the file is absent from the root commit's tree —
        // the parent blob fetch must not surface as IO_ERROR; the diff
        // shows everything added
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        // base commit with a DIFFERENT file
        std::fs::write(dir.path().join("other.txt"), b"base").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(
            dir.path(),
            &["commit", "--quiet", "-m", "base without page"],
        );
        // "add page" in the second commit
        let file = dir.path().join("page.note");
        std::fs::write(
            &file,
            serde_json::to_vec(&json!({"type": "noteDoc", "content": [
                {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]}
            ]}))
            .unwrap(),
        )
        .unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "add page"]);

        let history = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert_eq!(history.revisions.len(), 1);
        assert_eq!(history.revisions[0].revision.subject, "add page");
        assert!(history.revisions[0].warning.is_none());
    }

    #[test]
    fn deleting_revision_shows_removals_not_io_error() {
        // F-01 site 2: the revision that DELETES the file has no blob for
        // it — the diff shows the notes removed, the command survives
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        let file = dir.path().join("page.note");
        std::fs::write(
            &file,
            serde_json::to_vec(&json!({"type": "noteDoc", "content": [
                {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]}
            ]}))
            .unwrap(),
        )
        .unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "create page"]);
        std::fs::remove_file(&file).unwrap();
        git_in(dir.path(), &["add", "--all"]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "delete page"]);

        let history = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert_eq!(history.revisions.len(), 2);
        let delete_row = &history.revisions[0];
        assert_eq!(delete_row.revision.subject, "delete page");
        assert!(
            delete_row.notes.iter().any(|n| n.removed),
            "the deletion shows as removed notes: {:?}",
            delete_row
        );
    }

    #[test]
    fn merge_bringing_the_file_from_second_parent_works() {
        // F-01 site 3: the file arrives via a merge whose FIRST parent
        // does not have it
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet", "--initial-branch=main"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        // main: no page.um at all
        std::fs::write(dir.path().join("readme.txt"), b"x").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "main base"]);
        // side branch adds page.um
        git_in(dir.path(), &["checkout", "--quiet", "-b", "side"]);
        write_um_bytes(
            &dir.path().join("page.um"),
            &json!({"format": "um", "version": 1, "type": "notepad",
                    "notes": [{"id": "note01", "path": "notes/note01.json", "order": 0,
                               "title": "T", "schema": "plain", "schemaVersion": 3}]}),
            &json!({"type": "noteDoc", "content": [
                {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]}
            ]}),
        );
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "side adds page"]);
        // merge into main (first parent = main without the file)
        git_in(dir.path(), &["checkout", "--quiet", "main"]);
        git_in(
            dir.path(),
            &[
                "merge", "--quiet", "--no-ff", "-X", "ours", "-m", "m", "side",
            ],
        );

        let history = collect(
            &dir.path().join("page.um"),
            None,
            MAX_REVISIONS,
            &Limits::defaults(),
        )
        .unwrap();
        let subjects: Vec<&str> = history
            .revisions
            .iter()
            .map(|r| r.revision.subject.as_str())
            .collect();
        assert!(
            subjects.contains(&"side adds page") && subjects.contains(&"m"),
            "{subjects:?}"
        );
    }

    #[test]
    fn deleted_file_history_is_served_with_a_warning() {
        // F-01 adjacent gap: a file missing from the working copy but
        // present in git history is served with FILE_ABSENT_IN_WORKING_COPY
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        let file = dir.path().join("page.note");
        std::fs::write(
            &file,
            serde_json::to_vec(&json!({"type": "noteDoc", "content": [
                {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]}
            ]}))
            .unwrap(),
        )
        .unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "create page"]);
        std::fs::remove_file(&file).unwrap();
        git_in(dir.path(), &["add", "--all"]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "delete page"]);

        // the file is gone, but the history is still answerable
        let history = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert_eq!(history.revisions.len(), 2);
        assert!(
            history
                .warnings
                .iter()
                .any(|w| w["code"] == "FILE_ABSENT_IN_WORKING_COPY"),
            "{:?}",
            history.warnings
        );
    }

    #[test]
    fn file_deleted_with_its_directory_is_served() {
        // G-02: rm -r of the directory is the most common deletion shape;
        // the git probe must run from the nearest EXISTING ancestor
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        let sub = dir.path().join("doomed");
        std::fs::create_dir_all(&sub).unwrap();
        let file = sub.join("doomed.note");
        std::fs::write(
            &file,
            serde_json::to_vec(&json!({"type": "noteDoc", "content": [
                {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]}
            ]}))
            .unwrap(),
        )
        .unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "create doomed"]);
        std::fs::remove_dir_all(&sub).unwrap();
        git_in(dir.path(), &["add", "--all"]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "rm -r doomed"]);

        let history = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert_eq!(history.revisions.len(), 2);
        assert!(
            history
                .warnings
                .iter()
                .any(|w| w["code"] == "FILE_ABSENT_IN_WORKING_COPY"),
            "{:?}",
            history.warnings
        );
    }

    #[test]
    fn oversized_log_stream_is_capped_not_buffered() {
        // a forged commit subject of half a megabyte: the log stream is
        // read through a hard take bound and fails fast with
        // LIMIT_EXCEEDED (S-02), never buffering it whole
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        let file = dir.path().join("page.um");
        std::fs::write(&file, b"placeholder").unwrap();
        git_in(dir.path(), &["add", "."]);
        // the subject goes through a message file: 9 MiB exceeds the
        // GIT_STDOUT_MAX bound, and argv limits forbid -m that large
        let big_subject = "x".repeat(9 * 1024 * 1024);
        let msg = dir.path().join("msg.txt");
        std::fs::write(&msg, &big_subject).unwrap();
        git_in(dir.path(), &["commit", "--quiet", "-F", "msg.txt"]);

        let started = std::time::Instant::now();
        let err = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap_err();
        assert_eq!(err.code, ErrorCode::LimitExceeded);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the capped stream must fail fast: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn glob_metacharacters_in_file_name_do_not_widen_the_pathspec() {
        // "note[1].um" must not act as a glob pulling foreign commits (S-04)
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        // a foreign commit touching a DIFFERENT file the glob would match
        let foreign = dir.path().join("note11.um");
        std::fs::write(&foreign, b"foreign content").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "foreign commit"]);
        // the target file, committed after the foreign one
        let file = dir.path().join("note[1].um");
        std::fs::write(&file, b"placeholder").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "target commit"]);

        let history = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert_eq!(history.revisions.len(), 1);
        assert_eq!(history.revisions[0].revision.subject, "target commit");
    }

    #[test]
    fn side_branch_commits_are_listed_and_diffed_against_real_parents() {
        // T-03 (merge-aware): c1 -> (c3 "main text", c4 "branch text") -> m
        // --full-history lists all four; every diff runs against the
        // revision's REAL first parent, not the list neighbor
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet", "--initial-branch=main"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        let file = dir.path().join("page.um");
        let write_doc = |text: &str| {
            write_um_bytes(
                &file,
                &serde_json::json!({
                    "format": "um", "version": 1, "type": "notepad",
                    "notes": [{"id": "note01", "path": "notes/note01.json", "order": 0,
                               "title": "T", "schema": "plain", "schemaVersion": 3}]
                }),
                &serde_json::json!({"type": "noteDoc", "content": [
                    {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
                    {"type": "paragraph", "content": [{"type": "text", "text": text}]}
                ]}),
            );
        };
        write_doc("base");
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "c1 base"]);
        git_in(dir.path(), &["checkout", "--quiet", "-b", "side"]);
        write_doc("branch text");
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "c4 branch text"]);
        git_in(dir.path(), &["checkout", "--quiet", "main"]);
        write_doc("main text");
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "c3 main text"]);
        // both sides changed the same file: resolve mechanically, the
        // merge topology is what this test exercises
        git_in(
            dir.path(),
            &[
                "merge", "--quiet", "--no-ff", "-X", "ours", "-m", "m merge", "side",
            ],
        );

        let history = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        let subjects: Vec<&str> = history
            .revisions
            .iter()
            .map(|r| r.revision.subject.as_str())
            .collect();
        assert!(
            subjects.contains(&"c3 main text")
                && subjects.contains(&"c4 branch text")
                && subjects.contains(&"m merge"),
            "--full-history must list side-branch commits: {subjects:?}"
        );
        assert!(
            history.revisions[0].revision.parents.len() == 2,
            "the merge has two parents"
        );
    }

    #[test]
    fn title_filter_finds_renamed_notes_and_root_context() {
        // T-02/C-06: a revision that RENAMES a note is findable by both the
        // old and the new title; the root-commit context matches by title
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        let file = dir.path().join("page.um");
        let write_named = |id: &str, title: &str, extra: &str| {
            let doc = serde_json::json!({"type": "noteDoc", "content": [
                {"type": "noteTitle", "content": [{"type": "text", "text": title}]},
                {"type": "paragraph", "content": [{"type": "text", "text": extra}]}
            ]});
            let manifest = serde_json::json!({
                "format": "um", "version": 1, "type": "notepad",
                "notes": [{"id": id, "path": format!("notes/{id}.json"), "order": 0,
                           "title": title, "schema": "plain", "schemaVersion": 3}]
            });
            let writer = std::fs::File::create(&file).unwrap();
            let mut zip = zip::ZipWriter::new(writer);
            use std::io::Write as _;
            zip.start_file("manifest.json", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&serde_json::to_vec(&manifest).unwrap())
                .unwrap();
            zip.start_file(
                format!("notes/{id}.json"),
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(&serde_json::to_vec(&doc).unwrap()).unwrap();
            zip.finish().unwrap();
        };
        write_named("note01", "Old Title", "тело");
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "v1 old title"]);
        write_named("note01", "New Title", "изменённое тело");
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "v2 rename"]);

        // the OLD title still finds the rename revision
        let history =
            collect(&file, Some("Old Title"), MAX_REVISIONS, &Limits::defaults()).unwrap();
        let renamed = &history.revisions[0];
        assert_eq!(renamed.revision.subject, "v2 rename");
        assert!(renamed.notes[0].note_title_old.as_deref() == Some("Old Title"));
        assert_eq!(renamed.notes[0].note_title.as_deref(), Some("New Title"));

        // the ROOT commit context matches by title (id differs from title)
        let by_title =
            collect(&file, Some("Old Title"), MAX_REVISIONS, &Limits::defaults()).unwrap();
        let root_row = &by_title.revisions.last().unwrap();
        assert!(
            root_row
                .notes
                .iter()
                .any(|n| n.note_title.as_deref() == Some("Old Title")),
            "the oldest listed revision reports title context"
        );
    }

    #[test]
    fn truncated_listing_warns_has_more() {
        // T-07: a limit smaller than the history says so explicitly
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        let file = dir.path().join("page.um");
        std::fs::write(&file, b"placeholder").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "only commit"]);

        let history = collect(&file, None, 0, &Limits::defaults()).unwrap();
        assert!(history.revisions.is_empty());
        assert!(
            history
                .warnings
                .iter()
                .any(|w| w["code"] == "HAS_MORE_REVISIONS"),
            "{:?}",
            history.warnings
        );
    }

    #[test]
    fn forged_log_records_are_skipped_with_a_warning() {
        // T-07/S-03 family: a commit subject carrying the field separators
        // forges log records — the record is skipped with a warning, and
        // the forged "sha" never reaches a git argument
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        git_in(dir.path(), &["config", "user.email", "test@insum"]);
        git_in(dir.path(), &["config", "user.name", "insum test"]);
        let file = dir.path().join("page.um");
        std::fs::write(&file, b"placeholder").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "honest commit"]);
        let forged = "v1\u{1e}--output=PWNED\u{1f}zz\u{1e}";
        // the forged commit must TOUCH the file — --full-history with a
        // pathspec lists only commits that change it
        std::fs::write(&file, b"touched by the forged commit").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", forged]);

        let history = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert!(
            history
                .warnings
                .iter()
                .any(|w| w["code"] == "LOG_RECORDS_SKIPPED"),
            "{:?}",
            history.warnings
        );
        assert!(
            history
                .revisions
                .iter()
                .all(|r| r.revision.subject != forged.replace(['\u{1e}', '\u{1f}'], ""))
        );
        assert!(!dir.path().join("PWNED:page.um").exists());
    }

    #[test]
    fn output_is_deterministic_across_runs() {
        let (_dir, file) = three_revision_repo();
        let a = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        let b = collect(&file, None, MAX_REVISIONS, &Limits::defaults()).unwrap();
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }
}
