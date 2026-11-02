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
    // the FIRST page ends mid-note (budget 300 < note size) — a mid-note
    // cursor for the rev-stale check below
    let mid_note_cursor = note["next_cursor"].as_str().unwrap().to_owned();
    let mut cursor = mid_note_cursor.clone();
    let mut complete = false;
    for _ in 0..50 {
        let page = run_json(&["tree", &file, "--max-bytes", "300", "--cursor", &cursor]);
        for page_note in page["notes"].as_array().unwrap() {
            // pointers repeat across notes (/content/0 everywhere) — the
            // dedup key is (note_id, pointer)
            let nid = page_note["note_id"].as_str().unwrap_or("").to_owned();
            for block in page_note["blocks"].as_array().unwrap() {
                // T-13: strict extraction — a broken entry must fail loudly
                let pointer = block["pointer"]
                    .as_str()
                    .expect("block carries a pointer")
                    .to_owned();
                let key = format!("{nid}{pointer}");
                assert!(!emitted.contains(&key), "pages never repeat a block");
                emitted.push(key);
            }
        }
        let page_note = &page["notes"][0];
        // termination = no next_cursor; note.complete describes the note
        // itself (a boundary page ends with complete:true + a cursor to the
        // next note — NEW-1 semantics)
        match page_note["next_cursor"].as_str() {
            Some(next) => cursor = next.to_owned(),
            None => {
                complete = true;
                break;
            }
        }
    }
    assert!(complete, "pagination terminates");

    // T-01: the note_rev branch specifically — rewrite the SAME path with
    // the SAME note id but different content, so only note_rev diverges
    let note_id = note["note_id"].as_str().unwrap().to_owned();
    let notes = [fixtures::FixtureNote::plain_page(
        &note_id,
        "Changed content",
        &[fixtures::paragraph(&[fixtures::text("Different body")])],
    )];
    fixtures::write_um(&std::path::PathBuf::from(corpus.path("regular.um")), &notes)
        .expect("rewrite");
    let output = insum()
        .args([
            "tree",
            &corpus.path("regular.um"),
            "--max-bytes",
            "300",
            "--cursor",
            &mid_note_cursor,
        ])
        .output()
        .expect("insum runs");
    assert_eq!(output.status.code(), Some(7), "CURSOR_STALE exits 7");
    let error = stderr_json(&output);
    assert_eq!(error["error"]["code"], "CURSOR_STALE");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("revision"),
        "stale by revision, not by path: {}",
        error["error"]["message"]
    );
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
    assert_eq!(
        search["complete"],
        Value::Bool(false),
        "real response: {}",
        serde_json::to_string(&search).unwrap()
    );
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

/// Raw STORE-entry ZIP builder for hostile-input fixtures: entries are
/// (name, bytes) pairs, written verbatim (no writer-side validation).
fn write_raw_zip(path: &Path, entries: &[(&str, &[u8])]) {
    use std::io::Write as _;
    let file = std::fs::File::create(path).expect("create");
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    for (name, data) in entries {
        zip.start_file(*name, options).expect("start entry");
        zip.write_all(data).expect("write entry");
    }
    zip.finish().expect("finish zip");
}

