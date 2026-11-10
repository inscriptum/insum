//! `insum migrate-ids` — stage C contract tests (§7.2, §4.4–4.6, criteria
//! 22–27 of §15.3). The artifact fixture is self-hashed via
//! `insum_core::schema::compute_hash`; write effects are verified by
//! unpacking the result.

use std::fs;
use std::path::Path;
use std::process::Output as ProcessOutput;

use assert_cmd::Command;
use serde_json::{Value, json};

use insum_core::schema::compute_hash;
use insum_test_utils::{FixtureNote, write_um};

fn insum() -> Command {
    Command::cargo_bin("insum").expect("insum binary builds")
}

fn artifact_with_id_rules() -> Value {
    let mut raw = json!({
        "schema_id": "inscriptum",
        "schema_version": 1,
        "schema_hash": "",
        "top_node": "noteDoc",
        "validation_capability": "partial",
        "id_rules": {
            "attrs_key": "id",
            "dom_attribute": "data-id",
            "pattern": "^[a-z0-9]{8,12}$",
            "recommended_length": 10,
            "uniqueness": "note",
            "encoding": "opaque"
        },
        "nodes": {
            "noteDoc": {"content": "noteTitle block*", "groups": [], "attrs": {},
                        "marks": [], "addressable": false, "inline_content": false,
                        "searchable": false},
            "noteTitle": {"content": "inline*", "groups": ["block"], "attrs": {"id": {"default": null}},
                          "marks": [], "addressable": true, "inline_content": true,
                          "searchable": true},
            "paragraph": {"content": "inline*", "groups": ["block"],
                          "attrs": {"id": {"default": null}}, "marks": [],
                          "addressable": true, "inline_content": true, "searchable": true},
            "text": {"content": null, "groups": ["inline"], "attrs": {},
                     "marks": [], "addressable": false, "inline_content": true,
                     "searchable": false}
        },
        "marks": {}
    });
    raw["schema_hash"] = json!(compute_hash(&raw));
    raw
}

fn write_artifact(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("schema.json");
    fs::write(
        &path,
        serde_json::to_vec(&artifact_with_id_rules()).unwrap(),
    )
    .unwrap();
    path
}

/// Unpack one entry of a `.um` container (zip crate — no external tools).
fn read_entry(file: &str, name: &str) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(fs::File::open(file).unwrap()).unwrap();
    let mut entry = archive.by_name(name).unwrap();
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut out).unwrap();
    out
}

/// Read one entry from in-memory container bytes.
fn read_entry_bytes(bytes: &[u8], name: &str) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
    let mut entry = archive.by_name(name).unwrap();
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut out).unwrap();
    out
}

/// List entry names of a `.um` container.
#[allow(dead_code)]
fn entry_names(file: &str) -> Vec<String> {
    let mut archive = zip::ZipArchive::new(fs::File::open(file).unwrap()).unwrap();
    (0..archive.len())
        .map(|i| {
            archive
                .by_index_raw(i)
                .unwrap()
                .name()
                .unwrap()
                .into_owned()
        })
        .collect()
}

/// Rewrite one entry of a `.um` container in place (zip crate).
fn patch_entry(file: &str, name: &str, new_bytes: &[u8]) {
    let mut archive = zip::ZipArchive::new(fs::File::open(file).unwrap()).unwrap();
    let buffer = std::io::Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(buffer);
    for index in 0..archive.len() {
        let raw = archive.by_index_raw(index).unwrap();
        let existing = raw.name().unwrap().into_owned();
        if existing == name {
            let last_modified = raw.last_modified().unwrap_or_default();
            drop(raw);
            writer
                .start_file(
                    existing.as_str(),
                    zip::write::SimpleFileOptions::default().last_modified_time(last_modified),
                )
                .unwrap();
            std::io::Write::write_all(&mut writer, new_bytes).unwrap();
        } else {
            writer.raw_copy_file(raw).unwrap();
        }
    }
    let buffer = writer.finish().unwrap();
    fs::write(file, buffer.into_inner()).unwrap();
}

fn stderr_json(output: &ProcessOutput) -> Value {
    assert!(output.stdout.is_empty(), "stdout stays empty on error");
    serde_json::from_slice(&output.stderr).expect("stderr is one JSON error object")
}

