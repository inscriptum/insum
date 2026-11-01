//! Stage A acceptance tests — the 13 criteria of spec §15.1 plus the error
//! contract of §5.6/§11. Fixtures come from `insum-test-utils` and are
//! generated into a temp directory per test; nothing outside it is touched.

use std::fs;
use std::path::Path;
use std::process::Output as ProcessOutput;

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

fn stderr_json(output: &ProcessOutput) -> Value {
    assert!(
        output.stdout.is_empty(),
        "stdout stays empty on error (§5.3)"
    );
    serde_json::from_slice(&output.stderr).expect("stderr is one JSON error object")
}

/// A full corpus: three containers (minimal, empty, regular), two `.note`
/// flavors and broken inputs, in one directory (§15.1 №1–2, №10).
struct Corpus {
    dir: TempDir,
}

impl Corpus {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path();
        fixtures::minimal_container(&path.join("minimal.um")).expect("minimal fixture");
        fixtures::empty_container(&path.join("empty.um")).expect("empty fixture");
        fixtures::regular_container(&path.join("regular.um")).expect("regular fixture");
        let doc = fixtures::plain_doc(&[
            fixtures::heading(1, "Standalone"),
            fixtures::paragraph(&[fixtures::text("Legacy note body")]),
        ]);
        fixtures::write_legacy_note(&path.join("legacy.note"), &doc).expect("legacy fixture");
        fixtures::write_envelope_note(&path.join("envelope.note"), &doc).expect("envelope fixture");
        fs::write(path.join("broken.um"), b"this is not a ZIP archive").expect("broken fixture");
        Self { dir }
    }

    fn path(&self, name: &str) -> String {
        self.dir.path().join(name).to_string_lossy().into_owned()
    }

    fn hash_all(&self) -> Vec<(String, [u8; 32])> {
        let mut hashes = Vec::new();
        for entry in fs::read_dir(self.dir.path()).expect("read_dir") {
            let entry = entry.expect("entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let bytes = fs::read(entry.path()).expect("read");
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&sha256(&bytes));
            hashes.push((name, hash));
        }
        hashes.sort();
        hashes
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    // tiny duplicate of the algorithm for test independence: FIPS 180-4 over
    // a single block is overkill to hand-roll, so reuse insum-core.
    let digest = insum_core::container::sha256_hex(bytes);
    let mut out = [0u8; 32];
    for (index, pair) in digest.as_bytes().chunks(2).enumerate() {
        out[index] = u8::from_str_radix(std::str::from_utf8(pair).expect("hex pair"), 16)
            .expect("hex digit");
    }
    out
}

// ---------------------------------------------------------------------------
// №1 — every container flavor is read by all six commands
// ---------------------------------------------------------------------------

#[test]
fn all_commands_read_minimal_empty_and_regular_containers() {
    let corpus = Corpus::new();
    for name in ["minimal.um", "empty.um", "regular.um"] {
        let file = corpus.path(name);
        let ls = run_json(&["ls", &file]);
        assert_eq!(ls["kind"], "notebook", "{name}");
        assert!(
            ls["file_rev"].as_str().is_some(),
            "{name}: file_rev present"
        );

        let tree = run_json(&["tree", &file]);
        assert_eq!(tree["api_version"], 1);
        for note in tree["notes"].as_array().expect("notes array") {
            assert_eq!(note["complete"], json_true());
        }

        run_json(&["read", &file]);
        // annotated output is a text projection, not JSON (§6.5)
        let annotated = insum()
            .args(["read", "--format", "annotated", &file])
            .output()
            .expect("insum runs");
        assert!(annotated.status.success());
        run_json(&["validate", &file]);
        run_json(&["search", "-F", "deployment", &file]);
    }
    // block get on the empty container has no notes to address; on others:
    let tree = run_json(&["tree", &corpus.path("minimal.um")]);
    let note_id = tree["notes"][0]["note_id"].as_str().unwrap().to_owned();
    let pointer = tree["notes"][0]["blocks"][0]["pointer"]
        .as_str()
        .unwrap()
        .to_owned();
    let block = run_json(&[
        "block",
        "get",
        &corpus.path("minimal.um"),
        "--note",
        &note_id,
        "--pointer",
        &pointer,
    ]);
    assert_eq!(block["block"]["pointer"], Value::String(pointer));
}

