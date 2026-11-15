//! `insum props` and `insum ls --with-properties --with-rev` acceptance
//! tests — the contract of the props-api task: several properties, `--get`
//! (hit / miss / duplicate name), plain pages, `.note` files, ls regression
//! without the flags and the augmented listing with them. Fixtures come
//! from `insum-test-utils`; path expectations use `/` only (the JSON
//! rendering is OS-independent).

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

use insum_test_utils as fixtures;

fn insum() -> Command {
    Command::cargo_bin("insum").expect("insum binary builds")
}

fn run_json(args: &[&str]) -> Value {
    let output = insum().args(args).output().expect("insum runs");
    assert!(
        output.status.success(),
        "command {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|err| panic!("stdout of {args:?} is one JSON document: {err}"))
}

/// Run a command expected to fail; return (exit_code, stderr JSON).
fn run_error(args: &[&str]) -> (i32, Value) {
    let output = insum().args(args).output().expect("insum runs");
    assert!(
        output.stdout.is_empty(),
        "stdout stays empty on error: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let error: Value = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|err| panic!("stderr of {args:?} is one JSON error object: {err}"));
    (output.status.code().expect("exit code"), error)
}

/// A container with a v3 title page (two rows: a text one and a list one)
/// plus a plain page, and a standalone `.note` with the same title doc.
struct Corpus {
    dir: TempDir,
}

impl Corpus {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path();
        let notes = [
            fixtures::FixtureNote::title_page_v3(
                &fixtures::ulid_seq(1),
                "issues",
                &[
                    fixtures::note_property("description", &["Journal of the insum project"]),
                    fixtures::note_property("state", &["todo", "done"]),
                ],
                &[
                    fixtures::heading(2, "Backlog"),
                    fixtures::paragraph(&[fixtures::text("body")]),
                ],
            ),
            fixtures::FixtureNote::plain_page(
                &fixtures::ulid_seq(2),
                "plain page",
                &[fixtures::paragraph(&[fixtures::text("no properties here")])],
            ),
        ];
        fixtures::write_um(&path.join("props.um"), &notes).expect("props.um fixture");
        fixtures::write_um(
            &path.join("plain.um"),
            &[fixtures::FixtureNote::title_page(
                &fixtures::ulid_seq(3),
                "v2 title",
                "a v2 summary, no properties",
                &[],
            )],
        )
        .expect("plain.um fixture");
        fixtures::write_legacy_note(
            &path.join("single.note"),
            &fixtures::title_doc_v3(
                "standalone",
                &[fixtures::note_property("status", &["in progress"])],
                &[],
            ),
        )
        .expect("single.note fixture");
        Self { dir }
    }

    fn path(&self, name: &str) -> String {
        self.dir.path().join(name).to_string_lossy().into_owned()
    }

    fn raw(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join(name)
    }
}

// ---------------------------------------------------------------------------
// props: the list form
// ---------------------------------------------------------------------------

#[test]
fn props_lists_several_properties_with_ids_and_pointers() {
    let corpus = Corpus::new();
    let file = corpus.path("props.um");
    let json = run_json(&["props", &file]);
    assert_eq!(json["api_version"], 1);
    assert_eq!(json["path"], file);
    assert_eq!(json["note_id"], fixtures::ulid_seq(1));
    assert_eq!(json["note_title"], "issues");
    assert!(
        json["note_rev"]
            .as_str()
            .expect("note_rev")
            .starts_with("sha256:")
    );
    assert_eq!(json["complete"], true);
    assert_eq!(json["warnings"], serde_json::json!([]));

    // one entry per value node: 1 (description) + 2 (state) = 3
    let properties = json["properties"].as_array().expect("properties array");
    assert_eq!(properties.len(), 3);

    let description = &properties[0];
    assert_eq!(description["name"], "description");
    assert_eq!(description["value"], "Journal of the insum project");
    assert!(
        description["name_id"]
            .as_str()
            .expect("name_id")
            .starts_with('p')
    );
    assert!(
        description["value_id"]
            .as_str()
            .expect("value_id")
            .starts_with('p')
    );
    assert_ne!(description["name_id"], description["value_id"]);
    assert_eq!(description["name_pointer"], "/content/1/content/0");
    assert_eq!(description["value_pointer"], "/content/1/content/1");
    assert_eq!(description["property_pointer"], "/content/1");

    // a list-type row flattens into one entry per value node; the name
    // repeats, the value ids differ
    let state_first = &properties[1];
    let state_second = &properties[2];
    assert_eq!(state_first["name"], "state");
    assert_eq!(state_second["name"], "state");
    assert_eq!(state_first["value"], "todo");
    assert_eq!(state_second["value"], "done");
    assert_ne!(state_first["value_id"], state_second["value_id"]);
    assert_eq!(
        state_first["property_pointer"],
        state_second["property_pointer"]
    );
}

#[test]
fn props_defaults_to_the_title_note_and_resolves_note_by_id_and_title() {
    let corpus = Corpus::new();
    let file = corpus.path("props.um");
    let by_default = run_json(&["props", &file]);
    let by_id = run_json(&["props", &file, "--note", &fixtures::ulid_seq(1)]);
    let by_title = run_json(&["props", &file, "--note", "issues"]);
    // the title note is order 0 — targeted without --note, by ULID and by
    // exact title alike
    assert_eq!(by_default, by_id);
    assert_eq!(by_default, by_title);
    assert_eq!(by_default["note_title"], "issues");

    // the canonical ULID travels in every response, also for a title lookup
    assert_eq!(by_title["note_id"], fixtures::ulid_seq(1));

    // --note on a plain page selects THAT page (still no rows there)
    let (_, error) = run_error(&["props", &file, "--note", "plain page"]);
    assert_eq!(error["error"]["code"], "PROPERTY_NOT_FOUND");

    // an ambiguous title fails with the matching ids, never a first match
    let dup_dir = TempDir::new().expect("tempdir");
    let dup_file = dup_dir.path().join("dup.um");
    let same_title = "twin";
    fixtures::write_um(
        &dup_file,
        &[
            fixtures::FixtureNote::plain_page(&fixtures::ulid_seq(4), same_title, &[]),
            fixtures::FixtureNote::plain_page(&fixtures::ulid_seq(5), same_title, &[]),
        ],
    )
    .expect("dup fixture");
    let (code, error) = run_error(&["props", &dup_file.to_string_lossy(), "--note", same_title]);
    assert_eq!(code, 3);
    assert_eq!(error["error"]["code"], "NOTE_NOT_FOUND");
    assert_eq!(error["error"]["details"]["ambiguous"], true);
}

#[test]
fn props_is_deterministic_across_runs() {
    let corpus = Corpus::new();
    let file = corpus.path("props.um");
    let first = insum().args(["props", &file]).output().expect("insum runs");
    let second = insum().args(["props", &file]).output().expect("insum runs");
    assert!(first.status.success());
    assert!(second.status.success());
    assert_eq!(first.stdout, second.stdout, "same input, same bytes");
}

// ---------------------------------------------------------------------------
// props: --get
// ---------------------------------------------------------------------------

#[test]
fn props_get_hits_exactly() {
    let corpus = Corpus::new();
    let file = corpus.path("props.um");
    let json = run_json(&["props", &file, "--get", "description"]);
    let properties = json["properties"].as_array().expect("array");
    assert_eq!(properties.len(), 1);
    assert_eq!(properties[0]["name"], "description");
    assert_eq!(properties[0]["value"], "Journal of the insum project");
    // the envelope stays intact around the single entry
    assert_eq!(json["note_id"], fixtures::ulid_seq(1));
    assert!(
        json["note_rev"]
            .as_str()
            .expect("note_rev")
            .starts_with("sha256:")
    );
}

/// --get matches ROWS, not flattened entries (review round-1 MINOR-1): a
/// list-typed row (one name, several notePropertyValue nodes) is one
/// legitimate property — the response carries ALL of its value entries
/// instead of a false ambiguity report.
#[test]
fn props_get_returns_all_values_of_one_list_row() {
    let corpus = Corpus::new();
    let file = corpus.path("props.um");
    let json = run_json(&["props", &file, "--get", "state"]);
    let properties = json["properties"].as_array().expect("array");
    assert_eq!(properties.len(), 2, "one list row, two value entries");
    assert_eq!(properties[0]["value"], "todo");
    assert_eq!(properties[1]["value"], "done");
    // both entries belong to ONE row — the same property_pointer, distinct
    // value nodes; the duplicate-name error would have pointed at two rows
    assert_eq!(properties[0]["property_pointer"], "/content/2");
    assert_eq!(properties[1]["property_pointer"], "/content/2");
    assert_ne!(
        properties[0]["value_pointer"],
        properties[1]["value_pointer"]
    );
}

#[test]
fn props_get_misses_with_property_not_found() {
    let corpus = Corpus::new();
    let file = corpus.path("props.um");
    let (code, error) = run_error(&["props", &file, "--get", "nosuch"]);
    assert_eq!(code, 3);
    assert_eq!(error["error"]["code"], "PROPERTY_NOT_FOUND");
    assert!(error["error"]["hint"].is_string());
}

#[test]
fn props_get_rejects_a_duplicated_name_as_ambiguous() {
    let dir = TempDir::new().expect("tempdir");
    let file = dir.path().join("dupprops.um");
    fixtures::write_um(
        &file,
        &[fixtures::FixtureNote::title_page_v3(
            &fixtures::ulid_seq(6),
            "dup",
            &[
                fixtures::note_property("state", &["todo"]),
                fixtures::note_property("state", &["done"]),
            ],
            &[],
        )],
    )
    .expect("fixture");
    let (code, error) = run_error(&["props", &file.to_string_lossy(), "--get", "state"]);
    assert_eq!(code, 3);
    assert_eq!(error["error"]["code"], "PROPERTY_NOT_FOUND");
    assert_eq!(error["error"]["details"]["ambiguous"], true);
    let pointers = error["error"]["details"]["pointers"]
        .as_array()
        .expect("pointers");
    assert_eq!(pointers.len(), 2);
    assert_eq!(pointers[0], "/content/1");
    assert_eq!(pointers[1], "/content/2");
}

// ---------------------------------------------------------------------------
// props: plain pages, v2 pages, .note files, degenerate containers
// ---------------------------------------------------------------------------

#[test]
fn props_reports_a_page_without_properties_as_a_normal_state() {
    let corpus = Corpus::new();
    // a v2 title page (noteSummary, no noteProperty) and a plain page are
    // both explicit PROPERTY_NOT_FOUND with the normal-state hint
    for name in ["plain.um", "props.um"] {
        let target: Vec<String> = if name == "props.um" {
            vec!["--note".to_owned(), "plain page".to_owned()]
        } else {
            vec![]
        };
        let mut args = vec!["props".to_owned(), corpus.path(name)];
        args.extend(target);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, error) = run_error(&args);
        assert_eq!(code, 3, "{name}");
        assert_eq!(error["error"]["code"], "PROPERTY_NOT_FOUND", "{name}");
        let hint = error["error"]["hint"].as_str().expect("hint");
        assert!(hint.contains("normal state"), "{name}: {hint}");
    }
}