fn collect_ids(value: &Value) -> Vec<String> {
    fn walk(node: &Value, ids: &mut Vec<String>) {
        if let Some(id) = node
            .get("attrs")
            .and_then(|attrs| attrs.get("id"))
            .and_then(Value::as_str)
        {
            ids.push(id.to_owned());
        }
        if let Some(children) = node.get("content").and_then(Value::as_array) {
            for child in children {
                walk(child, ids);
            }
        }
    }
    let mut ids = Vec::new();
    walk(value, &mut ids);
    ids
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::TempDir::new().unwrap(),
        }
    }

    fn artifact(&self) -> String {
        write_artifact(self.dir.path())
            .to_string_lossy()
            .into_owned()
    }

    /// Two notes: one with missing + duplicate + invalid ids, one clean
    /// legacy (no ids at all); an asset and an unknown manifest field ride
    /// along to prove preservation.
    fn container(&self) -> String {
        let path = self.dir.path().join("migrate.um");
        let doc1 = json!({
            "type": "noteDoc",
            "content": [
                {"type": "noteTitle", "content": [{"type": "text", "text": "Alpha"}]},
                {"type": "paragraph", "attrs": {"id": "keepvalid0"}, "content": [{"type": "text", "text": "kept"}]},
                {"type": "paragraph", "attrs": {"id": "keepvalid0"}, "content": [{"type": "text", "text": "dup"}]},
                {"type": "paragraph", "attrs": {"id": "BAD_ID!!"}, "content": [{"type": "text", "text": "invalid"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "missing"}]}
            ]
        });
        let doc2 = json!({
            "type": "noteDoc",
            "content": [
                {"type": "noteTitle", "attrs": {"id": "secondone0"}, "content": [{"type": "text", "text": "Beta"}]}
            ]
        });
        let notes = [
            FixtureNote {
                id: insum_test_utils::ulid_seq(1),
                title: "Alpha".to_owned(),
                schema: "title",
                doc: doc1,
            },
            FixtureNote {
                id: insum_test_utils::ulid_seq(2),
                title: "Beta".to_owned(),
                schema: "plain",
                doc: doc2,
            },
        ];
        write_um(&path, &notes).unwrap();
        // append an asset and an unknown manifest field (raw zip surgery)
        let file = fs::OpenOptions::new().read(true).open(&path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let buffer = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(buffer);
        for index in 0..archive.len() {
            let raw = archive.by_index_raw(index).unwrap();
            writer.raw_copy_file(raw).unwrap();
        }
        {
            use std::io::Write as _;
            writer
                .start_file("assets/pic.png", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"\x89PNG-fake-bytes").unwrap();
        }
        let buffer = writer.finish().unwrap();
        fs::write(&path, buffer.into_inner()).unwrap();

        let manifest_path = self.dir.path().join("manifest-patch.json");
        fs::write(&manifest_path, b"").unwrap();
        path.to_string_lossy().into_owned()
    }

    fn patch_manifest_unknown_field(&self, file: &str) {
        let manifest = read_entry(file, "manifest.json");
        let mut manifest: Value = serde_json::from_slice(&manifest).unwrap();
        manifest["owner_unknown"] = json!({"keep": "me"});
        patch_entry(
            file,
            "manifest.json",
            &serde_json::to_vec_pretty(&manifest).unwrap(),
        );
    }
}