fn json_true() -> Value {
    Value::Bool(true)
}

// ---------------------------------------------------------------------------
// №2 — legacy .note reads without envelope and without IDs
// ---------------------------------------------------------------------------

#[test]
fn legacy_note_reads_without_envelope_or_ids() {
    let corpus = Corpus::new();
    let file = corpus.path("legacy.note");

    let ls = run_json(&["ls", &file]);
    assert_eq!(ls["legacy"], Value::Bool(true));
    assert!(ls["note_rev"].as_str().is_some());

    let tree = run_json(&["tree", &file]);
    assert_eq!(tree["notes"][0]["note_title"], "Standalone"); // first heading (№9)
    assert!(
        tree["notes"][0]["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block["id"] == Value::Null)
    );

    let envelope_ls = run_json(&["ls", &corpus.path("envelope.note")]);
    assert_eq!(envelope_ls["legacy"], Value::Bool(false));
    assert_eq!(envelope_ls["format"], "inscriptum-note");
}

// ---------------------------------------------------------------------------
// №3 + №12 — directory listing: no file_rev, no payload access
// ---------------------------------------------------------------------------

#[test]
fn directory_listing_has_no_file_rev() {
    let corpus = Corpus::new();
    let ls = run_json(&["ls", corpus.dir.path().to_string_lossy().as_ref()]);
    assert_eq!(ls["complete"], json_true());
    let entries = ls["entries"].as_array().expect("entries");
    assert!(entries.len() >= 6);
    for entry in entries {
        assert!(
            entry.get("file_rev").is_none(),
            "dir listing must not compute file_rev: {entry}"
        );
        // classification only: notebooks expose the manifest note count
        if entry["kind"] == "notebook" {
            assert!(entry["notes"].as_u64().is_some());
        }
    }
    let paths: Vec<&str> = entries.iter().filter_map(|e| e["path"].as_str()).collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "entries are sorted by path");
}

// ---------------------------------------------------------------------------
// №5 + №6 — pointers are correct; block get returns the node at the pointer
// ---------------------------------------------------------------------------

#[test]
fn block_get_returns_the_node_at_the_pointer() {
    let corpus = Corpus::new();
    let file = corpus.path("regular.um");
    let tree = run_json(&["tree", &file]);

    // resolve every tree pointer against the actual payload via block get
    for note in tree["notes"].as_array().unwrap() {
        let note_id = note["note_id"].as_str().unwrap();
        for block in note["blocks"].as_array().unwrap() {
            let pointer = block["pointer"].as_str().unwrap();
            let got = run_json(&[
                "block",
                "get",
                &file,
                "--note",
                note_id,
                "--pointer",
                pointer,
            ]);
            let node = &got["block"]["node"];
            assert_eq!(node["type"], block["type"], "pointer {pointer}");
            assert!(
                node.get("attrs")
                    .and_then(|attrs| attrs.get("id"))
                    .is_none(),
                "wire form excludes attrs.id"
            );
            if block["text"].is_string() {
                let text = flatten_inline_text(node);
                assert_eq!(text.trim(), block["text"].as_str().unwrap().trim());
            }
        }
    }
}