#[test]
fn props_reads_a_note_file_as_the_single_document() {
    let corpus = Corpus::new();
    let file = corpus.path("single.note");
    let json = run_json(&["props", &file]);
    assert_eq!(json["note_id"], Value::Null, "a .note has no manifest id");
    assert_eq!(json["properties"][0]["name"], "status");
    assert_eq!(json["properties"][0]["value"], "in progress");

    // --note is meaningless for a single-document file
    let (_, error) = run_error(&["props", &file, "--note", "anything"]);
    assert_eq!(error["error"]["code"], "USAGE");

    // a plain .note is the same explicit normal-state error
    let plain_note = corpus.raw("plain.note");
    fixtures::write_legacy_note(
        &plain_note,
        &fixtures::plain_doc(&[fixtures::paragraph(&[fixtures::text("body")])]),
    )
    .expect("plain note");
    let (code, error) = run_error(&["props", &plain_note.to_string_lossy()]);
    assert_eq!(code, 3);
    assert_eq!(error["error"]["code"], "PROPERTY_NOT_FOUND");
}

#[test]
fn props_errors_on_an_empty_container_and_a_bad_format() {
    let dir = TempDir::new().expect("tempdir");
    let empty = dir.path().join("empty.um");
    fixtures::empty_container(&empty).expect("empty fixture");
    let (_, error) = run_error(&["props", &empty.to_string_lossy()]);
    assert_eq!(error["error"]["code"], "NOTE_NOT_FOUND");

    let corpus = Corpus::new();
    let (_, error) = run_error(&["props", &corpus.path("props.um"), "--format", "annotated"]);
    assert_eq!(error["error"]["code"], "USAGE");
}

