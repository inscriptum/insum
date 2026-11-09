//! `insum history` — CLI contract tests: envelope, filters, formats,
//! budgets, error classes. Core-level sourcing/diff tests live in
//! insum-core; these exercise the command end to end in a real repository.

use std::io::Write;
use std::path::Path;
use std::process::Command as Process;

use assert_cmd::Command;
use serde_json::{Value, json};
use zip::write::SimpleFileOptions;

fn insum() -> Command {
    Command::cargo_bin("insum").expect("insum binary builds")
}

fn git_in(dir: &Path, args: &[&str]) {
    let status = Process::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("git is available");
    assert!(status.success(), "git {args:?} failed");
}

fn write_um(file: &Path, doc: &Value) {
    let manifest = json!({
        "format": "um", "version": 1, "type": "notepad",
        "notes": [{"id": "note01", "path": "notes/note01.json", "order": 0,
                   "title": "T", "schema": "plain", "schemaVersion": 3}]
    });
    let writer = std::fs::File::create(file).unwrap();
    let mut zip = zip::ZipWriter::new(writer);
    zip.start_file("manifest.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&serde_json::to_vec(&manifest).unwrap())
        .unwrap();
    zip.start_file("notes/note01.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&serde_json::to_vec(doc).unwrap()).unwrap();
    zip.finish().unwrap();
}

/// A repository with three revisions of kb/page.um.
fn three_revision_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git_in(dir.path(), &["init", "--quiet"]);
    git_in(dir.path(), &["config", "user.email", "test@insum"]);
    git_in(dir.path(), &["config", "user.name", "insum test"]);
    let file = dir.path().join("page.um");
    write_um(
        &file,
        &json!({"type": "noteDoc", "content": [
            {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]}
        ]}),
    );
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "--quiet", "-m", "v1: create"]);
    write_um(
        &file,
        &json!({"type": "noteDoc", "content": [
            {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
            {"type": "paragraph", "content": [{"type": "text", "text": "текст"}]}
        ]}),
    );
    git_in(dir.path(), &["add", "."]);
    git_in(
        dir.path(),
        &["commit", "--quiet", "-m", "v2: add paragraph"],
    );

    dir
}

#[test]
fn history_outputs_the_json_envelope() {
    let dir = three_revision_repo();
    let output = insum()
        .arg("history")
        .arg("page.um")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.ends_with('\n'), "one JSON document plus newline");
    let value: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["api_version"], 1);
    assert_eq!(value["file_in_repo"], "page.um");
    assert!(!value["repo_root"].as_str().unwrap().is_empty());
    assert_eq!(value["complete"], true);
    assert_eq!(value["warnings"], json!([]));
    let revisions = value["revisions"].as_array().unwrap();
    assert_eq!(revisions.len(), 2);
    // newest first
    assert_eq!(revisions[0]["subject"], "v2: add paragraph");
    assert!(revisions[0]["timestamp"].is_i64());
    assert!(revisions[0]["author"].is_string());
    // the newest revision diffs against the previous one
    let notes = revisions[0]["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["note_id"], "note01");
    let added = notes[0]["blocks_added"].as_array().unwrap();
    assert_eq!(added.len(), 1);
    assert_eq!(added[0]["block_type"], "paragraph");
    assert_eq!(added[0]["pointer"], "/content/1");
    assert_eq!(added[0]["text"], "текст");
}

#[test]
fn history_limit_bounds_and_orders() {
    let dir = three_revision_repo();
    let output = insum()
        .arg("history")
        .arg("page.um")
        .arg("--limit")
        .arg("1")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let revisions = value["revisions"].as_array().unwrap();
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0]["subject"], "v2: add paragraph");
}

#[test]
fn history_note_filter_and_unknown_note_warning() {
    let dir = three_revision_repo();
    let output = insum()
        .arg("history")
        .arg("page.um")
        .arg("--note")
        .arg("note01")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    for revision in value["revisions"].as_array().unwrap() {
        for note in revision["notes"].as_array().unwrap() {
            assert_eq!(note["note_id"], "note01");
        }
    }

    let unknown = insum()
        .arg("history")
        .arg("page.um")
        .arg("--note")
        .arg("NoSuchNote")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(unknown.status.success());
    let value: Value = serde_json::from_slice(&unknown.stdout).unwrap();
    let codes: Vec<&str> = value["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"NOTE_NOT_FOUND_IN_HISTORY"), "{codes:?}");
}

#[test]
fn history_annotated_is_text_with_anchors() {
    let dir = three_revision_repo();
    let output = insum()
        .arg("history")
        .arg("page.um")
        .arg("--format")
        .arg("annotated")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("<!-- um:history "), "anchor first: {text}");
    assert!(text.contains("@"));
    assert!(text.contains("v2: add paragraph"));
    assert!(text.contains("+ /content/1 paragraph"));
}

#[test]
fn history_unknown_format_is_a_usage_error() {
    let dir = three_revision_repo();
    let output = insum()
        .arg("history")
        .arg("page.um")
        .arg("--format")
        .arg("yaml")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], "USAGE");
}