fn flatten_inline_text(node: &Value) -> String {
    let mut out = String::new();
    if let Some(children) = node["content"].as_array() {
        for child in children {
            if child["type"] == "text" {
                out.push_str(child["text"].as_str().unwrap_or_default());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// №7 — pagination: whole blocks, cursor bound to note_rev
// ---------------------------------------------------------------------------

#[test]
fn pagination_resumes_and_detects_stale_revisions() {
    let corpus = Corpus::new();
    let file = corpus.path("regular.um");

    let first = run_json(&["tree", &file, "--max-bytes", "300"]);
    let note = &first["notes"][0];
    assert_eq!(
        note["complete"],
        Value::Bool(false),
        "budget forces pagination"
    );
    assert!(note["next_cursor"].is_string());
    let emitted: Vec<&str> = note["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block["pointer"].as_str())
        .collect();
    assert!(!emitted.is_empty());

    // resume from the cursor until the note is complete; each page emits
    // whole blocks and the cursor advances by at least one block
    let mut emitted: Vec<String> = note["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block["pointer"].as_str().map(str::to_owned))
        .collect();
    let mut cursor = note["next_cursor"].as_str().unwrap().to_owned();
    let mut complete = false;
    for _ in 0..50 {
        let page = run_json(&["tree", &file, "--max-bytes", "300", "--cursor", &cursor]);
        let page_note = &page["notes"][0];
        for block in page_note["blocks"].as_array().unwrap() {
            let pointer = block["pointer"].as_str().unwrap().to_owned();
            assert!(!emitted.contains(&pointer), "pages never repeat a block");
            emitted.push(pointer);
        }
        if page_note["complete"] == json_true() {
            assert!(page_note["next_cursor"].is_null());
            complete = true;
            break;
        }
        cursor = page_note["next_cursor"].as_str().unwrap().to_owned();
    }
    assert!(complete, "pagination terminates");

    // a cursor from another revision is CURSOR_STALE (§6.2): rewrite the
    // container with different note content so note_rev changes
    let changed = TempDir::new().expect("tempdir");
    let notes = [fixtures::FixtureNote::plain_page(
        &fixtures::ulid_seq(9),
        "Changed content",
        &[fixtures::paragraph(&[fixtures::text("Different body")])],
    )];
    fixtures::write_um(&changed.path().join("regular.um"), &notes).expect("rewrite");
    let output = insum()
        .args([
            "tree",
            &changed.path().join("regular.um").to_string_lossy(),
            "--max-bytes",
            "300",
            "--cursor",
            &cursor,
        ])
        .output()
        .expect("insum runs");
    assert_eq!(output.status.code(), Some(7), "CURSOR_STALE exits 7");
    let error = stderr_json(&output);
    assert_eq!(error["error"]["code"], "CURSOR_STALE");
}

// ---------------------------------------------------------------------------
// №8 — search hits only inline-content blocks, no ancestor duplicates
// ---------------------------------------------------------------------------

#[test]
fn search_matches_inline_blocks_without_ancestor_duplicates() {
    let corpus = Corpus::new();
    let result = run_json(&["search", "-F", "Установить", &corpus.path("regular.um")]);
    assert_eq!(result["complete"], json_true());
    let matches = result["matches"].as_array().expect("matches");
    assert!(!matches.is_empty());
    for hit in matches {
        assert_eq!(
            hit["type"], "paragraph",
            "the paragraph directly holds the text"
        );
    }
    let pointers: Vec<&str> = matches
        .iter()
        .filter_map(|hit| hit["pointer"].as_str())
        .collect();
    let unique: std::collections::BTreeSet<&str> = pointers.iter().copied().collect();
    assert_eq!(
        pointers.len(),
        unique.len(),
        "one entry per match, no duplicates"
    );

    // regex mode with case-insensitivity (§6.4)
    let regex = run_json(&[
        "search",
        "-i",
        "deployment.*приложени",
        &corpus.path("regular.um"),
    ]);
    assert_eq!(regex["mode"], "regex");
    assert!(!regex["matches"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// №9 — note_title: manifest title first, else first heading, else null
// ---------------------------------------------------------------------------

#[test]
fn note_title_follows_the_spec_rule() {
    let corpus = Corpus::new();
    let tree = run_json(&["tree", &corpus.path("regular.um")]);
    let titles: Vec<&str> = tree["notes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|note| note["note_title"].as_str())
        .collect();
    assert!(
        titles.contains(&"Развёртывание"),
        "manifest title wins: {titles:?}"
    );
    assert!(titles.contains(&"Инструкция по deployment приложения"));
}

// ---------------------------------------------------------------------------
// №10 — skipped/invalid entries make complete=false, structure documented
// ---------------------------------------------------------------------------

#[test]
fn broken_files_surface_as_invalid_or_skipped() {
    let corpus = Corpus::new();
    let ls = run_json(&["ls", corpus.dir.path().to_string_lossy().as_ref()]);
    let invalid: Vec<&Value> = ls["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "invalid")
        .collect();
    assert_eq!(invalid.len(), 1, "broken.um is one invalid entry");
    assert_eq!(invalid[0]["error"]["code"], "NOT_A_ZIP");
    assert!(invalid[0]["error"]["message"].is_string());
    assert_eq!(invalid[0]["mtime"], Value::Null);
    // ls recovers: skipped is empty, complete stays true
    assert_eq!(ls["complete"], json_true());

    // search reports the unreadable file in `skipped` and lowers `complete`
    let search = run_json(&[
        "search",
        "-F",
        "Legacy",
        corpus.dir.path().to_string_lossy().as_ref(),
    ]);
    assert_eq!(search["complete"], Value::Bool(false));
    assert_eq!(search["skipped"].as_array().unwrap().len(), 1);
    assert_eq!(search["skipped"][0]["error"]["code"], "NOT_A_ZIP");
    assert!(!search["matches"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// №11 — nothing writes; №13 — deterministic output
// ---------------------------------------------------------------------------

#[test]
fn read_only_and_deterministic() {
    let corpus = Corpus::new();
    let before = corpus.hash_all();

    let dir_arg = corpus.dir.path().to_string_lossy().into_owned();
    let _ = run_json(&["ls", &dir_arg]);
    for name in ["minimal.um", "empty.um", "regular.um"] {
        let _ = run_json(&["tree", &corpus.path(name)]);
        let _ = run_json(&["read", &corpus.path(name)]);
        let _ = insum()
            .args(["read", "--format", "annotated", &corpus.path(name)])
            .output()
            .expect("annotated read runs");
        let _ = run_json(&["validate", &corpus.path(name)]);
        let _ = run_json(&["search", "-F", "the", &corpus.path(name)]);
    }
    let after = corpus.hash_all();
    assert_eq!(
        before, after,
        "stage A never modifies input files (§15.1 №11)"
    );

    // determinism (№13): same input, same JSON — twice
    for args in [
        vec!["tree", corpus.path("regular.um").as_str()],
        vec![
            "search",
            "-F",
            "deployment",
            corpus.path("regular.um").as_str(),
        ],
        vec!["read", corpus.path("regular.um").as_str()],
        vec!["validate", dir_arg.as_str()],
    ] {
        let first = insum().args(&args).output().unwrap();
        let second = insum().args(&args).output().unwrap();
        assert_eq!(first.stdout, second.stdout, "deterministic: {args:?}");
    }
}

// ---------------------------------------------------------------------------
// Limits (§5.5), errors (§11), exit codes (§5.6)
// ---------------------------------------------------------------------------

#[test]
fn limits_and_error_codes_are_enforced() {
    let corpus = Corpus::new();

    // LIMIT_EXCEEDED via a tiny UM_MAX_ENTRY_BYTES (checked on actual data)
    let output = insum()
        .args(["read", &corpus.path("minimal.um")])
        .env("UM_MAX_ENTRY_BYTES", "10")
        .output()
        .expect("insum runs");
    assert_eq!(output.status.code(), Some(5), "exit 5 for LIMIT_EXCEEDED");
    let error = stderr_json(&output);
    assert_eq!(error["error"]["code"], "LIMIT_EXCEEDED");

    // PATH_NOT_FOUND exits 3
    let output = insum()
        .args(["tree", &corpus.path("missing.um")])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(stderr_json(&output)["error"]["code"], "PATH_NOT_FOUND");

    // NOT_A_ZIP exits 4
    let output = insum()
        .args(["tree", &corpus.path("broken.um")])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(stderr_json(&output)["error"]["code"], "NOT_A_ZIP");

    // block get on .um without --note → USAGE exit 2
    let output = insum()
        .args([
            "block",
            "get",
            &corpus.path("regular.um"),
            "--pointer",
            "/content/0",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stderr_json(&output)["error"]["code"], "USAGE");

    // --id on stage A → USAGE exit 2
    let output = insum()
        .args([
            "block",
            "get",
            &corpus.path("regular.um"),
            "--note",
            "x",
            "--id",
            "a1b2c3d4e5",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));

    // validate --schema with a missing artifact → SCHEMA_UNAVAILABLE exit 6
    let output = insum()
        .args([
            "validate",
            &corpus.path("minimal.um"),
            "--schema",
            "/nonexistent/schema.json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(6));
    assert_eq!(stderr_json(&output)["error"]["code"], "SCHEMA_UNAVAILABLE");
}

// ---------------------------------------------------------------------------
// validate: block ID rules for marked documents (§6.6)
// ---------------------------------------------------------------------------

#[test]
fn validate_checks_block_ids_only_when_marked() {
    let corpus = Corpus::new();
    // legacy documents without the marker are fine without IDs (§2.1)
    let ok = run_json(&["validate", &corpus.path("regular.um")]);
    assert_eq!(ok["validation_level"], "structural");
    assert_eq!(ok["valid"], json_true());

    // a marked document with a duplicate/invalid/missing ID fails
    let dir = TempDir::new().expect("tempdir");
    let doc = serde_json::json!({
        "type": "noteDoc",
        "content": [
            {"type": "paragraph", "attrs": {"id": "a1b2c3d4e5"}, "content": [{"type": "text", "text": "one"}]},
            {"type": "paragraph", "attrs": {"id": "a1b2c3d4e5"}, "content": [{"type": "text", "text": "two"}]},
            {"type": "paragraph", "attrs": {"id": "SHORT"}, "content": [{"type": "text", "text": "three"}]},
            {"type": "paragraph", "content": [{"type": "text", "text": "four"}]}
        ]
    });
    let envelope = serde_json::json!({
        "format": "inscriptum-note",
        "version": 1,
        "features": {"block_ids": 1},
        "schema": {"id": "inscriptum", "schema_version": 1, "schema_hash": "sha256:00"},
        "document": doc
    });
    let marked = dir.path().join("marked.note");
    fs::write(&marked, envelope.to_string()).expect("write marked fixture");

    let result = run_json(&["validate", marked.to_string_lossy().as_ref()]);
    assert_eq!(result["valid"], Value::Bool(false));
    let notes = result["files"][0]["notes"].as_array().unwrap();
    let codes: Vec<&str> = notes[0]["errors"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|err| err["code"].as_str())
        .collect();
    assert!(codes.contains(&"BLOCK_ID_DUPLICATE"));
    assert!(codes.contains(&"BLOCK_ID_INVALID"));
    assert!(codes.contains(&"BLOCK_ID_MISSING"));
}

// ---------------------------------------------------------------------------
// ls of a .um file reports descriptors without payload decompression (№4)
// ---------------------------------------------------------------------------

#[test]
fn ls_file_reports_manifest_metadata() {
    let corpus = Corpus::new();
    let ls = run_json(&["ls", &corpus.path("regular.um")]);
    assert_eq!(ls["format"], "um");
    assert_eq!(ls["type"], "notepad");
    assert!(ls["file_rev"].as_str().is_some());
    assert_eq!(ls["notes"].as_array().unwrap().len(), 2);
    let first = &ls["notes"][0];
    assert!(first["id"].is_string());
    assert_eq!(first["schema"], "title");
    assert!(first["path"].as_str().unwrap().starts_with("notes/"));
    // parts are empty for fixture containers without assets
    assert_eq!(ls["parts"], Value::Array(Vec::new()));
}

#[test]
fn annotated_projection_has_anchors_and_header() {
    let corpus = Corpus::new();
    let output = insum()
        .args(["read", "--format", "annotated", &corpus.path("regular.um")])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("<!-- um:api_version=1"));
    assert!(text.contains("<!-- um:pointer=\"/content/0\" -->"));
    assert!(text.contains("# Развёртывание"));
    assert!(text.contains("- Установить зависимости"));
    assert!(text.contains("```bash"));
}

#[test]
fn ls_respects_hidden_and_node_modules() {
    let corpus = Corpus::new();
    let hidden_dir = corpus.dir.path().join(".hidden");
    fs::create_dir_all(&hidden_dir).expect("mkdir");
    fixtures::minimal_container(&hidden_dir.join("secret.um")).expect("hidden fixture");
    let modules_dir = corpus.dir.path().join("node_modules");
    fs::create_dir_all(&modules_dir).expect("mkdir");
    fixtures::minimal_container(&modules_dir.join("dep.um")).expect("dep fixture");

    let plain = run_json(&["ls", corpus.dir.path().to_string_lossy().as_ref()]);
    let paths: Vec<&str> = plain["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["path"].as_str())
        .collect();
    assert!(
        paths
            .iter()
            .all(|path| !path.contains("/.hidden/") && !path.contains("node_modules"))
    );

    let with_hidden = run_json(&[
        "ls",
        "--hidden",
        corpus.dir.path().to_string_lossy().as_ref(),
    ]);
    let hidden_paths: Vec<&str> = with_hidden["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["path"].as_str())
        .collect();
    assert!(hidden_paths.iter().any(|path| path.contains("/.hidden/")));
    assert!(
        hidden_paths
            .iter()
            .all(|path| !path.contains("node_modules"))
    );
}

// ---------------------------------------------------------------------------
// Review-driven regressions
// ---------------------------------------------------------------------------

/// §5.4: note_rev hashes the RAW payload bytes — re-archiving the same
/// semantic document with different payload whitespace changes the revision.
#[test]
fn note_rev_follows_raw_payload_bytes() {
    let dir = TempDir::new().expect("tempdir");

    let compact = dir.path().join("compact.um");
    fixtures::write_um(
        &compact,
        &[fixtures::FixtureNote::plain_page(
            &fixtures::ulid_seq(3),
            "r",
            &doc_content(),
        )],
    )
    .unwrap();

    // same document, but serialized with pretty JSON inside the archive
    let pretty = dir.path().join("pretty.um");
    write_um_pretty_payload(&pretty);

    let rev_compact =
        run_json(&["tree", compact.to_string_lossy().as_ref()])["notes"][0]["note_rev"]
            .as_str()
            .unwrap()
            .to_owned();
    let rev_pretty = run_json(&["tree", pretty.to_string_lossy().as_ref()])["notes"][0]["note_rev"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(rev_compact, rev_pretty, "raw payload bytes feed note_rev");

    // stable across runs on identical bytes
    let again = run_json(&["tree", compact.to_string_lossy().as_ref()])["notes"][0]["note_rev"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(rev_compact, again);
}

fn doc_content() -> Vec<Value> {
    vec![fixtures::paragraph(&[fixtures::text("rev test")])]
}

/// A `.um` whose payload JSON is pretty-printed (different raw bytes).
fn write_um_pretty_payload(path: &Path) {
    use std::io::Write as _;
    let file = std::fs::File::create(path).expect("create");
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    let note_id = fixtures::ulid_seq(3);
    let manifest = format!(
        "{{\"format\":\"um\",\"version\":1,\"type\":\"notepad\",\"notes\":[{{\"id\":\"{note_id}\",\"path\":\"notes/{note_id}.json\",\"order\":0,\"title\":\"r\",\"schema\":\"plain\",\"schemaVersion\":2}}]}}"
    );
    zip.start_file("manifest.json", options).unwrap();
    zip.write_all(manifest.as_bytes()).unwrap();
    let payload = serde_json::to_string_pretty(&serde_json::json!({
        "type": "noteDoc",
        "content": [{"type": "paragraph", "content": [{"type": "text", "text": "rev test"}]}]
    }))
    .unwrap();
    zip.start_file(format!("notes/{note_id}.json"), options)
        .unwrap();
    zip.write_all(payload.as_bytes()).unwrap();
    zip.finish().unwrap();
}

/// §12: duplicate archive entries are rejected (zip 9 collapses them in
/// lookups, so the explicit probe is the only reliable guard).
#[test]
fn duplicate_archive_entries_are_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("dup.um");
    write_zip_with_duplicate(&path);

    let output = insum()
        .args(["tree", path.to_string_lossy().as_ref()])
        .output()
        .expect("insum runs");
    assert_eq!(output.status.code(), Some(4), "exit 4 for invalid data");
    let error = stderr_json(&output);
    assert_eq!(error["error"]["code"], "ARCHIVE_DUPLICATE_ENTRY");
}

fn write_zip_with_duplicate(path: &Path) {
    // zip 9's writer refuses duplicate names, so build the raw bytes: two
    // STOREd entries sharing one name, with a matching central directory.
    let name = b"manifest.json";
    let data = b"{\"format\":\"um\",\"version\":1,\"type\":\"notepad\",\"notes\":[]}";
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let mut offset = 0u32;
    for _ in 0..2 {
        let lfh_offset = offset;
        let mut lfh = Vec::new();
        lfh.extend(0x0403_4b50u32.to_le_bytes());
        lfh.extend(20u16.to_le_bytes()); // version needed
        lfh.extend(0u16.to_le_bytes()); // flags
        lfh.extend(0u16.to_le_bytes()); // method: store
        lfh.extend(0u16.to_le_bytes()); // mtime
        lfh.extend(0u16.to_le_bytes()); // mdate
        lfh.extend(0u32.to_le_bytes()); // crc (data is never read)
        lfh.extend((data.len() as u32).to_le_bytes());
        lfh.extend((data.len() as u32).to_le_bytes());
        lfh.extend((name.len() as u16).to_le_bytes());
        lfh.extend(0u16.to_le_bytes()); // extra len
        lfh.extend_from_slice(name);
        lfh.extend_from_slice(data);
        out.extend(&lfh);

        let mut cdh = Vec::new();
        cdh.extend(0x0201_4b50u32.to_le_bytes());
        cdh.extend(20u16.to_le_bytes()); // version made by
        cdh.extend(20u16.to_le_bytes()); // version needed
        cdh.extend(0u16.to_le_bytes()); // flags
        cdh.extend(0u16.to_le_bytes()); // method
        cdh.extend(0u16.to_le_bytes()); // mtime
        cdh.extend(0u16.to_le_bytes()); // mdate
        cdh.extend(0u32.to_le_bytes()); // crc
        cdh.extend((data.len() as u32).to_le_bytes());
        cdh.extend((data.len() as u32).to_le_bytes());
        cdh.extend((name.len() as u16).to_le_bytes());
        cdh.extend(0u16.to_le_bytes()); // extra
        cdh.extend(0u16.to_le_bytes()); // comment
        cdh.extend(0u16.to_le_bytes()); // disk start
        cdh.extend(0u16.to_le_bytes()); // internal attrs
        cdh.extend(0u32.to_le_bytes()); // external attrs
        cdh.extend(lfh_offset.to_le_bytes());
        cdh.extend_from_slice(name);
        central.extend(cdh);

        offset += lfh.len() as u32;
    }
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend(0u16.to_le_bytes()); // disk
    out.extend(0u16.to_le_bytes()); // cd disk
    out.extend(2u16.to_le_bytes()); // entries this disk
    out.extend(2u16.to_le_bytes()); // entries total
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(offset.to_le_bytes()); // cd offset
    out.extend(0u16.to_le_bytes()); // comment len
    std::fs::write(path, out).expect("write raw zip");
}

/// §6.2: a cursor issued with other --depth/--text-limit options is stale.
#[test]
fn cursor_rejects_changed_query_options() {
    let corpus = Corpus::new();
    let file = corpus.path("regular.um");
    let first = run_json(&["tree", &file, "--max-bytes", "300"]);
    let cursor = first["notes"][0]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();

    let output = insum()
        .args([
            "tree",
            &file,
            "--max-bytes",
            "300",
            "--depth",
            "2",
            "--cursor",
            &cursor,
        ])
        .output()
        .expect("insum runs");
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(stderr_json(&output)["error"]["code"], "CURSOR_STALE");
}

/// §6.2: a cursor position past the document is stale, not an empty page.
#[test]
fn cursor_past_the_end_is_stale() {
    let corpus = Corpus::new();
    let file = corpus.path("regular.um");
    let first = run_json(&["tree", &file, "--max-bytes", "300"]);
    let cursor = first["notes"][0]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();

    // decode, push bi far beyond the document, re-encode
    let decoded = base64url_decode(&cursor);
    let mut value: Value = serde_json::from_slice(&decoded).unwrap();
    value["bi"] = serde_json::json!(9999);
    let tampered = base64url_encode(value.to_string().as_bytes());

    let output = insum()
        .args(["tree", &file, "--max-bytes", "300", "--cursor", &tampered])
        .output()
        .expect("insum runs");
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(stderr_json(&output)["error"]["code"], "CURSOR_STALE");
}

fn base64url_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(triple >> 12 & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(triple >> 6 & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 63) as usize] as char);
        }
    }
    out
}

fn base64url_decode(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let bytes: Vec<usize> = text
        .bytes()
        .map(|b| {
            ALPHABET
                .iter()
                .position(|&a| a == b)
                .expect("cursor alphabet")
        })
        .collect();
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        match chunk.len() {
            4 => {
                let t = (chunk[0] << 18) | (chunk[1] << 12) | (chunk[2] << 6) | chunk[3];
                out.push((t >> 16) as u8);
                out.push((t >> 8) as u8);
                out.push(t as u8);
            }
            3 => {
                let t = (chunk[0] << 18) | (chunk[1] << 12) | (chunk[2] << 6);
                out.push((t >> 16) as u8);
                out.push((t >> 8) as u8);
            }
            2 => {
                let t = (chunk[0] << 18) | (chunk[1] << 12);
                out.push((t >> 16) as u8);
            }
            _ => panic!("bad cursor"),
        }
    }
    out
}

/// §6.4: search honors --max-bytes by truncating at whole matches.
#[test]
fn search_respects_max_bytes_budget() {
    let corpus = Corpus::new();
    let result = run_json(&[
        "search",
        "-F",
        "е",
        "--max-bytes",
        "300",
        &corpus.path("regular.um"),
    ]);
    assert_eq!(result["truncated"], Value::Bool(true));
    assert_eq!(result["complete"], Value::Bool(false));
    // whole matches only
    assert!(!result["matches"].as_array().unwrap().is_empty());
}

/// §6.6: --strict turns warnings into errors and flips `valid`.
#[test]
fn validate_strict_turns_warnings_into_errors() {
    let dir = TempDir::new().expect("tempdir");
    let doc = serde_json::json!({
        "type": "noteDoc",
        "content": [
            {"type": "paragraph", "content": [{"type": "text", "text": "ok"}]},
            {"type": "exoticUnknown", "content": [{"type": "text", "text": "?"}]}
        ]
    });
    let file = dir.path().join("unknown.note");
    fixtures::write_legacy_note(&file, &doc).expect("write");

    // without --strict: valid, with a warning
    let plain = run_json(&["validate", file.to_string_lossy().as_ref()]);
    assert_eq!(plain["valid"], Value::Bool(true));
    let warnings = &plain["files"][0]["notes"][0]["warnings"];
    assert_eq!(warnings.as_array().unwrap().len(), 1);

    // with --strict: invalid, error carries the warning text
    let strict = run_json(&["validate", "--strict", file.to_string_lossy().as_ref()]);
    assert_eq!(strict["valid"], Value::Bool(false));
    assert_eq!(strict["files"][0]["valid"], Value::Bool(false));
    let errors = strict["files"][0]["notes"][0]["errors"].as_array().unwrap();
    assert_eq!(errors[0]["code"], "NOTE_DOC_INVALID");
}

#[test]
fn search_in_a_note_file_and_directory_work() {
    let corpus = Corpus::new();
    let hits = run_json(&[
        "search",
        "-F",
        "Legacy",
        corpus.dir.path().to_string_lossy().as_ref(),
    ]);
    let found: Vec<&Value> = hits["matches"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|hit| hit["path"].as_str().unwrap().ends_with("legacy.note"))
        .collect();
    assert!(!found.is_empty());
    assert!(found[0]["note_id"].is_null(), ".note files have no note id");
}