#[test]
fn props_withholds_text_of_unknown_inline_nodes_with_a_warning() {
    let dir = TempDir::new().expect("tempdir");
    let file = dir.path().join("unknown.um");
    let mut doc = fixtures::title_doc_v3(
        "unknown inline",
        &[fixtures::note_property("broken", &["placeholder"])],
        &[],
    );
    // inject an unknown inline node into the value subtree: the text is
    // withheld whole — never a partial "before "
    doc["content"][1]["content"][1]["content"] = serde_json::json!([
        {"type": "text", "text": "before "},
        {"type": "futureWidget"}
    ]);
    fixtures::write_um(
        &file,
        &[fixtures::FixtureNote {
            id: fixtures::ulid_seq(7),
            title: "unknown inline".to_owned(),
            schema: "title",
            doc,
        }],
    )
    .expect("fixture");
    let json = run_json(&["props", &file.to_string_lossy()]);
    assert_eq!(
        json["properties"][0]["value"],
        Value::Null,
        "withheld whole"
    );
    let warnings = json["warnings"].as_array().expect("warnings");
    assert!(
        warnings
            .iter()
            .any(|warning| warning["code"] == "UNKNOWN_NODE_TYPE"),
        "no silent losses: {:?}",
        warnings
    );
}

#[test]
fn props_marks_do_not_alter_the_value_text() {
    let dir = TempDir::new().expect("tempdir");
    let file = dir.path().join("marks.um");
    let mut doc = fixtures::title_doc_v3(
        "marked",
        &[fixtures::note_property("marked", &["placeholder"])],
        &[],
    );
    doc["content"][1]["content"][1]["content"] = serde_json::json!([
        {"type": "text", "text": "plain "},
        {"type": "text", "marks": [{"type": "strong"}], "text": "bold "},
        {"type": "hardBreak"},
        {"type": "text", "text": "after"}
    ]);
    fixtures::write_um(
        &file,
        &[fixtures::FixtureNote {
            id: fixtures::ulid_seq(8),
            title: "marked".to_owned(),
            schema: "title",
            doc,
        }],
    )
    .expect("fixture");
    let json = run_json(&["props", &file.to_string_lossy()]);
    assert_eq!(json["properties"][0]["value"], "plain bold \nafter");
}

