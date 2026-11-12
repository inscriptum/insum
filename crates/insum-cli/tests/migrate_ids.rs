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
            "bulletList": {"content": "listItem+", "groups": ["block"],
                           "attrs": {"id": {"default": null}}, "marks": [],
                           "addressable": true, "inline_content": false, "searchable": false},
            "listItem": {"content": "paragraph block*", "groups": [],
                         "attrs": {"id": {"default": null}}, "marks": [],
                         "addressable": true, "inline_content": false, "searchable": false},
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

/// M-6: the artifact is resolved for the vault that CONTAINS the target file
/// (§4.7) — a vault artifact is found with the process cwd outside the vault,
/// and an artifact sitting in the cwd is never stamped into a vault that has
/// none. Dry-run and commit resolve the same way (plan == commit).
#[test]
fn artifact_is_resolved_from_the_target_vault_not_the_cwd() {
    let fixture = Fixture::new();

    // a vault carrying the plugin runtime artifact, target file inside it
    let vault = fixture.dir.path().join("vault");
    fs::create_dir_all(vault.join(".obsidian/plugins/inscriptum")).unwrap();
    fs::write(
        vault.join(".obsidian/plugins/inscriptum/schema.json"),
        serde_json::to_vec(&artifact_with_id_rules()).unwrap(),
    )
    .unwrap();
    let notes = [FixtureNote::title_page(
        &insum_test_utils::ulid_seq(1),
        "Alpha",
        "first",
        &[insum_test_utils::paragraph(&[insum_test_utils::text(
            "body",
        )])],
    )];
    let container = vault.join("target.um");
    write_um(&container, &notes).unwrap();
    let file = container.to_string_lossy().into_owned();

    // cwd outside the vault, no --schema: dry-run plans and the commit
    // writes, both through the vault artifact of the target file
    let outside = tempfile::TempDir::new().unwrap();
    for extra in [vec!["--dry-run"], vec![]] {
        let mut args = vec!["migrate-ids", &file];
        args.extend(extra);
        let output = insum()
            .args(&args)
            .current_dir(outside.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "vault artifact of the target file is used without --schema ({args:?}): {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // the marker was stamped by that commit — the vault artifact was not
    // merely readable, it authorized the write
    let manifest: Value = serde_json::from_slice(&read_entry(&file, "manifest.json")).unwrap();
    assert_eq!(manifest["notes"][0]["features"]["block_ids"], 1);

    // the cwd artifact must not leak into a vault that has none: the sibling
    // vault without .obsidian refuses (bundled fallback refuses to write) in
    // dry-run and commit alike, with the file untouched
    let foreign = fixture.dir.path().join("foreign-vault");
    fs::create_dir_all(&foreign).unwrap();
    let foreign_container = foreign.join("foreign.um");
    write_um(&foreign_container, &notes).unwrap();
    let foreign_file = foreign_container.to_string_lossy().into_owned();
    let before = fs::read(&foreign_file).unwrap();

    let work_vault = tempfile::TempDir::new().unwrap();
    fs::create_dir_all(work_vault.path().join(".obsidian/plugins/inscriptum")).unwrap();
    fs::write(
        work_vault
            .path()
            .join(".obsidian/plugins/inscriptum/schema.json"),
        serde_json::to_vec(&artifact_with_id_rules()).unwrap(),
    )
    .unwrap();
    for extra in [vec!["--dry-run"], vec![]] {
        let mut args = vec!["migrate-ids", &foreign_file];
        args.extend(extra);
        let output = insum()
            .args(&args)
            .current_dir(work_vault.path())
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(6),
            "a cwd artifact is not applied to a vault that has none ({args:?})"
        );
        assert_eq!(stderr_json(&output)["error"]["code"], "SCHEMA_UNAVAILABLE");
    }
    assert_eq!(
        before,
        fs::read(&foreign_file).unwrap(),
        "the refused migration leaves the foreign vault untouched"
    );
}

/// M-9: the change report respects UM_MAX_OUTPUT_BYTES — an oversized
/// report is explicitly incomplete (`complete: false`, `truncated: true`,
/// counts and an OUTPUT_BUDGET warning; §6.5 forbids silent truncation),
/// while the operation itself still covers every selected note. The
/// remainder is reachable with `--note` filters (a cursor cannot work:
/// the commit is one transaction).
#[test]
fn report_respects_the_output_budget_and_marks_incompleteness() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let before = fs::read(&file).unwrap();

    // a budget below the first note plan: the first note is emitted whole
    // (documented oversized-first-note exception), the rest is truncated
    // with the counts and the warning
    let output = insum()
        .args(["migrate-ids", &file, "--dry-run", "--schema", &artifact])
        .env("UM_MAX_OUTPUT_BYTES", "100")
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["complete"], false);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["notes_total"], 2);
    assert_eq!(value["notes_returned"], 1);
    assert_eq!(value["notes"].as_array().unwrap().len(), 1);
    let warnings = value["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w["code"] == "OUTPUT_BUDGET"),
        "the truncation is explained: {warnings:?}"
    );
    assert_eq!(
        before,
        fs::read(&file).unwrap(),
        "the truncated dry-run still does not touch the file"
    );

    // the remainder is reachable with --note filters
    let output = insum()
        .args([
            "migrate-ids",
            &file,
            "--dry-run",
            "--note",
            &insum_test_utils::ulid_seq(2),
            "--schema",
            &artifact,
        ])
        .env("UM_MAX_OUTPUT_BYTES", "100")
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["complete"], true);
    assert_eq!(value["notes"].as_array().unwrap().len(), 1);

    // the commit under the same tiny budget still migrates EVERY selected
    // note — only the report is partial
    let output = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .env("UM_MAX_OUTPUT_BYTES", "100")
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["complete"], false);
    assert_eq!(value["truncated"], true);
    let manifest: Value = serde_json::from_slice(&read_entry(&file, "manifest.json")).unwrap();
    let marked: Vec<bool> = manifest["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["features"]["block_ids"] == 1)
        .collect();
    assert_eq!(
        marked,
        vec![true, true],
        "the write covers every selected note despite the partial report"
    );

    // default budget: the full report is complete and carries no truncation
    // markers
    let fixture2 = Fixture::new();
    let file2 = fixture2.container();
    let output = insum()
        .args(["migrate-ids", &file2, "--dry-run", "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["complete"], true);
    assert!(value.get("truncated").is_none());
    assert!(value.get("warnings").is_none());
    assert_eq!(value["notes"].as_array().unwrap().len(), 2);
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
fn block_get_by_id_resolves_on_presence_and_warns_without_the_marker() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();

    // before migration: a physically present id resolves (§4.6 п.4 — read
    // by ID works on presence), but the answer warns the ids are not
    // guaranteed without the marker
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
        "pre-migration --id resolves on presence: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["block"]["id"], "secondone0");
    assert_eq!(value["block"]["node"]["type"], "noteTitle");
    let warnings = value["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w["code"] == "BLOCK_IDS_NOT_GUARANTEED"),
        "unmarked note warns the ids are not guaranteed: {warnings:?}"
    );

    // after migration: the same --id resolves without that warning
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
    let warnings = value["warnings"].as_array().unwrap();
    assert!(
        !warnings
            .iter()
            .any(|w| w["code"] == "BLOCK_IDS_NOT_GUARANTEED"),
        "a marked note does not warn: {warnings:?}"
    );

    // unknown id: BLOCK_NOT_FOUND (exit 3), not just a bare exit code —
    // marker or not, absence stays absence
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
    assert_eq!(stderr_json(&output)["error"]["code"], "BLOCK_NOT_FOUND");
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
fn regression_attrs_null_refuses_the_migration() {
    // M-02: a non-object attrs must refuse (the marker would lie), in
    // dry-run and commit alike
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let path = fixture.dir.path().join("badattrs.um");
    let doc = json!({
        "type": "noteDoc",
        "content": [
            {"type": "noteTitle", "attrs": null, "content": [{"type": "text", "text": "X"}]}
        ]
    });
    let notes = [FixtureNote {
        id: insum_test_utils::ulid_seq(1),
        title: "X".to_owned(),
        schema: "title",
        doc,
    }];
    write_um(&path, &notes).unwrap();
    let file = path.to_string_lossy().into_owned();
    let before = fs::read(&file).unwrap();

    for extra in [vec!["--dry-run"], vec![]] {
        let mut args = vec!["migrate-ids", &file, "--schema", &artifact];
        args.extend(extra.iter());
        let output = insum().args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(4), "NOTE_DOC_INVALID exits 4");
        assert_eq!(stderr_json(&output)["error"]["code"], "NOTE_DOC_INVALID");
    }
    assert_eq!(
        before,
        fs::read(&file).unwrap(),
        "refused writes leave the file alone"
    );
}

#[test]
fn regression_envelope_unknown_fields_survive_and_unmarked_envelope_is_selected() {
    // M-04/M-06: an envelope keeps its unknown fields and is selected by
    // marker state, not envelope presence (both scenarios are positive —
    // refusal cases live in regression_attrs_null_refuses_the_migration
    // and regression_broken_features_descriptor_refuses_the_marker)
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let path = fixture.dir.path().join("envelope.note");
    let envelope = json!({
        "format": "inscriptum-note",
        "version": 1,
        "features": {"block_ids": 1, "custom": "kept"},
        "schema": {"id": "inscriptum", "schema_version": 1, "schema_hash": "sha256:abc"},
        "document": {"type": "noteDoc", "content": [
            {"type": "noteTitle", "attrs": {"id": "BAD_ID_00"}, "content": [{"type": "text", "text": "T"}]}
        ]},
        "x_owner_field": {"keep": "me"}
    });
    fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    let file = path.to_string_lossy().into_owned();

    // marker set + an invalid id: fixes ids, keeps the unknown field and
    // the envelope's own schema metadata
    let output = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let after: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(
        after["x_owner_field"]["keep"], "me",
        "unknown envelope fields survive"
    );
    assert_eq!(
        after["schema"]["schema_hash"], "sha256:abc",
        "envelope schema metadata kept"
    );
    assert_eq!(after["features"]["block_ids"], 1);
    let ids = collect_ids(&after["document"]);
    assert!(
        !ids.contains(&"BAD_ID_00".to_owned()),
        "invalid id replaced"
    );

    // envelope without the marker + complete ids → still selected (M-06)
    let path2 = fixture.dir.path().join("envelope-nomarker.note");
    let envelope2 = json!({
        "format": "inscriptum-note",
        "version": 1,
        "features": {"custom": "x"},
        "schema": {"id": "inscriptum", "schema_version": 1, "schema_hash": "sha256:abc"},
        "document": {"type": "noteDoc", "content": [
            {"type": "noteTitle", "attrs": {"id": "completeid1"}, "content": [{"type": "text", "text": "T"}]}
        ]}
    });
    fs::write(&path2, serde_json::to_vec(&envelope2).unwrap()).unwrap();
    let file2 = path2.to_string_lossy().into_owned();
    let output = insum()
        .args(["migrate-ids", &file2, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let after: Value = serde_json::from_slice(&fs::read(&file2).unwrap()).unwrap();
    assert_eq!(
        after["features"]["block_ids"], 1,
        "the marker is reachable for this class"
    );
    assert_eq!(
        after["features"]["custom"], "x",
        "existing features values survive"
    );
}

#[test]
fn regression_broken_features_descriptor_refuses_the_marker() {
    // N-01: a non-object features field on a selected descriptor refuses
    // the stamp instead of silently skipping it
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    // corrupt the first descriptor's features to a string
    let manifest = read_entry(&file, "manifest.json");
    let mut manifest: Value = serde_json::from_slice(&manifest).unwrap();
    manifest["notes"][0]["features"] = json!("corrupt");
    patch_entry(
        &file,
        "manifest.json",
        &serde_json::to_vec(&manifest).unwrap(),
    );

    let output = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4), "MANIFEST_INVALID exits 4");
    assert_eq!(stderr_json(&output)["error"]["code"], "MANIFEST_INVALID");
}

#[test]
fn regression_sidecar_lock_files_are_cleaned_up() {
    // M-01: release on success, no-op and refusal — no sidecar litter
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let run = |args: &[&str]| {
        let output = insum().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&["migrate-ids", &file, "--schema", &artifact]);
    run(&["migrate-ids", &file, "--schema", &artifact]); // no-op
    let sidecars: Vec<_> = fs::read_dir(fixture.dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().contains("insum-lock"))
        .collect();
    assert!(
        sidecars.is_empty(),
        "lock sidecars are released: {sidecars:?}"
    );
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

/// B-1 / M-10: real title pages carry `noteSummary` — a structural type the
/// artifact does not declare addressable — and nested lists. The plan
/// (artifact-addressable universe) and the pre-commit validator must agree:
/// dry-run and commit both succeed, the nested blocks get ids, the
/// undeclared noteSummary stays untouched (no id, no refusal).
#[test]
fn nested_blocks_and_undeclared_note_summary_migrate() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let path = fixture.dir.path().join("nested.um");
    let notes = [FixtureNote::title_page(
        &insum_test_utils::ulid_seq(1),
        "Alpha",
        "A short summary",
        &[
            insum_test_utils::bullet_list(&["one", "two"]),
            insum_test_utils::paragraph(&[insum_test_utils::text("after the list")]),
        ],
    )];
    insum_test_utils::write_um(&path, &notes).unwrap();
    let file = path.to_string_lossy().into_owned();

    // dry-run: green; nested blocks planned, noteSummary untouched
    let output = insum()
        .args(["migrate-ids", &file, "--dry-run", "--schema", &artifact])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "dry-run must not refuse what the commit can fix: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let changes = value["notes"][0]["changes"].as_array().unwrap();
    let pointers: Vec<&str> = changes
        .iter()
        .filter_map(|c| c["pointer"].as_str())
        .collect();
    assert!(
        pointers.contains(&"/content/2/content/0/content/0"),
        "the nested list-item paragraph is planned: {pointers:?}"
    );
    assert!(
        !pointers.contains(&"/content/1"),
        "noteSummary (undeclared in the artifact) is not planned: {pointers:?}"
    );
    assert_eq!(value["would_write"], true);

    // commit: green — the validator now judges by the same artifact universe
    let output = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "commit must not fail with INTERNAL on noteSummary: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let note: Value = serde_json::from_slice(&read_entry(
        &file,
        &format!("notes/{}.json", insum_test_utils::ulid_seq(1)),
    ))
    .unwrap();
    let summary = &note["content"][1];
    assert_eq!(summary["type"], "noteSummary");
    assert!(
        summary["attrs"].get("id").is_none(),
        "the undeclared noteSummary keeps no id: {summary}"
    );
    let nested = &note["content"][2]["content"][0]["content"][0];
    assert_eq!(nested["type"], "paragraph");
    assert!(
        nested["attrs"]["id"].is_string(),
        "the nested paragraph carries an id: {nested}"
    );
}