#[test]
fn dry_run_does_not_touch_bytes_or_mtime_and_previews_changes() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let before = fs::read(&file).unwrap();
    let mtime_before = fs::metadata(&file).unwrap().modified().unwrap();

    let output = insum()
        .args(["migrate-ids", &file, "--dry-run", "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["dry_run"], true);
    assert_eq!(value["would_write"], true);
    let note = &value["notes"][0];
    assert!(note["missing_ids"].as_u64().unwrap() > 0);
    assert_eq!(note["duplicate_ids"].as_u64().unwrap(), 1);
    assert_eq!(note["invalid_ids"].as_u64().unwrap(), 1);
    let changes = note["changes"].as_array().unwrap();
    assert!(!changes.is_empty());
    for change in changes {
        assert!(change["pointer"].is_string());
        assert!(change["new_id"].is_string());
        assert!(change["type"].is_string());
    }

    let after = fs::read(&file).unwrap();
    assert_eq!(before, after, "dry-run must not change file bytes");
    assert_eq!(
        mtime_before,
        fs::metadata(&file).unwrap().modified().unwrap(),
        "dry-run must not change mtime"
    );
}

#[test]
fn commit_fixes_ids_per_document_order_and_keeps_first_valid_duplicate() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    fixture.patch_manifest_unknown_field(&file);
    let fixture_bytes_before = fs::read(&file).unwrap();
    let output = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["dry_run"], false);

    // unpack and verify
    let manifest: Value = serde_json::from_slice(&read_entry(&file, "manifest.json")).unwrap();
    // criterion 26: marker + schema metadata per note
    for note in manifest["notes"].as_array().unwrap() {
        assert_eq!(note["features"]["block_ids"], 1);
        assert_eq!(note["features"]["schema"]["id"], "inscriptum");
        assert!(
            note["features"]["schema"]["schema_hash"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        // the editor profile field is untouched
        assert!(matches!(
            note["schema"].as_str(),
            Some("title") | Some("plain")
        ));
    }
    // unknown manifest fields survive
    assert_eq!(manifest["owner_unknown"]["keep"], "me");

    let note1: Value = serde_json::from_slice(&read_entry(
        &file,
        &format!("notes/{}.json", insum_test_utils::ulid_seq(1)),
    ))
    .unwrap();
    let ids = collect_ids(&note1);
    // criterion 25: unique within the note
    let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
    assert_eq!(ids.len(), unique.len(), "ids must be unique: {ids:?}");
    // criterion 23: the kept valid id survived (first of the duplicate pair)
    assert!(
        ids.contains(&"keepvalid0".to_owned()),
        "first valid duplicate kept"
    );
    // criterion 24: grammar for every id
    for id in &ids {
        assert!(
            id.len() >= 8
                && id.len() <= 12
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "id {id} violates the grammar"
        );
    }
    // the invalid id is gone
    assert!(!ids.contains(&"BAD_ID!!".to_owned()));

    // mapping: the kept id is not in changes; the second duplicate is
    let changes = value["notes"][0]["changes"].as_array().unwrap();
    let kept_in_changes = changes
        .iter()
        .any(|c| c["pointer"] == json!("/content/1") && c["old_id"] == json!("keepvalid0"));
    assert!(!kept_in_changes, "the kept id is not remapped");
    let duplicate_fixed = changes.iter().find(|c| c["old_id"] == json!("keepvalid0"));
    assert!(
        duplicate_fixed.is_some(),
        "the second duplicate is remapped"
    );

    // the untouched note payload is byte-identical (raw copy); the marker
    // rides in the manifest; assets survive byte-identically
    let note2_new = read_entry(
        &file,
        &format!("notes/{}.json", insum_test_utils::ulid_seq(2)),
    );
    let note2_doc: Value = serde_json::from_slice(&note2_new).unwrap();
    assert_eq!(collect_ids(&note2_doc), vec!["secondone0"]);
    assert_eq!(
        read_entry(&file, "assets/pic.png"),
        read_entry_bytes(&fixture_bytes_before, "assets/pic.png"),
        "asset entry is raw-copied byte-identically"
    );
    assert_eq!(
        note2_new,
        read_entry_bytes(
            &fixture_bytes_before,
            &format!("notes/{}.json", insum_test_utils::ulid_seq(2)),
        ),
        "untouched payload entry is raw-copied byte-identically"
    );
}

#[test]
fn rerun_on_migrated_note_is_a_no_op() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let first = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(first.status.success());
    let bytes_after_first = fs::read(&file).unwrap();

    let second = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(second.status.success());
    let value: Value = serde_json::from_slice(&second.stdout).unwrap();
    let total_changes: u64 = value["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["changes"].as_array().unwrap().len() as u64)
        .sum();
    assert_eq!(total_changes, 0, "migrated notes are not selected again");
    let bytes_after_second = fs::read(&file).unwrap();
    assert_eq!(
        bytes_after_first, bytes_after_second,
        "a full no-op writes nothing (the marker is already there)"
    );
    let third = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(third.status.success());
    assert_eq!(
        bytes_after_second,
        fs::read(&file).unwrap(),
        "the second no-op run writes nothing new"
    );
}

#[test]
fn selection_by_note_id_migrates_only_that_note() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    fixture.patch_manifest_unknown_field(&file);
    let before = fs::read(&file).unwrap();

    let output = insum()
        .args([
            "migrate-ids",
            &file,
            "--note",
            &insum_test_utils::ulid_seq(1),
            "--schema",
            &artifact,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["notes"].as_array().unwrap().len(), 1);

    let manifest: Value = serde_json::from_slice(&read_entry(&file, "manifest.json")).unwrap();
    let markers: Vec<bool> = manifest["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["features"]["block_ids"] == 1)
        .collect();
    assert_eq!(
        markers,
        vec![true, false],
        "only the selected note is marked"
    );
    assert_eq!(
        manifest["owner_unknown"]["keep"], "me",
        "unknown fields survive"
    );
    let _ = before;
}

#[test]
fn unmarked_note_with_complete_ids_still_gets_the_marker() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    // a container whose only note already carries complete unique ids but
    // no marker: migration verifies and stamps, no payload rewrite
    let path = fixture.dir.path().join("complete.um");
    let doc = json!({
        "type": "noteDoc",
        "content": [
            {"type": "noteTitle", "attrs": {"id": "alreadyok0"}, "content": [{"type": "text", "text": "T"}]},
            {"type": "paragraph", "attrs": {"id": "anotherok1"}, "content": [{"type": "text", "text": "P"}]}
        ]
    });
    let notes = [FixtureNote {
        id: insum_test_utils::ulid_seq(1),
        title: "Complete".to_owned(),
        schema: "title",
        doc,
    }];
    write_um(&path, &notes).unwrap();
    let file = path.to_string_lossy().into_owned();

    let output = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["notes"][0]["changes"].as_array().unwrap().len(), 0);
    assert!(
        value["file_rev"].as_str().is_some(),
        "the manifest was rewritten"
    );

    let manifest: Value = serde_json::from_slice(&read_entry(&file, "manifest.json")).unwrap();
    assert_eq!(manifest["notes"][0]["features"]["block_ids"], 1);
    // payloads unchanged: ids are the same as before
    let payload: Value = serde_json::from_slice(&read_entry(
        &file,
        &format!("notes/{}.json", insum_test_utils::ulid_seq(1)),
    ))
    .unwrap();
    assert_eq!(collect_ids(&payload), vec!["alreadyok0", "anotherok1"]);
}

