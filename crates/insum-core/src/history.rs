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

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use serde_json::Value;

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
    pub sha: String,
    /// Commit time, epoch seconds (from git — no local clock).
    pub timestamp: i64,
    pub author: String,
    pub subject: String,
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
fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, UmError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
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
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        // a repository without any commits: `git log` fails — that is an
        // empty history, not an error
        if stderr.contains("does not have any commits yet") {
            return Ok(Vec::new());
        }
        return Err(UmError::new(
            ErrorCode::IoError,
            format!("git {} failed: {stderr}", args.join(" ")),
        ));
    }
    Ok(output.stdout)
}

struct RepoLocation {
    root: PathBuf,
    file_in_repo: String,
}

/// Locate the repository and the file path inside it; `Ok(None)` — the
/// file is not inside a work tree (the caller answers with a warning).
fn locate_repo(file: &Path) -> Result<Option<RepoLocation>, UmError> {
    let dir = if file.is_dir() {
        file
    } else {
        file.parent().unwrap_or(Path::new("."))
    };
    let root_bytes = match git(dir, &["rev-parse", "--show-toplevel"]) {
        Ok(bytes) => bytes,
        Err(err) => {
            // "not a git repository" is an expected outcome, not a failure
            if err.message.contains("not a git repository") {
                return Ok(None);
            }
            return Err(err);
        }
    };
    let root_text = String::from_utf8_lossy(&root_bytes).trim().to_owned();
    let root = PathBuf::from(root_text);
    let canonical_file = file.canonicalize().map_err(UmError::from)?;
    let canonical_root = root.canonicalize().map_err(UmError::from)?;
    let relative = canonical_file.strip_prefix(&canonical_root).map_err(|_| {
        UmError::new(
            ErrorCode::IoError,
            format!(
                "file {} is outside the repository at {}",
                canonical_file.display(),
                canonical_root.display()
            ),
        )
    })?;
    let file_in_repo = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Ok(Some(RepoLocation { root, file_in_repo }))
}

/// List the revisions that touched the file, newest-first.
fn list_revisions(repo: &RepoLocation, limit: usize) -> Result<Vec<Revision>, UmError> {
    let format = "%H%x1f%at%x1f%an%x1f%s%x1e";
    let out = git(
        &repo.root,
        &[
            "log",
            &format!("--max-count={limit}"),
            &format!("--format={format}"),
            "--",
            &repo.file_in_repo,
        ],
    )?;
    let text = String::from_utf8_lossy(&out);
    let mut revisions = Vec::new();
    for record in text.split('\u{1e}') {
        let record = record.trim_start_matches(['\n', '\r']).trim();
        if record.is_empty() {
            continue;
        }
        let mut fields = record.split('\u{1f}');
        let sha = fields.next().unwrap_or("").trim().to_owned();
        let timestamp = fields
            .next()
            .unwrap_or("")
            .trim()
            .parse::<i64>()
            .unwrap_or(0);
        let author = fields.next().unwrap_or("").trim().to_owned();
        let subject = fields.next().unwrap_or("").trim().to_owned();
        if sha.is_empty() {
            continue;
        }
        revisions.push(Revision {
            sha,
            timestamp,
            author,
            subject,
        });
    }
    Ok(revisions)
}