/// m-12: the dry-run plan and the commit mapping agree on shape —
/// `pointer`, `old_id` and `type` per change for the same file state.
/// `new_id` is a preview and may differ (§7.2).
#[test]
fn dry_run_plan_shape_matches_the_commit_mapping() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();

    let dry = insum()
        .args(["migrate-ids", &file, "--dry-run", "--schema", &artifact])
        .output()
        .unwrap();
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    let dry_value: Value = serde_json::from_slice(&dry.stdout).unwrap();

    let shape = |value: &Value| -> Vec<(String, Option<String>, String)> {
        value["notes"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|note| note["changes"].as_array().unwrap().iter())
            .map(|change| {
                (
                    change["pointer"].as_str().unwrap().to_owned(),
                    change["old_id"].as_str().map(str::to_owned),
                    change["type"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let dry_shape = shape(&dry_value);
    assert!(!dry_shape.is_empty(), "the fixture plans real changes");

    let commit = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let commit_value: Value = serde_json::from_slice(&commit.stdout).unwrap();
    assert_eq!(
        dry_shape,
        shape(&commit_value),
        "dry-run plan shape == commit mapping shape"
    );
    // new_id is generated at commit time: identical previews would be
    // suspicious for the two independent runs
    let dry_ids: Vec<&str> = dry_value["notes"][0]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["new_id"].as_str())
        .collect();
    let commit_ids: Vec<&str> = commit_value["notes"][0]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["new_id"].as_str())
        .collect();
    assert_eq!(dry_ids.len(), commit_ids.len());
}

// ---------------------------------------------------------------------------
// Write-path hardening (m-6): permissions, symlinks, staged temp files
// ---------------------------------------------------------------------------

/// M-2 (§9.3): the replacement keeps the permissions of the source — a
/// 0600 note must not become world-readable after a migration.
#[cfg(unix)]
#[test]
fn commit_preserves_source_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();

    let output = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the mode of the source survives the replace");
}

/// M-3 (§9.3): a symlink source is refused — the link is never followed to
/// its target and never silently replaced by a regular file. Dry-run
/// refuses the same way (plan == commit).
#[test]
fn symlink_source_is_refused() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let target = fixture.container();
    let target_bytes = fs::read(&target).unwrap();
    let link = fixture.dir.path().join("alias.um");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    #[cfg(not(unix))]
    fs::copy(&target, &link).unwrap(); // windows: no symlinks in tests
    let link_str = link.to_string_lossy().into_owned();

    for extra in [vec!["--dry-run"], vec![]] {
        let mut args = vec!["migrate-ids", &link_str, "--schema", &artifact];
        args.extend(extra.iter());
        let output = insum().args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "IO_ERROR exits 1: {args:?}");
        assert_eq!(stderr_json(&output)["error"]["code"], "IO_ERROR");
        // the link is untouched and the target was not migrated through it
        #[cfg(unix)]
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink survives a refused write"
        );
        assert_eq!(
            fs::read(&target).unwrap(),
            target_bytes,
            "the target is untouched"
        );
    }
}