#[test]
fn history_outside_git_is_a_warning_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lonely.um");
    write_um(&file, &json!({"type": "noteDoc", "content": []}));
    let output = insum().arg("history").arg(&file).output().unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["revisions"], json!([]));
    let codes: Vec<&str> = value["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"NO_GIT_REPOSITORY"), "{codes:?}");
}

#[test]
fn history_missing_file_is_exit_3() {
    let dir = tempfile::tempdir().unwrap();
    let output = insum()
        .arg("history")
        .arg(dir.path().join("nope.um"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], "PATH_NOT_FOUND");
}

#[test]
fn history_budget_drops_oldest_explicitly() {
    let dir = three_revision_repo();
    // a tiny budget cannot hold even one revision with the diff payload
    let output = insum()
        .arg("history")
        .arg("page.um")
        .arg("--max-bytes")
        .arg("40")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(5),
        "one revision > budget: LIMIT_EXCEEDED"
    );
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], "LIMIT_EXCEEDED");

    // a budget that fits some revisions but not all: the oldest are
    // dropped, complete flips false, a warning names the knob; the size
    // is derived from the actual output so the test does not guess bytes
    let full = insum()
        .arg("history")
        .arg("page.um")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let full_value: Value = serde_json::from_slice(&full.stdout).unwrap();
    let _full_len = serde_json::to_string(&full_value).unwrap().len();

    // a HEAVY middle revision makes the drop profitable: the drop walks
    // from the oldest (a tiny root without diffs), so the budget is aimed
    // below the two oldest revisions — both are dropped, the newest stays
    let long_text = "длинный текст ".repeat(200);
    write_um(
        &dir.path().join("page.um"),
        &json!({"type": "noteDoc", "content": [
            {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
            {"type": "paragraph", "content": [{"type": "text", "text": long_text}]}
        ]}),
    );
    git_in(dir.path(), &["add", "."]);
    git_in(
        dir.path(),
        &["commit", "--quiet", "-m", "v3: long paragraph"],
    );

    // one more tiny commit on top: now the heavy diff sits in the MIDDLE
    // of the list [v4 tiny, v3 heavy, v2 small, v1 tiny]
    write_um(
        &dir.path().join("page.um"),
        &json!({"type": "noteDoc", "content": [
            {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
            {"type": "paragraph", "content": [{"type": "text", "text": long_text}]},
            {"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "H"}]}
        ]}),
    );
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "--quiet", "-m", "v4: heading"]);

    let full = insum()
        .arg("history")
        .arg("page.um")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let full_value: Value = serde_json::from_slice(&full.stdout).unwrap();
    let full_len = serde_json::to_string(&full_value).unwrap().len();
    let revisions = full_value["revisions"].as_array().unwrap();
    assert!(revisions.len() == 4, "four revisions listed");
    let heavy_len = serde_json::to_string(&revisions[1]).unwrap().len();
    assert!(heavy_len > 400, "the fixture needs a heavy middle revision");
    let tail_len = serde_json::to_string(&revisions[2]).unwrap().len()
        + serde_json::to_string(&revisions[3]).unwrap().len();
    // the budget forces exactly two drops (the two oldest, the small
    // ones) and keeps the heavy middle revision alive
    let budget = full_len - tail_len + 200;
    assert!(budget < full_len, "the full output must not fit");
    let output = insum()
        .arg("history")
        .arg("page.um")
        .arg("--max-bytes")
        .arg(budget.to_string())
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["complete"], false);
    let revisions_left = value["revisions"].as_array().unwrap().len();
    assert_eq!(
        value["revisions_dropped"].as_u64().unwrap() as usize + revisions_left,
        4
    );
    assert_eq!(revisions_left, 2, "the heavy middle revision is dropped");
    assert!(codes_contains(&value, "OUTPUT_BUDGET"));
}

fn codes_contains(value: &Value, code: &str) -> bool {
    value["warnings"]
        .as_array()
        .map(|warnings| warnings.iter().any(|w| w["code"].as_str() == Some(code)))
        .unwrap_or(false)
}

#[test]
fn history_working_copy_and_head_are_untouched() {
    let dir = three_revision_repo();
    let head_before = git_output(dir.path(), &["rev-parse", "HEAD"]);
    let file_bytes_before = std::fs::read(dir.path().join("page.um")).unwrap();
    let _ = insum()
        .arg("history")
        .arg("page.um")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let head_after = git_output(dir.path(), &["rev-parse", "HEAD"]);
    let file_bytes_after = std::fs::read(dir.path().join("page.um")).unwrap();
    assert_eq!(head_before, head_after);
    assert_eq!(file_bytes_before, file_bytes_after);
    // no stray sidecar/temp files appear (git's own .git dir is expected)
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        entries
            .iter()
            .all(|name| name == ".git" || !name.starts_with('.')),
        "{entries:?}"
    );
}

fn git_output(dir: &Path, args: &[&str]) -> String {
    let output = Process::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap()
}