#[test]
fn unknown_note_id_is_note_not_found() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let output = insum()
        .args([
            "migrate-ids",
            &file,
            "--note",
            "01NOPE00000000000000000000",
            "--schema",
            &artifact,
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(stderr_json(&output)["error"]["code"], "NOTE_NOT_FOUND");
}

#[test]
fn bundled_fallback_refuses_to_write() {
    let fixture = Fixture::new();
    let file = fixture.container();
    // cwd outside any vault: the bundled fallback is the only artifact
    let work = tempfile::TempDir::new().unwrap();
    let output = insum()
        .args(["migrate-ids", &file])
        .current_dir(work.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(6), "SCHEMA_UNAVAILABLE exits 6");
    assert_eq!(stderr_json(&output)["error"]["code"], "SCHEMA_UNAVAILABLE");
}

#[test]
fn tampered_artifact_refuses_to_write() {
    let fixture = Fixture::new();
    let file = fixture.container();
    let mut raw = artifact_with_id_rules();
    raw["validation_capability"] = json!("full"); // shift the fingerprint
    let path = fixture.dir.path().join("tampered.json");
    fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
    let output = insum()
        .args(["migrate-ids", &file, "--schema", &path.to_string_lossy()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(6), "SCHEMA_MISMATCH exits 6");
    assert_eq!(stderr_json(&output)["error"]["code"], "SCHEMA_MISMATCH");
}

#[test]
fn live_editor_lease_blocks_the_write() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();

    // a fresh dirty lease with a live holder pid — the write must refuse
    // immediately (WRITE_LOCKED, exit 7)
    let lease = fixture.dir.path().join(".migrate.um.insum-lease");
    let holder = json!({
        "format": 1,
        "kind": "lease",
        "holder": {"kind": "cli", "pid": std::process::id(), "hostname": "test-host"},
        "op_id": "leasetest01",
        "created_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
        "purpose": "editor"
    });
    fs::write(&lease, serde_json::to_vec(&holder).unwrap()).unwrap();

    let output = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .env("INSUM_LOCK_STALE_AFTER_MS", "60000")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "WRITE_LOCKED exits 7");
    assert_eq!(stderr_json(&output)["error"]["code"], "WRITE_LOCKED");
}