// ---------------------------------------------------------------------------
// ls --with-properties --with-rev
// ---------------------------------------------------------------------------

#[test]
fn ls_file_listing_gains_note_rev_and_properties_with_the_flags() {
    let corpus = Corpus::new();
    let file = corpus.path("props.um");

    // both flags: every descriptor gains note_rev + properties
    let json = run_json(&["ls", "--with-properties", "--with-rev", &file]);
    let notes = json["notes"].as_array().expect("notes");
    assert_eq!(notes.len(), 2);
    assert!(
        notes[0]["note_rev"]
            .as_str()
            .expect("rev")
            .starts_with("sha256:")
    );
    let properties = notes[0]["properties"].as_array().expect("properties");
    assert_eq!(properties.len(), 3);
    assert_eq!(properties[0]["name"], "description");
    assert_eq!(properties[0]["value"], "Journal of the insum project");
    // the manifest fields stay on the descriptor
    assert_eq!(notes[0]["title"], "issues");

    // --with-rev alone: revisions without properties
    let json = run_json(&["ls", "--with-rev", &file]);
    let notes = json["notes"].as_array().expect("notes");
    assert!(notes[0]["note_rev"].is_string());
    assert!(notes[0].get("properties").is_none());

    // --with-properties alone: summaries without revisions
    let json = run_json(&["ls", "--with-properties", &file]);
    let notes = json["notes"].as_array().expect("notes");
    assert!(notes[0].get("note_rev").is_none());
    assert_eq!(notes[0]["properties"][0]["name"], "description");

    // a v2 page shows an empty summary — a scan tolerates empty pages
    let plain = corpus.path("plain.um");
    let json = run_json(&["ls", "--with-properties", &plain]);
    assert_eq!(json["notes"][0]["properties"], serde_json::json!([]));

    // a .note file listing gains properties; note_rev was always there
    let note = corpus.path("single.note");
    let json = run_json(&["ls", "--with-properties", &note]);
    assert_eq!(json["properties"][0]["name"], "status");
    assert!(
        json["note_rev"]
            .as_str()
            .expect("rev")
            .starts_with("sha256:")
    );
}

