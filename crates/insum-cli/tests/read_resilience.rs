//! Read resilience: a torn, empty, or absent container answers READ_TORN
//! (exit 4) from every reading command — never NOTE_NOT_FOUND, which sends
//! agents hunting for a wrong note title. `history` is the documented
//! exception: it reads git, not the live container bytes, and its
//! absent-path contract (G-03) answers PATH_NOT_FOUND.

use assert_cmd::Command;
use insum_test_utils::{FixtureNote, ulid_seq, write_um};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn insum() -> Command {
    Command::cargo_bin("insum").expect("insum binary builds")
}

fn stderr_code(output: &std::process::Output) -> String {
    let value: Value =
        serde_json::from_slice(&output.stderr).expect("stderr is one JSON error object");
    value["error"]["code"]
        .as_str()
        .expect("error carries a code")
        .to_owned()
}

fn assert_torn(args: &[&str], cause: &str) {
    let output = insum().args(args).output().unwrap();
    assert_eq!(output.status.code(), Some(4), "READ_TORN exits 4: {args:?}");
    assert!(output.stdout.is_empty(), "stdout stays empty: {args:?}");
    let code = stderr_code(&output);
    assert_eq!(code, "READ_TORN", "args: {args:?}");
    let value: Value =
        serde_json::from_slice(&output.stderr).expect("stderr is one JSON error object");
    assert_eq!(value["error"]["details"]["cause"], cause, "args: {args:?}");
    assert!(
        value["error"]["hint"].is_string(),
        "READ_TORN carries a hint: {args:?}"
    );
}

/// `search` is best-effort (§11): a torn file lands in `skipped` with its
/// READ_TORN error and lowers `complete`; the run itself exits 0.
fn assert_search_skipped_torn(path: &str, cause: &str) {
    let output = insum()
        .args(["search", "-F", "Alpha", path])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "best-effort exits 0");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["complete"], false);
    let skipped = value["skipped"].as_array().expect("skipped is a list");
    assert_eq!(skipped.len(), 1, "exactly the torn file is skipped");
    assert_eq!(skipped[0]["error"]["code"], "READ_TORN");
    assert_eq!(skipped[0]["error"]["details"]["cause"], cause);
}