/// M-4/M-5/m-2: the staged temp file lives next to the target, is created
/// exclusively with a random name and is gone after the run — a planted
/// stale tmp (the historically predictable name) is never followed and
/// never reused, and a victim file it points at is not overwritten.
#[cfg(unix)]
#[test]
fn staged_temp_file_is_exclusive_and_cleaned_up() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let dir = fixture.dir.path();

    // plant the historically predictable name as a symlink to a victim
    let victim = dir.join("victim.txt");
    fs::write(&victim, b"do not touch").unwrap();
    let planted = dir.join(".migrate.um.migrate-tmp");
    std::os::unix::fs::symlink(&victim, &planted).unwrap();

    let output = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // the planted victim is untouched; the planted name was never followed
    assert_eq!(fs::read(&victim).unwrap(), b"do not touch");
    assert!(
        fs::symlink_metadata(&planted)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the planted file itself is untouched"
    );
    // the note path was not hijacked into a symlink
    assert!(
        fs::symlink_metadata(&file).unwrap().is_file(),
        "the target stays a regular file"
    );
    // no staged temp file survived the run (own tmp unlinked)
    let leftovers: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.contains("migrate-tmp") && name != ".migrate.um.migrate-tmp"
        })
        .collect();
    assert!(leftovers.is_empty(), "no temp leftovers: {leftovers:?}");
}