/// Extract the blob of one revision: `git show <sha>:<path>`.
fn revision_blob(repo: &RepoLocation, sha: &str, limits: &Limits) -> Result<Vec<u8>, UmError> {
    let object = format!("{sha}:{}", repo.file_in_repo);
    let out = git(&repo.root, &["show", &object])?;
    if out.len() > limits.max_entry_bytes {
        return Err(UmError::new(
            ErrorCode::LimitExceeded,
            format!(
                "revision blob larger than UM_MAX_ENTRY_BYTES ({})",
                limits.max_entry_bytes
            ),
        ));
    }
    Ok(out)
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
        return Ok(Some(vec![("note".to_owned(), None, doc)]));
    }
    // .um: a ZIP container — parse in memory
    let reader = std::io::Cursor::new(blob);
    let mut archive = match zip::ZipArchive::new(reader) {
        Ok(archive) => archive,
        Err(_) => return Ok(None),
    };
    let manifest_bytes = match archive.by_name("manifest.json") {
        Ok(entry) => {
            if entry.size() > limits.max_entry_bytes as u64 {
                return Err(UmError::new(
                    ErrorCode::LimitExceeded,
                    format!(
                        "manifest larger than UM_MAX_ENTRY_BYTES ({})",
                        limits.max_entry_bytes
                    ),
                ));
            }
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut std::io::BufReader::new(entry), &mut bytes)?;
            bytes
        }
        Err(_) => return Ok(None),
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
        let payload = match archive.by_name(&descriptor.path) {
            Ok(entry) => {
                if entry.size() > limits.max_entry_bytes as u64 {
                    return Err(UmError::new(
                        ErrorCode::LimitExceeded,
                        format!(
                            "note payload larger than UM_MAX_ENTRY_BYTES ({})",
                            limits.max_entry_bytes
                        ),
                    ));
                }
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut std::io::BufReader::new(entry), &mut bytes)?;
                bytes
            }
            Err(_) => return Ok(None), // inconsistent history blob
        };
        let doc: Value = match serde_json::from_slice(&payload) {
            Ok(doc) => doc,
            Err(_) => return Ok(None),
        };
        notes.push((descriptor.id.clone(), descriptor.title.clone(), doc));
    }
    Ok(Some(notes))
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
    if !file.exists() {
        return Err(UmError::new(
            ErrorCode::PathNotFound,
            format!("path not found: {}", file.display()),
        ));
    }
    let limit = limit.min(MAX_REVISIONS);
    let extension = file
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if extension != "um" && extension != "note" {
        return Err(UmError::usage(
            "insum history reads .um containers and .note documents",
        ));
    }

    let Some(repo) = locate_repo(file)? else {
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

    // one extra revision gives the oldest listed one its real parent
    let listed = list_revisions(&repo, limit + 1)?;
    let mut warnings = Vec::new();
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

    // parse every listed revision's notes (bounded by --limit + 1)
    let mut parsed: Vec<Option<RevisionNotes>> = Vec::with_capacity(listed.len());
    for revision in &listed {
        let notes = revision_blob(&repo, &revision.sha, limits)
            .and_then(|blob| revision_notes(&blob, &extension, limits))?;
        parsed.push(notes);
    }

    let mut diffs: Vec<RevisionDiff> = Vec::new();
    for index in 0..listed.len().min(limit) {
        let revision = &listed[index];
        match &parsed[index] {
            Some(notes) => {
                let mut revision_diff = RevisionDiff {
                    revision: revision.clone(),
                    notes: Vec::new(),
                    warning: None,
                };
                if let Some(Some(parent_notes)) = parsed.get(index + 1) {
                    // the parent is the NEXT (older) revision; the diff
                    // runs from the parent to this revision
                    let mut per_note = diff_note_sets(parent_notes, notes);
                    if let Some(filter) = note_filter {
                        per_note.retain(|diff| {
                            diff.note_id == filter || diff.note_title.as_deref() == Some(filter)
                        });
                    }
                    revision_diff.notes = per_note;
                } else if index + 1 < listed.len() && parsed[index + 1].is_none() {
                    // the parent blob does not parse: this revision's diff
                    // is not judged
                    revision_diff.warning = Some(warning(
                        "PARENT_BLOB_NOT_PARSEABLE",
                        format!(
                            "the parent revision {} does not parse; the diff against it is not judged",
                            &listed[index + 1].sha[..7.min(listed[index + 1].sha.len())]
                        ),
                    ));
                } else if let Some(filter) = note_filter {
                    // the oldest revision overall: context for the filter
                    if let Some((_, title, _)) = notes.iter().find(|(id, _, _)| id == filter) {
                        revision_diff.notes.push(NoteDiff {
                            note_id: filter.to_owned(),
                            note_title: title.clone(),
                            ..NoteDiff::default()
                        });
                    }
                }
                diffs.push(revision_diff);
            }
            None => {
                // this blob does not parse: the revision is listed with a
                // warning, the parent linkage restarts at the next one
                diffs.push(RevisionDiff {
                    revision: revision.clone(),
                    notes: Vec::new(),
                    warning: Some(warning(
                        "BLOB_NOT_PARSEABLE",
                        format!(
                            "revision {} does not parse as a {} file; its diff is not judged",
                            &revision.sha[..7.min(revision.sha.len())],
                            if extension == "note" { ".note" } else { ".um" }
                        ),
                    )),
                });
            }
        }
    }

    if let Some(filter) = note_filter {
        let matched = diffs
            .iter()
            .any(|revision| revision.notes.iter().any(|note| !note.is_empty()))
            || diffs.is_empty();
        if !matched && warnings.is_empty() {
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
        let file = dir.path().join("readme.md");
        std::fs::write(&file, b"x").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "x"]);
        let err = collect(&file, None, 20, &Limits::defaults()).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
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