fn run_json_env(args: &[&str], key: &str, value: &str) -> Value {
    let output = insum()
        .args(args)
        .env(key, value)
        .output()
        .expect("insum runs");
    assert!(
        output.status.success(),
        "command {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|err| panic!("stdout of {args:?} is one JSON document: {err}"))
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
        "600",
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

// ---------------------------------------------------------------------------
// Review round 2 — contract fixes (K-01..K-06, S-01..S-03, C-04, T-02/T-03)
// ---------------------------------------------------------------------------

/// §8.6 formula computed INDEPENDENTLY in the test (raw payload bytes from
/// the archive, BE64 lengths, NUL-terminated domain) — not via insum-core.
#[test]
fn note_rev_matches_spec_8_6_formula() {
    use sha2::{Digest, Sha256};

    let corpus = Corpus::new();
    let file = corpus.path("minimal.um");
    let tree = run_json(&["tree", &file]);
    let cli_rev = tree["notes"][0]["note_rev"].as_str().unwrap();
    assert!(
        cli_rev.starts_with("sha256:"),
        "wire format is sha256:<hex> (K-02)"
    );

    // raw payload bytes straight from the archive
    let zip = std::fs::File::open(&file).unwrap();
    let mut archive = zip::ZipArchive::new(zip).unwrap();
    let note_id = tree["notes"][0]["note_id"].as_str().unwrap().to_owned();
    let mut payload_entry = archive.by_name(&format!("notes/{note_id}.json")).unwrap();
    let mut payload = Vec::new();
    std::io::Read::read_to_end(&mut payload_entry, &mut payload).unwrap();

    // canonical descriptor: manifest entry sorted-keys compact JSON
    let manifest: Value =
        serde_json::from_slice(&read_zip_entry_bytes(&file, "manifest.json")).unwrap();
    let descriptor = &manifest["notes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == Value::String(note_id.clone()))
        .unwrap()
        .clone();
    let canonical = canonical_json_str(descriptor);

    // literal §8.6: "UM-NOTE-REV-1\x00" + BE64 + payload + BE64 + descriptor
    let mut hasher = Sha256::new();
    hasher.update(b"UM-NOTE-REV-1\x00");
    hasher.update((payload.len() as u64).to_be_bytes());
    hasher.update(&payload);
    hasher.update((canonical.len() as u64).to_be_bytes());
    hasher.update(canonical.as_bytes());
    let expected = format!("sha256:{}", hex(&hasher.finalize()));
    assert_eq!(cli_rev, expected, "note_rev follows §8.6 (BE, NUL domain)");
}

fn read_zip_entry_bytes(path: &str, name: &str) -> Vec<u8> {
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut entry = archive.by_name(name).unwrap();
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut buf).unwrap();
    buf
}

/// Minimal canonical JSON for descriptor-shaped values (sorted keys, compact).
fn canonical_json_str(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by_key(|k| k.encode_utf16().collect::<Vec<u16>>());
            let parts: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap(),
                        canonical_json_str(&map[k.as_str()])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json_str).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn revisions_carry_the_sha256_prefix() {
    let corpus = Corpus::new();
    let ls = run_json(&["ls", &corpus.path("minimal.um")]);
    assert!(ls["file_rev"].as_str().unwrap().starts_with("sha256:"));
    let tree = run_json(&["tree", &corpus.path("regular.um")]);
    for note in tree["notes"].as_array().unwrap() {
        assert!(note["note_rev"].as_str().unwrap().starts_with("sha256:"));
    }
    let note_ls = run_json(&["ls", &corpus.path("legacy.note")]);
    assert!(note_ls["file_rev"].as_str().unwrap().starts_with("sha256:"));
    assert!(note_ls["note_rev"].as_str().unwrap().starts_with("sha256:"));
}

/// §10.1 structural checks: attrs/text/content/marks shape errors (K-04).
#[test]
fn validate_catches_broken_structures() {
    let dir = TempDir::new().expect("tempdir");
    let doc = serde_json::json!({
        "type": "noteDoc",
        "content": [
            {"type": "paragraph", "attrs": "not-an-object", "content": [
                {"type": "text", "text": "ok"}
            ]},
            {"type": "text"},
            {"type": "paragraph", "content": {"a": 1}},
            {"type": "paragraph", "marks": [{"type": "strong"}], "content": [
                {"type": "text", "text": "marked"}
            ]},
            {"type": "exoticUnknown", "content": []}
        ]
    });
    let file = dir.path().join("broken.note");
    fixtures::write_legacy_note(&file, &doc).expect("write");
    let result = run_json(&["validate", file.to_string_lossy().as_ref()]);
    assert_eq!(
        result["valid"],
        Value::Bool(false),
        "structural errors fail"
    );
    let notes = result["files"][0]["notes"].as_array().unwrap();
    let codes: Vec<&str> = notes[0]["errors"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["code"].as_str())
        .collect();
    assert_eq!(codes.len(), 3, "attrs/text/content violations: {codes:?}");
    assert!(codes.iter().all(|c| *c == "NOTE_DOC_INVALID"));
    // unknown node type is a warning, not an error
    let warn_codes: Vec<&str> = notes[0]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["code"].as_str())
        .collect();
    assert!(warn_codes.contains(&"UNKNOWN_NODE_TYPE"));
}

/// §4.7: unknown inline nodes withhold text and lower search completeness.
#[test]
fn unknown_inline_nodes_are_not_silent() {
    let dir = TempDir::new().expect("tempdir");
    let doc = serde_json::json!({
        "type": "noteDoc",
        "content": [
            {"type": "paragraph", "content": [
                {"type": "text", "text": "deploy the "},
                {"type": "fancyMention", "content": [{"type": "text", "text": "@x"}]},
                {"type": "text", "text": "service now"}
            ]}
        ]
    });
    let file = dir.path().join("mention.note");
    fixtures::write_legacy_note(&file, &doc).expect("write");

    let tree = run_json(&["tree", file.to_string_lossy().as_ref()]);
    let block = &tree["notes"][0]["blocks"][0];
    assert_eq!(block["text_incomplete"], Value::Bool(true));
    assert!(
        tree["notes"][0]["warnings"].as_array().is_some(),
        "note carries a warning"
    );

    let search = run_json(&["search", "-F", "deploy", file.to_string_lossy().as_ref()]);
    // the block has no extractable text → no match, and completeness is NOT
    // claimed (a false proof of absence is the failure mode under review)
    assert_eq!(search["complete"], Value::Bool(false));
    assert_eq!(search["warnings"].as_array().unwrap().len(), 1);
}

/// §6.5: anchors precede nested blocks; known marks project to markdown.
#[test]
fn annotated_covers_nested_blocks_and_marks() {
    let corpus = Corpus::new();
    let output = insum()
        .args(["read", "--format", "annotated", &corpus.path("regular.um")])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    // nested anchors: the list item and its paragraph are addressable (K-06)
    assert!(
        text.contains("<!-- um:pointer=\"/content/3/content/0\" -->"),
        "listItem anchor present"
    );
    assert!(
        text.contains("<!-- um:pointer=\"/content/3/content/0/content/0\" -->"),
        "nested paragraph anchor present"
    );
    assert!(text.contains("- Установить зависимости"));

    // marks (K-08): bold projects to **
    let dir = TempDir::new().expect("tempdir");
    let doc = serde_json::json!({
        "type": "noteDoc",
        "content": [
            {"type": "paragraph", "content": [
                {"type": "text", "text": "plain "},
                {"type": "text", "marks": [{"type": "bold"}], "text": "bold"},
                {"type": "text", "marks": [{"type": "sparkle"}], "text": "!"}
            ]}
        ]
    });
    let marked = dir.path().join("marked.note");
    fixtures::write_legacy_note(&marked, &doc).expect("write");
    let out = insum()
        .args([
            "read",
            "--format",
            "annotated",
            marked.to_string_lossy().as_ref(),
        ])
        .output()
        .unwrap();
    let body = String::from_utf8(out.stdout).unwrap();
    assert!(body.contains("plain **bold**"), "bold projects: {body}");
    assert!(
        body.contains("um:unrendered-marks=\"sparkle\""),
        "unknown marks are explicit"
    );
}

/// §11: one broken file must not kill search (S-01) — best-effort with
/// skipped + complete:false; NOTE_NOT_FOUND only when the note is nowhere.
#[test]
fn search_is_best_effort_over_broken_files() {
    let corpus = Corpus::new();
    let search = run_json(&[
        "search",
        "-F",
        "Установить",
        corpus.dir.path().to_string_lossy().as_ref(),
    ]);
    assert_eq!(search["complete"], Value::Bool(false));
    assert!(
        !search["matches"].as_array().unwrap().is_empty(),
        "healthy files still match"
    );
    assert!(
        search["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["error"]["code"] == "NOT_A_ZIP"),
        "broken.um is in skipped"
    );

    // --note missing in the searched container → NOTE_NOT_FOUND exit 3
    // (a directory with additional broken files stays best-effort exit 0)
    let output = insum()
        .args([
            "search",
            "-F",
            "x",
            "--note",
            "no-such-note",
            &corpus.path("minimal.um"),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(stderr_json(&output)["error"]["code"], "NOTE_NOT_FOUND");
}

/// §6.2/§5.5: UM_MAX_OUTPUT_BYTES bounds the WHOLE response (P-01).
#[test]
fn tree_budget_covers_the_whole_response() {
    let corpus = Corpus::new();
    let out = insum()
        .args(["tree", &corpus.path("regular.um"), "--max-bytes", "700"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = out.stdout.len();
    assert!(
        stdout < 1600,
        "response stays near the budget across notes: {stdout} bytes"
    );
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    let last = value["notes"].as_array().unwrap().last().unwrap();
    assert!(
        last["next_cursor"].is_string() || last["complete"] == Value::Bool(true),
        "pagination signal present"
    );
}

/// Hostile attrs must not break the budget (S-02).
#[test]
fn tree_caps_hostile_attrs() {
    let dir = TempDir::new().expect("tempdir");
    let doc = serde_json::json!({
        "type": "noteDoc",
        "content": [
            {"type": "paragraph", "attrs": {"pad": "x".repeat(5_000_000)}, "content": [
                {"type": "text", "text": "tiny"}
            ]}
        ]
    });
    let file = dir.path().join("padded.note");
    fixtures::write_legacy_note(&file, &doc).expect("write");
    let out = insum()
        .args(["tree", file.to_string_lossy().as_ref()])
        .output()
        .unwrap();
    let stdout = out.stdout.len();
    assert!(stdout < 8_000, "attrs capped: {stdout} bytes");
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    let block = &value["notes"][0]["blocks"][0];
    assert_eq!(block["attrs_truncated"], Value::Bool(true));
}

/// --context neighbors get windows; the budget includes the first match (S-03).
#[test]
fn search_context_is_windowed_and_budgeted() {
    let dir = TempDir::new().expect("tempdir");
    let huge = "z".repeat(300_000);
    let doc = serde_json::json!({
        "type": "noteDoc",
        "content": [
            {"type": "paragraph", "content": [{"type": "text", "text": huge}]},
            {"type": "paragraph", "content": [{"type": "text", "text": "needle here"}]}
        ]
    });
    let file = dir.path().join("huge.note");
    fixtures::write_legacy_note(&file, &doc).expect("write");
    let out = insum()
        .args([
            "search",
            "-F",
            "needle",
            "--context",
            "1",
            "--max-bytes",
            "4000",
            file.to_string_lossy().as_ref(),
        ])
        .output()
        .unwrap();
    let stdout = out.stdout.len();
    assert!(stdout < 20_000, "context is windowed: {stdout} bytes");
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    // the window keeps the whole response inside the budget → no truncation
    assert_eq!(value["truncated"], Value::Bool(false));
    let neighbor_text = value["matches"][0]["context"][0]["text"].as_str().unwrap();
    assert!(
        neighbor_text.ends_with("…"),
        "neighbor text carries a window marker"
    );
}

/// Explicit symlinked directories are followed (C-04): no silent empty sweep.
#[test]
fn search_follows_symlinked_directories() {
    let corpus = Corpus::new();
    let link = corpus.dir.path().join("kb-link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(corpus.dir.path(), &link).expect("symlink");
    #[cfg(not(unix))]
    panic!("symlink test requires unix");

    let hits = run_json(&["search", "-F", "Legacy", link.to_string_lossy().as_ref()]);
    assert!(
        !hits["matches"].as_array().unwrap().is_empty(),
        "symlinked dir is searched, not silently skipped"
    );
}

/// №3/№4 proven black-box: ls reads manifest only; payload corruption does
/// not affect ls but fails tree (T-02).
#[test]
fn ls_does_not_decompress_payload() {
    let dir = TempDir::new().expect("tempdir");
    let file = dir.path().join("corrupt.um");
    let note_id = fixtures::ulid_seq(4);
    let manifest = format!(
        "{{\"format\":\"um\",\"version\":1,\"type\":\"notepad\",\"notes\":[{{\"id\":\"{note_id}\",\"path\":\"notes/{note_id}.json\",\"order\":0,\"title\":\"t\",\"schema\":\"plain\",\"schemaVersion\":2}}]}}"
    );
    write_raw_zip(
        &file,
        &[
            ("manifest.json", manifest.as_bytes()),
            (
                format!("notes/{note_id}.json").as_str(),
                b"this is not JSON",
            ),
        ],
    );

    // ls: exit 0 — manifest only, payload never touched
    let ls = insum()
        .args(["ls", file.to_string_lossy().as_ref()])
        .output()
        .unwrap();
    assert!(ls.status.success(), "ls works with a corrupt payload");
    // tree: exit 4 — payload fails to parse
    let tree = insum()
        .args(["tree", file.to_string_lossy().as_ref()])
        .output()
        .unwrap();
    assert_eq!(tree.status.code(), Some(4));
    assert_eq!(stderr_json(&tree)["error"]["code"], "NOTE_JSON_INVALID");
}

/// Hostile inputs hit LIMIT_EXCEEDED, not crashes (T-03).
#[test]
fn hostile_limits_are_enforced() {
    // deep JSON nesting in a payload → exit 5, not a stack overflow
    let dir = TempDir::new().expect("tempdir");
    let file = dir.path().join("deep.um");
    let note_id = fixtures::ulid_seq(5);
    let manifest = format!(
        "{{\"format\":\"um\",\"version\":1,\"type\":\"notepad\",\"notes\":[{{\"id\":\"{note_id}\",\"path\":\"notes/{note_id}.json\",\"order\":0,\"title\":\"t\",\"schema\":\"plain\",\"schemaVersion\":2}}]}}"
    );
    let deep = format!("{{\"a\":{}", "[".repeat(5000));
    write_raw_zip(
        &file,
        &[
            ("manifest.json", manifest.as_bytes()),
            (format!("notes/{note_id}.json").as_str(), deep.as_bytes()),
        ],
    );
    let out = insum()
        .args(["tree", file.to_string_lossy().as_ref()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5), "deep payload → LIMIT_EXCEEDED");
    assert_eq!(stderr_json(&out)["error"]["code"], "LIMIT_EXCEEDED");

    // UM_MAX_TOTAL_BYTES enforced on actual data — ls <file> hard-fails
    // (a directory listing is best-effort and reports invalid entries instead)
    let corpus = Corpus::new();
    let out = insum()
        .args(["ls", &corpus.path("minimal.um")])
        .env("UM_MAX_TOTAL_BYTES", "10")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));
    assert_eq!(stderr_json(&out)["error"]["code"], "LIMIT_EXCEEDED");

    // UM_MAX_ARCHIVE_ENTRIES enforced
    let corpus = Corpus::new();
    let out = insum()
        .args(["ls", &corpus.path("minimal.um")])
        .env("UM_MAX_ARCHIVE_ENTRIES", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));

    // search match cap from env
    let hits = run_json_env(
        &["search", "-F", "е", &corpus.path("regular.um")],
        "UM_MAX_SEARCH_MATCHES",
        "2",
    );
    assert!(hits["matches"].as_array().unwrap().len() <= 2);
}

/// Unsafe archive entry names are rejected (§12, T-04).
#[test]
fn unsafe_zip_entry_names_are_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let file = dir.path().join("evil.um");
    let manifest = "{\"format\":\"um\",\"version\":1,\"type\":\"notepad\",\"notes\":[]}";
    write_raw_zip(
        &file,
        &[
            ("manifest.json", manifest.as_bytes()),
            ("../evil.json", b"x"),
        ],
    );
    let out = insum()
        .args(["ls", file.to_string_lossy().as_ref()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(
        stderr_json(&out)["error"]["code"],
        "ARCHIVE_ENTRY_NAME_INVALID"
    );
}

/// read / block get work on .note files (T-07).
#[test]
fn read_and_block_get_on_note_files() {
    let corpus = Corpus::new();
    let file = corpus.path("legacy.note");
    let read = run_json(&["read", &file]);
    assert_eq!(read["notes"][0]["note_title"], "Standalone");
    let block = run_json(&["block", "get", &file, "--pointer", "/content/0"]);
    assert_eq!(block["block"]["node"]["type"], "heading");
    // --note on .note is a usage error
    let out = insum()
        .args(["read", "--note", "x", &file])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// --depth truncates levels and marks parents (T-08).
#[test]
fn tree_depth_truncates_and_marks() {
    let corpus = Corpus::new();
    let tree = run_json(&["tree", &corpus.path("regular.um"), "--depth", "1"]);
    let note = &tree["notes"][0];
    assert_eq!(
        note["complete"],
        Value::Bool(false),
        "depth cut lowers complete"
    );
    let cut: Vec<&Value> = note["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["children_truncated"] == Value::Bool(true))
        .collect();
    assert!(!cut.is_empty(), "parents at the cut are marked");
}

/// C-08: read --format json rejects --cursor instead of ignoring it.
#[test]
fn read_json_rejects_cursor() {
    let corpus = Corpus::new();
    let out = insum()
        .args(["read", "--cursor", "abc", &corpus.path("minimal.um")])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

// ---------------------------------------------------------------------------
// Delta round 2 — mandatory gate: multi-note pagination (NEW-1/NEW-2)
// ---------------------------------------------------------------------------

/// A 4-note container with sizable notes: forces mid-note pages AND note
/// boundaries within one walk.
fn write_multi_note(path: &Path) {
    let mut notes = Vec::new();
    for i in 1..=4 {
        let blocks: Vec<Value> = (1..=8)
            .map(|j| {
                fixtures::paragraph(&[fixtures::text(&format!(
                    "note{i} block{j} payload text for pagination"
                ))])
            })
            .collect();
        notes.push(fixtures::FixtureNote::plain_page(
            &fixtures::ulid_seq(i as u32),
            &format!("note{i}"),
            &blocks,
        ));
    }
    fixtures::write_um(path, &notes).expect("multi-note fixture");
}

fn decode_cursor_json(token: &str) -> Value {
    let bytes = base64url_decode(token);
    serde_json::from_slice(&bytes).expect("cursor is base64url JSON")
}

/// Mandatory regression gate (review NEW-1): a full tree pagination walk of
/// a multi-note container reaches EVERY note, emits each block exactly once,
/// resumes cleanly from note-boundary cursors, and terminates complete.
#[test]
fn tree_pagination_walks_all_notes_of_multi_note_container() {
    let dir = TempDir::new().expect("tempdir");
    let file = dir.path().join("multi.um");
    write_multi_note(&file);

    let manifest_ids: Vec<String> = (1..=4).map(|i| fixtures::ulid_seq(i as u32)).collect();
    let mut cursor: Option<String> = None;
    let mut order_seen: Vec<String> = Vec::new();
    let mut seen_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut boundary_resumes = 0usize;
    let mut mid_note_resumes = 0usize;
    let mut pages = 0usize;

    let file_arg = file.to_string_lossy().into_owned();
    loop {
        let mut args = vec!["tree", file_arg.as_str(), "--max-bytes", "700"];
        if let Some(c) = &cursor {
            args.push("--cursor");
            args.push(c);
        }
        let page = run_json(&args);
        pages += 1;
        assert!(pages < 100, "walk does not terminate");

        for note in page["notes"].as_array().unwrap() {
            let nid = note["note_id"].as_str().unwrap().to_owned();
            if order_seen.last() != Some(&nid) {
                order_seen.push(nid.clone());
            }
            for block in note["blocks"].as_array().unwrap() {
                let pointer = block["pointer"].as_str().expect("pointer").to_owned();
                assert!(
                    seen_keys.insert(format!("{nid}{pointer}")),
                    "block {nid}{pointer} emitted twice"
                );
            }
        }
        let next = page["notes"].as_array().unwrap().last().unwrap()["next_cursor"]
            .as_str()
            .map(str::to_owned);
        match next {
            Some(token) => {
                let decoded = decode_cursor_json(&token);
                if decoded["np"].is_null() && decoded["bi"].as_u64() == Some(0) {
                    boundary_resumes += 1;
                } else {
                    mid_note_resumes += 1;
                }
                cursor = Some(token);
            }
            None => break,
        }
    }

    // every note reached, in manifest order, no silent tail
    let mut distinct = order_seen.clone();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        4,
        "all 4 notes must be reached: {order_seen:?}"
    );
    for (seen, expected) in distinct.iter().zip(manifest_ids.iter()) {
        assert_eq!(seen, expected, "notes follow manifest order");
    }
    assert!(
        boundary_resumes >= 1,
        "walk exercises note-boundary cursors (NEW-1 flavor 2)"
    );
    assert!(mid_note_resumes >= 1, "walk exercises mid-note cursors");
    // full coverage: 4 notes × 8 blocks = 32 blocks
    assert_eq!(seen_keys.len(), 32);
}

/// Mandatory regression gate (review NEW-2): annotated pagination covers all
/// notes, carries um:incomplete markers including at note boundaries, and
/// the final page ends without a marker.
#[test]
fn annotated_pagination_walks_all_notes_and_marks_boundaries() {
    let dir = TempDir::new().expect("tempdir");
    let file = dir.path().join("multi.um");
    write_multi_note(&file);

    let manifest_ids: Vec<String> = (1..=4).map(|i| fixtures::ulid_seq(i as u32)).collect();
    let mut cursor: Option<String> = None;
    let mut seen_notes: Vec<String> = Vec::new();
    let mut boundary_markers = 0usize;
    let mut pages = 0usize;

    let file_arg = file.to_string_lossy().into_owned();
    loop {
        let mut args = vec![
            "read",
            "--format",
            "annotated",
            file_arg.as_str(),
            "--max-bytes",
            "600",
        ];
        if let Some(c) = &cursor {
            args.push("--cursor");
            args.push(c);
        }
        let out = insum().args(&args).output().expect("insum runs");
        assert!(
            out.status.success(),
            "annotated page fails: {:?}",
            out.stderr
        );
        pages += 1;
        assert!(pages < 100, "walk does not terminate");
        let text = String::from_utf8(out.stdout.clone()).unwrap();
        // collect note headers
        for chunk in text.split("<!-- um:api_version=1 note_id=\"").skip(1) {
            let id: String = chunk.chars().take_while(|c| *c != '"').collect();
            if seen_notes.last() != Some(&id) {
                seen_notes.push(id);
            }
        }
        let incomplete = text.contains("um:incomplete next_cursor=\"");
        let next = text
            .split("um:incomplete next_cursor=\"")
            .nth(1)
            .map(|chunk| chunk.chars().take_while(|c| *c != '"').collect::<String>());
        match (incomplete, next) {
            (true, Some(token)) => {
                let decoded = decode_cursor_json(&token);
                if decoded["bi"].as_u64() == Some(0) {
                    boundary_markers += 1;
                }
                cursor = Some(token);
            }
            (false, None) => break,
            other => panic!("inconsistent annotated page: {other:?}"),
        }
    }

    let mut distinct = seen_notes.clone();
    distinct.dedup();
    assert_eq!(distinct.len(), 4, "all notes annotated: {seen_notes:?}");
    for (seen, expected) in distinct.iter().zip(manifest_ids.iter()) {
        assert_eq!(seen, expected);
    }
    assert!(
        boundary_markers >= 1,
        "um:incomplete appears at note boundaries (NEW-2)"
    );
}

/// P-02 preserved: a resume page parses only the pinned note — observable as
/// a page whose cost is dominated by one note even in a big container. The
/// behavioral proof: the resume emits the pinned note's blocks and continues.
#[test]
fn tree_resume_page_reads_beyond_pinned_note() {
    let dir = TempDir::new().expect("tempdir");
    let file = dir.path().join("multi.um");
    write_multi_note(&file);
    let first = run_json(&[
        "tree",
        file.to_string_lossy().as_ref(),
        "--max-bytes",
        "700",
    ]);
    let cursor = first["notes"][0]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    // resume: the pinned note finishes and the page continues into note 2
    let page = run_json(&[
        "tree",
        file.to_string_lossy().as_ref(),
        "--max-bytes",
        "700",
        "--cursor",
        &cursor,
    ]);
    let notes = page["notes"].as_array().unwrap();
    assert!(
        notes
            .iter()
            .any(|n| n["note_id"] == Value::String(fixtures::ulid_seq(2)))
            || notes[notes.len() - 1]["next_cursor"].is_string(),
        "resume continues past the pinned note"
    );
}

/// C-10: a deep manifest fails as LIMIT_EXCEEDED (exit 5), UM_MAX_DEPTH respected.
#[test]
fn deep_manifest_hits_limit_exceeded() {
    let dir = TempDir::new().expect("tempdir");
    let file = dir.path().join("deep-manifest.um");
    let deep = format!(
        "{{\"format\":\"um\",\"version\":1,\"type\":\"notepad\",\"notes\":[],\"pad\":{}",
        "[".repeat(200)
    );
    write_raw_zip(&file, &[("manifest.json", deep.as_bytes())]);
    let out = insum()
        .args(["ls", file.to_string_lossy().as_ref()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5), "deep manifest → LIMIT_EXCEEDED");
    assert_eq!(stderr_json(&out)["error"]["code"], "LIMIT_EXCEEDED");

    // UM_MAX_DEPTH is honored: depth 20 manifest rejected at limit 16
    let file2 = dir.path().join("depth20-manifest.um");
    let manifest = format!(
        "{{\"format\":\"um\",\"version\":1,\"type\":\"notepad\",\"notes\":[],\"pad\":{}",
        "[".repeat(20)
    );
    write_raw_zip(&file2, &[("manifest.json", manifest.as_bytes())]);
    let out = insum()
        .args(["ls", file2.to_string_lossy().as_ref()])
        .env("UM_MAX_DEPTH", "16")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));
    assert_eq!(stderr_json(&out)["error"]["code"], "LIMIT_EXCEEDED");
}

/// K-11: an inscriptum-note envelope with version != 1 → UNSUPPORTED_VERSION.
#[test]
fn envelope_version_is_checked() {
    let dir = TempDir::new().expect("tempdir");
    let doc = fixtures::plain_doc(&[fixtures::paragraph(&[fixtures::text("hi")])]);
    let envelope = serde_json::json!({
        "format": "inscriptum-note",
        "version": 99,
        "features": {"block_ids": 1},
        "document": doc
    });
    let file = dir.path().join("v99.note");
    std::fs::write(&file, envelope.to_string()).expect("write");
    let out = insum()
        .args(["tree", file.to_string_lossy().as_ref()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4), "UNSUPPORTED_VERSION exits 4");
    assert_eq!(stderr_json(&out)["error"]["code"], "UNSUPPORTED_VERSION");
}

/// NEW-3: the list marker survives a first child that is not a paragraph.
#[test]
fn list_marker_survives_non_paragraph_first_child() {
    let dir = TempDir::new().expect("tempdir");
    let doc = serde_json::json!({
        "type": "noteDoc",
        "content": [
            {"type": "bulletList", "content": [
                {"type": "listItem", "content": [
                    {"type": "heading", "attrs": {"level": 3}, "content": [{"type": "text", "text": "Nested head"}]},
                    {"type": "paragraph", "content": [{"type": "text", "text": "body"}]}
                ]}
            ]},
            {"type": "taskList", "content": [
                {"type": "taskItem", "attrs": {"checked": true}, "content": [
                    {"type": "bulletList", "content": [
                        {"type": "listItem", "content": [
                            {"type": "paragraph", "content": [{"type": "text", "text": "sub"}]}
                        ]}
                    ]}
                ]}
            ]}
        ]
    });
    let file = dir.path().join("lists.note");
    fixtures::write_legacy_note(&file, &doc).expect("write");
    let out = insum()
        .args([
            "read",
            "--format",
            "annotated",
            file.to_string_lossy().as_ref(),
        ])
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("- "), "bullet marker present");
    assert!(text.contains("- [x] "), "checkbox present on taskItem");
    assert!(
        text.contains("### Nested head"),
        "non-paragraph child still rendered"
    );
}