#[test]
fn ls_directory_listing_gains_note_details_with_the_flags() {
    let corpus = Corpus::new();
    let dir = corpus.dir.path().to_string_lossy().into_owned();
    let json = run_json(&["ls", "--with-properties", "--with-rev", &dir]);
    assert_eq!(json["complete"], true);
    let entries = json["entries"].as_array().expect("entries");

    let notebook = entries
        .iter()
        .find(|entry| entry["kind"] == "notebook" && entry["path"] == corpus.path("props.um"))
        .expect("props.um entry");
    // the cheap-listing fields stay, the details are additive
    assert_eq!(notebook["notes"], 2);
    let details = notebook["note_details"].as_array().expect("note_details");
    assert_eq!(details.len(), 2);
    assert_eq!(details[0]["id"], fixtures::ulid_seq(1));
    assert!(
        details[0]["note_rev"]
            .as_str()
            .expect("rev")
            .starts_with("sha256:")
    );
    assert_eq!(details[0]["properties"][0]["name"], "description");
    // the plain page: present, empty — never silently dropped
    assert_eq!(details[1]["properties"], serde_json::json!([]));

    let note = entries
        .iter()
        .find(|entry| entry["kind"] == "note" && entry["path"] == corpus.path("single.note"))
        .expect("single.note entry");
    assert!(note["note_rev"].is_string());
    assert_eq!(note["properties"][0]["name"], "status");
}

#[test]
fn ls_without_the_flags_keeps_the_plain_listing_shape() {
    // structural regression: no flag, no new keys; the exact bytes are
    // pinned by ls_without_flags_matches_the_golden_bytes (and were
    // independently compared against a master worktree build)
    let corpus = Corpus::new();
    let file = corpus.path("props.um");

    let file_listing = run_json(&["ls", &file]);
    let expected_file_keys = [
        "api_version",
        "bytes",
        "features",
        "file_rev",
        "format",
        "kind",
        "mtime",
        "notes",
        "parts",
        "path",
        "type",
    ];
    let mut actual_file_keys: Vec<&str> = file_listing
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    actual_file_keys.sort_unstable();
    assert_eq!(actual_file_keys, expected_file_keys);
    for note in file_listing["notes"].as_array().expect("notes") {
        assert!(note.get("note_rev").is_none());
        assert!(note.get("properties").is_none());
        assert!(note.get("properties_incomplete").is_none());
    }

    let dir = corpus.dir.path().to_string_lossy().into_owned();
    let dir_listing = run_json(&["ls", &dir]);
    let expected_dir_keys = [
        "api_version",
        "complete",
        "entries",
        "kind",
        "path",
        "skipped",
    ];
    let mut actual_dir_keys: Vec<&str> = dir_listing
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    actual_dir_keys.sort_unstable();
    assert_eq!(actual_dir_keys, expected_dir_keys);
    for entry in dir_listing["entries"].as_array().expect("entries") {
        assert!(entry.get("note_details").is_none());
        assert!(entry.get("properties").is_none());
        assert!(entry.get("note_rev").is_none(), "{entry:?}");
        if entry["kind"] == "notebook" {
            // a notebook entry keeps the note COUNT, not details
            assert!(entry["notes"].is_u64());
        }
    }

    // a .note file listing keeps its exact shape (note_rev was always there)
    let note_listing = run_json(&["ls", &corpus.path("single.note")]);
    let mut actual_note_keys: Vec<&str> = note_listing
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    actual_note_keys.sort_unstable();
    assert_eq!(
        actual_note_keys,
        [
            "api_version",
            "bytes",
            "file_rev",
            "kind",
            "legacy",
            "mtime",
            "note_rev",
            "path"
        ]
    );
}