/// `validate` reports per-file failures inside `files[]` entries (§6.6) —
/// the torn file shows up with the READ_TORN code and `valid: false`.
fn assert_validate_reports_torn(path: &str, cause: &str) {
    // validate's exit code stays 0 unless the run itself fails — the
    // validity signal is `valid: false` plus the per-file error entry
    let output = insum().args(["validate", path]).output().unwrap();
    assert!(output.status.success(), "validate reports in JSON");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["valid"], false);
    let files = value["files"].as_array().expect("files is a list");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["errors"][0]["code"], "READ_TORN");
    assert_eq!(files[0]["errors"][0]["details"]["cause"], cause);
    assert!(files[0]["errors"][0]["hint"].is_string());
}

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    bytes: Vec<u8>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("insum-resilience-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.um");
        write_um(
            &path,
            &[
                FixtureNote::title_page(&ulid_seq(1), "Alpha", "first", &[]),
                FixtureNote::plain_page(&ulid_seq(2), "Beta", &[]),
            ],
        )
        .unwrap();
        let bytes = fs::read(&path).unwrap();
        Self { dir, path, bytes }
    }

    fn path_str(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    fn rewrite(&self, bytes: &[u8]) -> String {
        fs::write(&self.path, bytes).unwrap();
        self.path_str()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// The reading commands over a single `.um` path.
fn single_file_commands(path: &str) -> Vec<Vec<&str>> {
    vec![
        vec!["ls", path],
        vec!["tree", path],
        vec!["read", path, "--note", "Alpha"],
        vec![
            "block",
            "get",
            path,
            "--note",
            "Alpha",
            "--pointer",
            "/content/0",
        ],
    ]
}

#[test]
fn absent_um_answers_read_torn_for_every_reader() {
    let fixture = Fixture::new("absent");
    let path = fixture
        .dir
        .join("missing.um")
        .to_string_lossy()
        .into_owned();
    for args in single_file_commands(&path) {
        assert_torn(&args, "absent");
    }
    // an absent ARGUMENT path fails the run before any per-file work
    assert_torn(&["search", "-F", "Alpha", &path], "absent");
    assert_torn(&["validate", &path], "absent");
}

#[test]
fn empty_um_answers_read_torn_for_every_reader() {
    let fixture = Fixture::new("empty");
    let path = fixture.rewrite(b"");
    for args in single_file_commands(&path) {
        assert_torn(&args, "empty");
    }
    // existing-but-broken files ride the best-effort paths (§11)
    assert_search_skipped_torn(&path, "empty");
    assert_validate_reports_torn(&path, "empty");
}

#[test]
fn truncated_um_answers_read_torn_for_every_reader() {
    let fixture = Fixture::new("truncated");
    let len = fixture.bytes.len();
    // representative cuts: signature stub, inside the manifest, inside a
    // payload, inside the central directory, one byte short
    let cuts = [1usize, 7, len / 3, 2 * len / 3, len - 40, len - 1];
    for cut in cuts {
        let path = fixture.rewrite(&fixture.bytes[..cut]);
        for args in single_file_commands(&path) {
            assert_torn(&args, "truncated");
        }
        assert_search_skipped_torn(&path, "truncated");
        assert_validate_reports_torn(&path, "truncated");
    }
}

#[test]
fn note_files_answer_read_torn_when_absent_or_empty() {
    let fixture = Fixture::new("note");
    let absent = fixture
        .dir
        .join("missing.note")
        .to_string_lossy()
        .into_owned();
    assert_torn(&["read", &absent], "absent");
    let empty_path = fixture.dir.join("empty.note");
    fs::write(&empty_path, b"").unwrap();
    assert_torn(&["read", &empty_path.to_string_lossy()], "empty");
}

/// A path argument that never existed and is not a container keeps the
/// PATH_NOT_FOUND diagnosis.
#[test]
fn absent_directory_stays_path_not_found() {
    let fixture = Fixture::new("dir");
    let dir = fixture
        .dir
        .join("no-such-dir")
        .to_string_lossy()
        .into_owned();
    let output = insum().args(["ls", &dir]).output().unwrap();
    assert_eq!(stderr_code(&output), "PATH_NOT_FOUND");
}

/// m-14: a directory where a container/note is expected answers a
/// deterministic data-class error on EVERY platform — never the transient
/// READ_TORN truncated verdict (a directory is not a write window; the
/// "restore from backup" hint would mislead) and never a platform-dependent
/// IO_ERROR. `ls` is exempt: a directory is its legitimate traversal input.
#[test]
fn directory_at_a_container_path_answers_a_deterministic_data_error() {
    let fixture = Fixture::new("dirfile");
    let as_um = fixture.dir.join("not-a-container.um");
    fs::create_dir(&as_um).unwrap();
    let as_note = fixture.dir.join("not-a-note.note");
    fs::create_dir(&as_note).unwrap();

    // the .um readers: tree / read / block get
    let um = as_um.to_string_lossy().into_owned();
    for args in [
        vec!["tree", um.as_str()],
        vec!["read", um.as_str(), "--note", "Alpha"],
        vec![
            "block",
            "get",
            um.as_str(),
            "--note",
            "Alpha",
            "--pointer",
            "/content/0",
        ],
    ] {
        let output = insum().args(&args).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(4),
            "invalid data exits 4: {args:?}"
        );
        assert!(output.stdout.is_empty(), "stdout stays empty: {args:?}");
        assert_eq!(stderr_code(&output), "NOT_A_ZIP", "args: {args:?}");
    }
    // the .note reader
    let output = insum()
        .arg("read")
        .arg(as_note.to_string_lossy().into_owned())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(stderr_code(&output), "NOTE_JSON_INVALID");
}

/// history reads git, not the live container bytes: a torn working file
/// serves its (empty, non-git) history like any untracked file — no
/// container-integrity error, and never NOTE_NOT_FOUND. The absent path
/// keeps the reviewed G-03 contract (PATH_NOT_FOUND, no git evidence).
#[test]
fn history_never_answers_note_not_found_on_torn_or_absent() {
    let fixture = Fixture::new("history");
    let torn = fixture.rewrite(&fixture.bytes[..fixture.bytes.len() / 2]);
    let output = insum().args(["history", &torn]).output().unwrap();
    assert!(
        output.status.success(),
        "a torn working file does not block history: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["revisions"].as_array().map(Vec::len), Some(0));
    assert!(
        value["warnings"]
            .as_array()
            .expect("warnings is a list")
            .iter()
            .any(|w| w["code"] == "NO_GIT_REPOSITORY"),
        "non-git directory explains the empty history"
    );
    let absent = fixture
        .dir
        .join("missing.um")
        .to_string_lossy()
        .into_owned();
    let output = insum().args(["history", &absent]).output().unwrap();
    assert_eq!(output.status.code(), Some(3), "G-03: absent path exits 3");
    assert_eq!(stderr_code(&output), "PATH_NOT_FOUND");
}

/// A `.note` is not a container: torn JSON content answers NOTE_JSON_INVALID
/// (truncation of a JSON document is undetectable from a signature). Empty
/// and absent .note files are covered above; this pins the content-error
/// class so the READ_TORN scope stays explicit.
#[test]
fn torn_note_content_answers_note_json_invalid() {
    let fixture = Fixture::new("notejson");
    let torn = fixture.dir.join("torn.note");
    fs::write(&torn, b"{\"type\":\"noteDoc\",\"con").unwrap();
    let output = insum()
        .arg("read")
        .arg(torn.to_string_lossy().into_owned())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(stderr_code(&output), "NOTE_JSON_INVALID");
}

/// Control: the same commands succeed on the intact container.
#[test]
fn healthy_container_still_reads_through_every_reader() {
    let fixture = Fixture::new("healthy");
    let path = fixture.path_str();
    for args in single_file_commands(&path) {
        let output = insum().args(&args).output().unwrap();
        assert!(
            output.status.success(),
            "healthy read must succeed: {args:?} — {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