/// M-4: the pre-commit validation stages over the very bytes that will be
/// renamed, next to the target — no predictable validation file may appear
/// in the shared temp directory (the historic insum-migrate-validate-<pid>
/// staging must not exist).
#[cfg(unix)]
#[test]
fn validation_does_not_stage_into_the_shared_temp_dir() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();

    let temp = std::env::temp_dir();
    let historic = temp.join(format!("insum-migrate-validate-{}.um", std::process::id()));
    let _ = fs::remove_file(&historic);

    let output = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        !historic.exists(),
        "the historic shared-temp staging path must stay unused"
    );
    // the staged file that DID exist next to the target was 0600 and is gone
    let leftovers: Vec<_> = fs::read_dir(fixture.dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().contains("migrate-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "no temp leftovers: {leftovers:?}");
}

/// m-13: `block get --id` verdicts are pinned by code, not just exit codes:
/// a note the partial migration did not mark answers with the
/// not-guaranteed warning even though its block carries a physical
/// attrs.id (read-by-id works on presence, §4.6 п.4), and an id of
/// another note never resolves cross-note.
#[test]
fn block_get_by_id_on_unmarked_note_warns_and_never_resolves_cross_note() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();

    // partial migration: only the first note is marked
    let partial = insum()
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
        partial.status.success(),
        "{}",
        String::from_utf8_lossy(&partial.stderr)
    );

    // the unmarked second note resolves its physical id, with the warning
    // (§4.6: ids are not guaranteed without the marker)
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
        "read by id works on presence for an unmarked note: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["block"]["id"], "secondone0");
    let warnings = value["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w["code"] == "BLOCK_IDS_NOT_GUARANTEED"),
        "the unmarked note warns: {warnings:?}"
    );

    // full migration marks both notes
    let full = insum()
        .args(["migrate-ids", &file, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(
        full.status.success(),
        "{}",
        String::from_utf8_lossy(&full.stderr)
    );

    // an id of another note does not address this one (BLOCK_NOT_FOUND,
    // exit 3 — never a silent first match across notes)
    let output = insum()
        .args([
            "block",
            "get",
            &file,
            "--note",
            &insum_test_utils::ulid_seq(1),
            "--id",
            "secondone0",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        stderr_json(&output)["error"]["code"],
        "BLOCK_NOT_FOUND",
        "a foreign note's id does not resolve"
    );
}