/// Byte-level regression gate for `ls` WITHOUT the flags (the review
/// round-1 MINOR-3): the committed golden files pin the exact stdout bytes
/// of three listing shapes over deterministic fixtures (fixed ZIP
/// timestamps, sequential ULIDs). Only two fields are normalized before
/// the comparison — `path` (a per-run temp directory, OS-specific) and
/// `mtime` (the wall clock of the fixture write); every other byte of the
/// response — keys, values, ordering, formatting — must match, so a change
/// to the plain listing under the flags-off path breaks this test in CI.
///
/// Regenerate the goldens with `INSUM_UPDATE_GOLDEN=1 cargo test --test
/// props_cmd` and commit the diff (it should be exactly the intentional
/// listing change). Windows-safe: the output is one JSON document with `/`
/// paths, and the comparison strips `\r` from the golden bytes, so an
/// autocrlf checkout cannot fail the gate.
#[test]
fn ls_without_flags_matches_the_golden_bytes() {
    let dir = TempDir::new().expect("tempdir");
    fixtures::minimal_container(&dir.path().join("minimal.um")).expect("minimal fixture");
    fixtures::write_legacy_note(
        &dir.path().join("legacy.note"),
        &fixtures::plain_doc(&[fixtures::paragraph(&[fixtures::text("Legacy note body")])]),
    )
    .expect("legacy fixture");
    let minimal = dir.path().join("minimal.um");
    let legacy = dir.path().join("legacy.note");
    let root = dir.path().to_path_buf();
    let cases: [(&str, Vec<String>); 3] = [
        (
            "ls_notebook_file.golden",
            vec!["ls".to_owned(), minimal.to_string_lossy().into_owned()],
        ),
        (
            "ls_directory.golden",
            vec!["ls".to_owned(), root.to_string_lossy().into_owned()],
        ),
        (
            "ls_note_file.golden",
            vec!["ls".to_owned(), legacy.to_string_lossy().into_owned()],
        ),
    ];
    for (golden_name, args) in &cases {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = insum().args(&args).output().expect("insum runs");
        assert!(
            output.status.success(),
            "ls {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let actual = normalize_listing(&output.stdout);
        let golden_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("goldens")
            .join(golden_name);
        if std::env::var_os("INSUM_UPDATE_GOLDEN").is_some() {
            if let Some(parent) = golden_path.parent() {
                fs::create_dir_all(parent).expect("goldens directory");
            }
            fs::write(&golden_path, &actual).expect("golden write");
            continue;
        }
        let expected: Vec<u8> = fs::read(&golden_path)
            .unwrap_or_else(|err| panic!("golden {golden_name} reads: {err}"))
            .into_iter()
            .filter(|byte| *byte != b'\r')
            .collect();
        assert_eq!(
            String::from_utf8_lossy(&actual),
            String::from_utf8_lossy(&expected),
            "ls without the flags drifted from the committed golden ({golden_name}); if the change is intentional, regenerate with INSUM_UPDATE_GOLDEN=1 and commit"
        );
    }
}

/// The only run-dependent fields of a listing are the user path (a temp
/// directory here) and the file mtime; both are replaced with markers and
/// the document is re-serialized compactly — the same rendering the CLI
/// uses, so the re-serialization is byte-faithful.
fn normalize_listing(stdout: &[u8]) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(stdout).expect("one JSON document");
    normalize_paths_and_mtime(&mut value);
    let mut text = serde_json::to_string(&value).expect("re-serialize");
    text.push('\n');
    text.into_bytes()
}

fn normalize_paths_and_mtime(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if map
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(is_run_specific_path)
            {
                map.insert("path".to_owned(), Value::String("<PATH>".to_owned()));
            }
            if map.contains_key("mtime") {
                map.insert("mtime".to_owned(), Value::String("<MTIME>".to_owned()));
            }
            for child in map.values_mut() {
                normalize_paths_and_mtime(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize_paths_and_mtime),
        _ => {}
    }
}

/// Only ABSOLUTE paths are run-specific (the temp directory: a leading `/`
/// on unix, a `C:\`-style drive prefix on windows). Manifest payload paths
/// like `notes/<id>.json` are relative and deterministic — they stay
/// byte-pinned in the golden.
fn is_run_specific_path(text: &str) -> bool {
    if text.starts_with('/') {
        return true;
    }
    let mut chars = text.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic()
    )
}