#[test]
fn legacy_note_converts_to_envelope_and_stays_readable() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let path = fixture.dir.path().join("legacy.note");
    let doc = json!({
        "type": "noteDoc",
        "content": [
            {"type": "noteTitle", "content": [{"type": "text", "text": "Legacy"}]},
            {"type": "paragraph", "content": [{"type": "text", "text": "body"}]}
        ]
    });
    fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();

    // before: readable as legacy (ls carries the legacy flag)
    let output = insum().arg("ls").arg(&path).output().unwrap();
    assert!(output.status.success());
    let before: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(before["legacy"], true);

    let output = insum()
        .args([
            "migrate-ids",
            &path.to_string_lossy(),
            "--schema",
            &artifact,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();

    let envelope: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(envelope["format"], "inscriptum-note");
    assert_eq!(envelope["version"], 1);
    assert_eq!(envelope["features"]["block_ids"], 1);
    assert_eq!(envelope["schema"]["id"], "inscriptum");
    assert!(envelope["document"].is_object());
    let ids = collect_ids(&envelope["document"]);
    assert_eq!(ids.len(), 2, "noteTitle + paragraph get ids");
    assert_eq!(value["dry_run"], false);

    // after: the current read path opens the envelope (criterion 27)
    let output = insum().arg("ls").arg(&path).output().unwrap();
    assert!(
        output.status.success(),
        "envelope is readable: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let after: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(after["legacy"], false);
    let output = insum().arg("read").arg(&path).output().unwrap();
    assert!(
        output.status.success(),
        "envelope reads: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // rerun: no-op
    let output = insum()
        .args([
            "migrate-ids",
            &path.to_string_lossy(),
            "--schema",
            &artifact,
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["notes"][0]["changes"].as_array().unwrap().len(), 0);
}

#[test]
fn block_get_by_id_is_gated_on_the_marker() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();

    // before migration: --id refuses (ids not guaranteed)
    let output = insum()
        .args([
            "block",
            "get",
            &file,
            "--note",
            &insum_test_utils::ulid_seq(2),
            "--id",
            "secondone0",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "pre-migration --id is USAGE");

    // after migration: the same --id resolves
    let migrate = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(
        migrate.status.success(),
        "{}",
        String::from_utf8_lossy(&migrate.stderr)
    );
    let output = insum()
        .args([
            "block",
            "get",
            &file,
            "--note",
            &insum_test_utils::ulid_seq(2),
            "--id",
            "secondone0",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["block"]["id"], "secondone0");
    assert_eq!(value["block"]["node"]["type"], "noteTitle");
    // unknown id: BLOCK_NOT_FOUND (exit 3)
    let output = insum()
        .args([
            "block",
            "get",
            &file,
            "--note",
            &insum_test_utils::ulid_seq(2),
            "--id",
            "no-such-id0",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    // --pointer and --id together: USAGE
    let output = insum()
        .args([
            "block",
            "get",
            &file,
            "--note",
            &insum_test_utils::ulid_seq(2),
            "--id",
            "secondone0",
            "--pointer",
            "/content/0",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn note_filter_is_rejected_for_note_files() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join("single.note");
    let doc = json!({"type": "noteDoc", "content": []});
    fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let output = insum()
        .args(["migrate-ids", &path.to_string_lossy(), "--note", "x"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stderr_json(&output)["error"]["code"], "USAGE");
}