#[test]
fn ls_directory_with_flags_stays_best_effort_on_broken_files() {
    let dir = TempDir::new().expect("tempdir");
    fixtures::write_um(
        &dir.path().join("good.um"),
        &[fixtures::FixtureNote::title_page_v3(
            &fixtures::ulid_seq(9),
            "good",
            &[fixtures::note_property("status", &["ok"])],
            &[],
        )],
    )
    .expect("good fixture");
    fs::write(dir.path().join("broken.um"), b"not a zip at all").expect("broken fixture");
    let json = run_json(&[
        "ls",
        "--with-properties",
        "--with-rev",
        dir.path().to_string_lossy().as_ref(),
    ]);
    let entries = json["entries"].as_array().expect("entries");
    let broken = entries
        .iter()
        .find(|entry| entry["path"].as_str().map(|p| p.ends_with("broken.um")) == Some(true))
        .expect("broken entry");
    assert_eq!(broken["kind"], "invalid");
    assert!(broken["error"]["code"].is_string());
    let good = entries
        .iter()
        .find(|entry| entry["path"].as_str().map(|p| p.ends_with("good.um")) == Some(true))
        .expect("good entry");
    assert_eq!(good["note_details"][0]["properties"][0]["value"], "ok");
}

/// A `.note` inside a directory whose payload cannot be parsed becomes an
/// invalid entry under the flags (the deep read widens the failure surface)
/// while the flagless listing classifies it fine. Complete tracks `skipped`
/// only — the plain-listing convention an augmented listing keeps.
#[test]
fn ls_directory_deep_read_surfaces_unreadable_note_payloads() {
    let dir = TempDir::new().expect("tempdir");
    let note_path = dir.path().join("torn.note");
    fs::write(&note_path, b"{ not json").expect("torn fixture");
    // without flags: classified as a note (bytes only)
    let json = run_json(&["ls", dir.path().to_string_lossy().as_ref()]);
    let entries = json["entries"].as_array().expect("entries");
    assert_eq!(entries[0]["kind"], "note");
    // with flags: the payload read fails -> invalid entry
    let json = run_json(&[
        "ls",
        "--with-properties",
        dir.path().to_string_lossy().as_ref(),
    ]);
    let entries = json["entries"].as_array().expect("entries");
    assert_eq!(entries[0]["kind"], "invalid");
    assert_eq!(entries[0]["error"]["code"], "NOTE_JSON_INVALID");
}

/// Paths in the output always render with `/`, whatever the host separator
/// (CI runs on three OSes; expectations never depend on `\`).
#[test]
fn ls_and_props_render_paths_with_forward_slashes() {
    let corpus = Corpus::new();
    let file = corpus.path("props.um");
    assert!(!file.contains('\\'));
    let json = run_json(&["props", &file]);
    assert_eq!(json["path"], file);
    let json = run_json(&["ls", "--with-rev", &file]);
    assert_eq!(json["path"], file);
}

/// The source files must not be touched: read-only commands hash-identical
/// before and after the runs of this suite (checked on a copy).
#[test]
fn read_only_commands_leave_the_source_bytes_unchanged() {
    let corpus = Corpus::new();
    let file = corpus.raw("props.um");
    let before = sha256(&file);
    let _ = run_json(&["props", &file.to_string_lossy()]);
    let _ = run_json(&[
        "ls",
        "--with-properties",
        "--with-rev",
        &file.to_string_lossy(),
    ]);
    let _ = run_json(&["ls", &file.to_string_lossy()]);
    let after = sha256(&file);
    assert_eq!(before, after, "props/ls never write");
}

/// Cross-platform SHA-256 via the `sha2` dev-dependency: tests never spawn
/// external binaries — `shasum` is a Perl script that Windows runners do
/// not resolve (review round-1 MAJOR-1; platform lesson V-04).
fn sha256(path: &Path) -> String {
    use sha2::Digest as _;
    let bytes = std::fs::read(path).expect("source file reads");
    let mut hasher = sha2::Sha256::new();
    hasher.update(&bytes);
    format!("{:x}", hasher.finalize())
}
