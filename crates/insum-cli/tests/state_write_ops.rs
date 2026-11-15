//! `insum apply` state operations — `set_property`, `set_table_cell`,
//! `upsert_table_row` and the `changed_ids` response field
//! (state-write-ops; the §8.3/§8.5 contract extension). The harness mirrors
//! `apply_write.rs`: a self-hashed runtime artifact fixture, a marked
//! two-note container, write effects verified by unpacking the result.
//! The artifact adds the `noteProperty` family and the table family so the
//! state operations can validate their results.

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

/// The stage-D artifact of `apply_write.rs` extended with the property and
/// table families (the node vocabulary the state operations write into).
/// Self-hashed so `SchemaArtifact::parse` accepts it (§4.7).
fn artifact_json() -> Value {
    let mut raw = json!({
        "schema_id": "inscriptum",
        "schema_version": 1,
        "schema_hash": "",
        "top_node": "noteDoc",
        "validation_capability": "partial",
        "id_rules": {
            "attrs_key": "id",
            "pattern": "^[a-z0-9]{8,12}$",
            "recommended_length": 10
        },
        "nodes": {
            "noteDoc": {"content": "noteTitle block*", "groups": [], "attrs": {},
                        "marks": [], "addressable": false, "inline_content": false,
                        "searchable": false},
            "noteTitle": {"content": "inline*", "groups": ["block"],
                          "attrs": {"id": {"default": null}}, "marks": ["strong"],
                          "addressable": true, "inline_content": true, "searchable": true},
            "noteProperty": {"content": "notePropertyName notePropertyValue*",
                             "groups": ["block"],
                             "attrs": {"id": {"default": null},
                                       "type": {"default": "text"}},
                             "marks": [], "addressable": true,
                             "inline_content": false, "searchable": false},
            "notePropertyName": {"content": "inline*", "groups": [],
                                 "attrs": {"id": {"default": null}},
                                 "marks": ["strong"], "addressable": true,
                                 "inline_content": true, "searchable": true},
            "notePropertyValue": {"content": "inline*", "groups": [],
                                  "attrs": {"id": {"default": null}},
                                  "marks": ["strong"], "addressable": true,
                                  "inline_content": true, "searchable": true},
            "heading": {"content": "inline*", "groups": ["block"],
                        "attrs": {"level": {"default": 2}, "id": {"default": null}},
                        "marks": ["strong"], "addressable": true, "inline_content": true,
                        "searchable": true},
            "paragraph": {"content": "inline*", "groups": ["block"],
                          "attrs": {"id": {"default": null},
                                    "align": {"default": null}},
                          "marks": ["strong"],
                          "addressable": true, "inline_content": true, "searchable": true},
            "table": {"content": "tableRow+", "groups": ["block"],
                      "attrs": {"borders": {"default": null},
                                "data": {"default": null},
                                "id": {"default": null}},
                      "marks": [], "addressable": true, "inline_content": false,
                      "searchable": false},
            "tableRow": {"content": "(tableCell | tableHeader)*", "groups": [],
                         "attrs": {"id": {"default": null}}, "marks": [],
                         "addressable": true, "inline_content": false,
                         "searchable": false},
            "tableCell": {"content": "block+", "groups": [],
                          "attrs": {"backgroundColor": {"default": null},
                                    "colspan": {"default": 1},
                                    "colwidth": {"default": [50]},
                                    "id": {"default": null},
                                    "rowspan": {"default": 1}},
                          "marks": [], "addressable": true, "inline_content": false,
                          "searchable": false},
            "tableHeader": {"content": "block+", "groups": [],
                            "attrs": {"backgroundColor": {"default": null},
                                      "colspan": {"default": 1},
                                      "colwidth": {"default": [50]},
                                      "id": {"default": null},
                                      "rowspan": {"default": 1}},
                            "marks": [], "addressable": true, "inline_content": false,
                            "searchable": false},
            "text": {"content": null, "groups": ["inline"], "attrs": {},
                     "marks": [], "addressable": false, "inline_content": true,
                     "searchable": false}
        },
        "marks": {
            "strong": {"attrs": {}}
        }
    });
    raw["schema_hash"] = json!(compute_hash(&raw));
    raw
}

fn stderr_json(output: &ProcessOutput) -> Value {
    assert!(output.stdout.is_empty(), "stdout stays empty on error");
    let envelope: Value =
        serde_json::from_slice(&output.stderr).expect("stderr is one JSON error object");
    envelope["error"].clone()
}

fn note_rev(insum: &mut Command, file: &str, note: &str) -> String {
    let output = insum.args(["tree", file]).output().expect("tree runs");
    let value: Value = serde_json::from_slice(&output.stdout).expect("tree json");
    value["notes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["note_id"] == json!(note))
        .expect("note present in tree")["note_rev"]
        .as_str()
        .expect("tree carries note_rev")
        .to_owned()
}

/// A package writer: fills the note_id/base_note_rev envelope around the
/// given operations.
fn package(dir: &Path, note: Option<&str>, base: &str, ops: Value) -> String {
    let path = dir.join(format!(
        "ops-{}.json",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let envelope = json!({
        "api_version": 1,
        "note_id": note.map(|n| json!(n)).unwrap_or(Value::Null),
        "base_note_rev": base,
        "operations": ops,
    });
    fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    path.to_string_lossy().into_owned()
}

/// The shared fixture: a marked two-note container built once per test.
/// Note 1 (title page) carries two property rows (a text row and a list
/// row with two values), a two-column kanban table and a tail paragraph
/// with an attribute the artifact does not declare (the preserved-attrs
/// probe). Note 2 is a plain page without a title — the set_property
/// refusal probe. Root layout of note 1: 0 noteTitle, 1..2 noteProperty,
/// 3 heading, 4 table, 5 paragraph.
struct Fixture {
    dir: tempfile::TempDir,
    artifact: String,
    file: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let artifact = {
            let path = dir.path().join("schema.json");
            fs::write(&path, serde_json::to_vec(&artifact_json()).unwrap()).unwrap();
            path.to_string_lossy().into_owned()
        };
        let note1 = insum_test_utils::ulid_seq(1);
        let note2 = insum_test_utils::ulid_seq(2);
        let file = build_container(dir.path(), &artifact, &note1, &note2);
        Self {
            dir,
            artifact,
            file,
        }
    }

    fn note1(&self) -> String {
        insum_test_utils::ulid_seq(1)
    }

    fn note2(&self) -> String {
        insum_test_utils::ulid_seq(2)
    }

    /// Run one dry-run + commit round; returns the commit response. The
    /// dry-run must be green and must not touch the file before the commit.
    fn apply(&self, note: &str, ops: Value) -> Value {
        let rev = note_rev(&mut insum(), &self.file, note);
        let ops_path = package(self.dir.path(), Some(note), &rev, ops.clone());
        // dry-run: green, applied false, no side effect on the file
        let dry = insum()
            .args([
                "apply",
                &self.file,
                "--ops",
                &ops_path,
                "--schema",
                &self.artifact,
                "--dry-run",
            ])
            .output()
            .unwrap();
        assert!(
            dry.status.success(),
            "dry-run failed: {}",
            String::from_utf8_lossy(&dry.stderr)
        );
        let dry_json: Value = serde_json::from_slice(&dry.stdout).unwrap();
        assert_eq!(dry_json["applied"], false);
        assert_eq!(dry_json["dry_run"], true);
        // commit
        let output = insum()
            .args([
                "apply",
                &self.file,
                "--ops",
                &ops_path,
                "--schema",
                &self.artifact,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// Run one package expecting a failure; returns the error object and
    /// asserts the file was not modified.
    fn apply_failing(&self, note: &str, ops: Value) -> Value {
        let before = fs::read(&self.file).unwrap();
        let rev = note_rev(&mut insum(), &self.file, note);
        let ops_path = package(self.dir.path(), Some(note), &rev, ops);
        let output = insum()
            .args([
                "apply",
                &self.file,
                "--ops",
                &ops_path,
                "--schema",
                &self.artifact,
            ])
            .output()
            .unwrap();
        assert!(!output.status.success(), "the package must fail");
        let error = stderr_json(&output);
        assert_eq!(
            fs::read(&self.file).unwrap(),
            before,
            "the refusal touched nothing (§8.7)"
        );
        error
    }

    /// The committed payload of one note (read by entry name — the note ID
    /// IS the entry name in the fixture container).
    fn payload(&self, note: &str) -> Value {
        let bytes = read_entry(&self.file, &format!("notes/{note}.json"));
        serde_json::from_slice(&bytes).unwrap()
    }
}

/// Build the marked two-note container (§4.6: migrate-ids is the only
/// writer of the marker in production; the fixture mirrors the
/// post-migration state apply requires).
fn build_container(dir: &Path, artifact_hash_source: &str, note1: &str, note2: &str) -> String {
    let path = dir.join("state.um");
    let doc1 = json!({
        "type": "noteDoc",
        "content": [
            {"type": "noteTitle", "attrs": {"id": "title0id1"}, "content": [{"type": "text", "text": "Alpha"}]},
            {"type": "noteProperty", "attrs": {"id": "prop000001", "type": "text"}, "content": [
                {"type": "notePropertyName", "attrs": {"id": "pname00001"}, "content": [{"type": "text", "text": "description"}]},
                {"type": "notePropertyValue", "attrs": {"id": "pval000001"}, "content": [{"type": "text", "text": "first value"}]}
            ]},
            {"type": "noteProperty", "attrs": {"id": "prop000002", "type": "list"}, "content": [
                {"type": "notePropertyName", "attrs": {"id": "pname00002"}, "content": [{"type": "text", "text": "state"}]},
                {"type": "notePropertyValue", "attrs": {"id": "pval000002"}, "content": [{"type": "text", "text": "todo"}]},
                {"type": "notePropertyValue", "attrs": {"id": "pval000003"}, "content": [{"type": "text", "marks": [{"type": "strong"}], "text": "done"}]}
            ]},
            {"type": "heading", "attrs": {"id": "head000001", "level": 2}, "content": [{"type": "text", "text": "Backlog"}]},
            {"type": "table", "attrs": {"id": "tabl000001"}, "content": [
                {"type": "tableRow", "attrs": {"id": "hrow000001"}, "content": [
                    {"type": "tableHeader", "attrs": {"id": "hdr0000001"}, "content": [
                        {"type": "paragraph", "attrs": {"id": "hpar000001"}, "content": [{"type": "text", "text": "Name"}]}
                    ]},
                    {"type": "tableHeader", "attrs": {"id": "hdr0000002"}, "content": [
                        {"type": "paragraph", "attrs": {"id": "hpar000002"}, "content": [{"type": "text", "text": "State"}]}
                    ]}
                ]},
                {"type": "tableRow", "attrs": {"id": "row0000001"}, "content": [
                    {"type": "tableCell", "attrs": {"id": "cell000001"}, "content": [
                        {"type": "paragraph", "attrs": {"id": "cpar000001"}, "content": [{"type": "text", "text": "task one"}]}
                    ]},
                    {"type": "tableCell", "attrs": {"id": "cell000002"}, "content": [
                        {"type": "paragraph", "attrs": {"id": "cpar000002"}, "content": [{"type": "text", "text": "Todo"}]}
                    ]}
                ]},
                {"type": "tableRow", "attrs": {"id": "row0000002"}, "content": [
                    {"type": "tableCell", "attrs": {"id": "cell000003"}, "content": [
                        {"type": "paragraph", "attrs": {"id": "cpar000003"}, "content": [{"type": "text", "text": "task two"}]}
                    ]},
                    {"type": "tableCell", "attrs": {"id": "cell000004"}, "content": [
                        {"type": "paragraph", "attrs": {"id": "cpar000004"}, "content": [{"type": "text", "text": "Done"}]}
                    ]}
                ]}
            ]},
            {"type": "paragraph", "attrs": {"id": "para000005", "zz-custom": "keep-me"}, "content": [{"type": "text", "text": "tail"}]}
        ]
    });
    let doc2 = json!({
        "type": "noteDoc",
        "content": [
            {"type": "paragraph", "attrs": {"id": "para000006"}, "content": [
                {"type": "text", "text": "plain page body"}
            ]}
        ]
    });
    let notes = [
        FixtureNote {
            id: note1.to_owned(),
            title: "Alpha".to_owned(),
            schema: "title",
            doc: doc1,
        },
        FixtureNote {
            id: note2.to_owned(),
            title: "Beta".to_owned(),
            schema: "plain",
            doc: doc2,
        },
    ];
    write_um(&path, &notes).unwrap();
    let manifest: Value =
        serde_json::from_slice(&read_entry(&path.to_string_lossy(), "manifest.json")).unwrap();
    let mut manifest = manifest.clone();
    for entry in manifest["notes"].as_array_mut().unwrap() {
        entry["features"] = json!({
            "block_ids": 1,
            "schema": {"id": "inscriptum", "schema_version": 1,
                       "schema_hash": artifact_hash_value(artifact_hash_source)}
        });
    }
    let file = fs::OpenOptions::new().read(true).open(&path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let buffer = std::io::Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(buffer);
    for index in 0..archive.len() {
        let raw = archive.by_index_raw(index).unwrap();
        let name = raw.name().unwrap().into_owned();
        if name == "manifest.json" {
            let last_modified = raw.last_modified().unwrap_or_default();
            drop(raw);
            writer
                .start_file(
                    name.as_str(),
                    zip::write::SimpleFileOptions::default().last_modified_time(last_modified),
                )
                .unwrap();
            use std::io::Write as _;
            writer
                .write_all(serde_json::to_vec(&manifest).unwrap().as_slice())
                .unwrap();
        } else {
            writer.raw_copy_file(raw).unwrap();
        }
    }
    let buffer = writer.finish().unwrap();
    fs::write(&path, buffer.into_inner()).unwrap();
    path.to_string_lossy().into_owned()
}

/// The artifact schema_hash (recomputed here — the fixture builder only
/// receives the artifact path).
fn artifact_hash_value(_artifact_path: &str) -> Value {
    artifact_json()["schema_hash"].clone()
}

fn read_entry(file: &str, name: &str) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(fs::File::open(file).unwrap()).unwrap();
    let mut entry = archive.by_name(name).unwrap();
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut out).unwrap();
    out
}

// ---------------------------------------------------------------------------
// set_property
// ---------------------------------------------------------------------------

/// set_property on an existing name: the value node's inline content is
/// replaced, its ID survives (`changed_ids`), the row/name IDs are
/// untouched, `insum props` reads the new text, and the result passes the
/// schema validation.
#[test]
fn set_property_replaces_the_value_of_an_existing_row() {
    let fixture = Fixture::new();
    let response = fixture.apply(
        &fixture.note1(),
        json!([{"op": "set_property", "name": "description", "value": "rewritten by set_property"}]),
    );
    assert_eq!(response["applied"], true);
    assert_eq!(
        response["changed_ids"],
        json!([{ "id": "pval000001", "pointer": "/content/1/content/1",
                 "type": "notePropertyValue" }]),
        "the value node is the changed block: {response}"
    );
    assert_eq!(response["created_ids"], json!([]));
    assert_eq!(response["deleted_ids"], json!([]));

    // the node shapes survive: the row keeps its attrs, the name node is
    // untouched, the value node keeps its ID and carries the new text
    let doc = fixture.payload(&fixture.note1());
    let row = &doc["content"][1];
    assert_eq!(row["attrs"]["id"], "prop000001");
    assert_eq!(row["attrs"]["type"], "text");
    assert_eq!(row["content"][0]["attrs"]["id"], "pname00001");
    assert_eq!(row["content"][0]["content"][0]["text"], "description");
    assert_eq!(row["content"][1]["attrs"]["id"], "pval000001");
    assert_eq!(
        row["content"][1]["content"][0]["text"],
        "rewritten by set_property"
    );

    // the read side agrees with the write
    let output = insum()
        .args([
            "props",
            &fixture.file,
            "--note",
            &fixture.note1(),
            "--get",
            "description",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["properties"][0]["value"], "rewritten by set_property");

    // the committed container passes validation
    let validate = insum()
        .args(["validate", &fixture.file, "--schema", &fixture.artifact])
        .output()
        .unwrap();
    let verdict: Value = serde_json::from_slice(&validate.stdout).unwrap();
    assert_eq!(verdict["valid"], true, "{verdict}");
}

/// set_property on a name held by a list-typed row (two value nodes): the
/// first value node keeps its ID and gets the text, the second leaves the
/// note (`deleted_ids`), the row is reported changed (its child list
/// changed).
#[test]
fn set_property_collapses_a_list_row_to_one_text_value() {
    let fixture = Fixture::new();
    let response = fixture.apply(
        &fixture.note1(),
        json!([{"op": "set_property", "name": "state", "value": "review"}]),
    );
    let changed: Vec<&str> = response["changed_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    assert!(
        changed.contains(&"pval000002"),
        "the kept value node is changed: {response}"
    );
    assert!(
        changed.contains(&"prop000002"),
        "the row is changed: its child list shrank: {response}"
    );
    assert_eq!(changed.len(), 2, "no other block was touched: {response}");
    assert_eq!(
        response["deleted_ids"],
        json!(["pval000003"]),
        "the dropped second value leaves the note"
    );

    let doc = fixture.payload(&fixture.note1());
    let row = &doc["content"][2];
    assert_eq!(row["content"].as_array().unwrap().len(), 2);
    assert_eq!(row["content"][1]["attrs"]["id"], "pval000002");
    assert_eq!(row["content"][1]["content"][0]["text"], "review");
}

/// set_property on a missing name creates the row right after the existing
/// property run: the row, its name node and its value node are new
/// (`created_ids` covers them all), nothing else changes.
#[test]
fn set_property_creates_a_row_when_the_name_is_missing() {
    let fixture = Fixture::new();
    let response = fixture.apply(
        &fixture.note1(),
        json!([{"op": "set_property", "name": "owner", "value": "agent"}]),
    );
    let created: Vec<Value> = response["created_ids"].as_array().unwrap().clone();
    assert_eq!(created.len(), 3, "row + name + value: {response}");
    let pointers: Vec<&str> = created
        .iter()
        .map(|entry| entry["pointer"].as_str().unwrap())
        .collect();
    // the new row sits after the two existing property rows, at /content/3
    assert!(
        pointers.iter().all(|p| p.starts_with("/content/3")),
        "the created blocks live under the new row: {pointers:?}"
    );
    assert_eq!(created[0]["type"], "noteProperty");

    let doc = fixture.payload(&fixture.note1());
    let row = &doc["content"][3];
    assert_eq!(row["type"], "noteProperty");
    // the CLI owns the IDs (§4.4/§8.2): the created nodes carry generated
    // IDs, and the row keeps the plugin's text-type convention
    assert!(row["attrs"]["id"].as_str().is_some());
    assert_eq!(row["attrs"]["type"], "text");
    assert_eq!(row["content"][0]["content"][0]["text"], "owner");
    assert_eq!(row["content"][1]["content"][0]["text"], "agent");
    // the tail paragraph moved but keeps its ID (positions shift, IDs do not)
    assert_eq!(doc["content"][6]["attrs"]["id"], "para000005");

    let validate = insum()
        .args(["validate", &fixture.file, "--schema", &fixture.artifact])
        .output()
        .unwrap();
    let verdict: Value = serde_json::from_slice(&validate.stdout).unwrap();
    assert_eq!(verdict["valid"], true, "{verdict}");
}

/// set_property on a name held by TWO rows refuses (ambiguity is never
/// resolved silently) — the error carries the ambiguous detail and the row
/// pointers.
#[test]
fn set_property_refuses_an_ambiguous_name() {
    let fixture = Fixture::new();
    // build the ambiguity: a second noteProperty row with the name
    // "description" (inserted next to the existing one)
    let rev = note_rev(&mut insum(), &fixture.file, &fixture.note1());
    let ops_path = package(
        fixture.dir.path(),
        Some(&fixture.note1()),
        &rev,
        json!([{"op": "insert_after", "id": "prop000001", "nodes": [
            {"type": "noteProperty", "attrs": {}, "content": [
                {"type": "notePropertyName", "attrs": {}, "content": [{"type": "text", "text": "description"}]},
                {"type": "notePropertyValue", "attrs": {}, "content": [{"type": "text", "text": "shadow"}]}
            ]}
        ]}]),
    );
    let output = insum()
        .args([
            "apply",
            &fixture.file,
            "--ops",
            &ops_path,
            "--schema",
            &fixture.artifact,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let error = fixture.apply_failing(
        &fixture.note1(),
        json!([{"op": "set_property", "name": "description", "value": "x"}]),
    );
    assert_eq!(error["code"], "BLOCK_NOT_FOUND");
    assert_eq!(error["details"]["ambiguous"], true);
    let pointers: Vec<&str> = error["details"]["pointers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(pointers, vec!["/content/1", "/content/2"], "{error}");
}

/// set_property on a note without a title page refuses: noteProperty rows
/// live on the title page only (the "attempt on another page" error).
#[test]
fn set_property_refuses_a_plain_page() {
    let fixture = Fixture::new();
    let error = fixture.apply_failing(
        &fixture.note2(),
        json!([{"op": "set_property", "name": "state", "value": "todo"}]),
    );
    assert_eq!(error["code"], "INVALID_OPERATION");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("title page"), "{message}");
}

/// A stale base_note_rev refuses with NOTE_REVISION_MISMATCH (exit 7)
/// before anything is written — the CAS is inherited, not reimplemented.
#[test]
fn state_operations_cas_refuses_a_stale_revision() {
    let fixture = Fixture::new();
    let before = fs::read(&fixture.file).unwrap();
    let ops_path = package(
        fixture.dir.path(),
        Some(&fixture.note1()),
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        json!([{"op": "set_property", "name": "state", "value": "todo"}]),
    );
    let output = insum()
        .args([
            "apply",
            &fixture.file,
            "--ops",
            &ops_path,
            "--schema",
            &fixture.artifact,
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = stderr_json(&output);
    assert_eq!(error["code"], "NOTE_REVISION_MISMATCH");
    assert_eq!(output.status.code(), Some(7), "exit 7 per §11");
    assert_eq!(
        fs::read(&fixture.file).unwrap(),
        before,
        "the CAS refusal touched nothing"
    );
}

/// The text-only boundary, stated in data: replacing a value whose old
/// inline content carries a mark — the new value is one plain text node,
/// the old mark disappears (the documented v1 boundary).
#[test]
fn set_property_value_is_text_only() {
    let fixture = Fixture::new();
    // the "state" row's second value carries a strong mark; the row
    // collapses to one value and the new content carries no marks
    fixture.apply(
        &fixture.note1(),
        json!([{"op": "set_property", "name": "state", "value": "done"}]),
    );
    let doc = fixture.payload(&fixture.note1());
    let value = &doc["content"][2]["content"][1];
    assert_eq!(value["content"][0]["type"], "text");
    assert!(value["content"][0].get("marks").is_none(), "{value}");
    assert_eq!(value["content"][0]["text"], "done");
}

/// A property value with an EMPTY string mirrors the plugin's empty-value
/// shape (inline content `[]`), and the value node keeps its ID.
#[test]
fn set_property_empty_value_writes_the_plugin_empty_shape() {
    let fixture = Fixture::new();
    fixture.apply(
        &fixture.note1(),
        json!([{"op": "set_property", "name": "description", "value": ""}]),
    );
    let doc = fixture.payload(&fixture.note1());
    let value = &doc["content"][1]["content"][1];
    assert_eq!(value["content"].as_array().unwrap().len(), 0, "{value}");
    assert_eq!(value["attrs"]["id"], "pval000001", "the value ID survives");
}

// ---------------------------------------------------------------------------
// set_table_cell
// ---------------------------------------------------------------------------

/// set_table_cell by row ID + column name: the cell keeps its ID
/// (`changed_ids`), its first paragraph keeps its ID and gets the new
/// text, and `table get` reads the new value.
#[test]
fn set_table_cell_writes_by_row_id_and_column_name() {
    let fixture = Fixture::new();
    let response = fixture.apply(
        &fixture.note1(),
        json!([{"op": "set_table_cell", "id": "tabl000001", "row": {"id": "row0000002"},
                "column": {"name": "State"}, "value": "Review"}]),
    );
    assert_eq!(
        response["changed_ids"],
        json!([{ "id": "cell000004", "pointer": "/content/4/content/2/content/1",
                 "type": "tableCell" }]),
        "{response}"
    );
    assert_eq!(response["created_ids"], json!([]));

    let doc = fixture.payload(&fixture.note1());
    let cell = &doc["content"][4]["content"][2]["content"][1];
    assert_eq!(cell["attrs"]["id"], "cell000004", "the cell ID survives");
    // the first paragraph keeps its identity (the §4.4 rule for an ordinary
    // text change) and carries the new text
    assert_eq!(cell["content"][0]["type"], "paragraph");
    assert_eq!(cell["content"][0]["content"][0]["text"], "Review");

    let output = insum()
        .args([
            "table",
            "get",
            &fixture.file,
            "--note",
            &fixture.note1(),
            "--id",
            "tabl000001",
            "--where",
            "State=Review",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["table"]["rows_matched"], 1,
        "the new text is readable: {value}"
    );
}

/// set_table_cell by key columns + header cell ID: the where-form matches
/// exactly one data row, the column resolves by its header cell block ID.
#[test]
fn set_table_cell_writes_by_keys_and_column_id() {
    let fixture = Fixture::new();
    let response = fixture.apply(
        &fixture.note1(),
        json!([{"op": "set_table_cell", "id": "tabl000001", "row": {"where": {"Name": "task one"}},
                "column": {"id": "hdr0000002"}, "value": "In progress"}]),
    );
    let changed: Vec<&str> = response["changed_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    assert_eq!(changed, vec!["cell000002"], "{response}");
}

/// set_table_cell on an unknown table ID, unknown row ID, a key match with
/// zero hits and an unknown column — BLOCK_NOT_FOUND with the honest
/// message each time.
#[test]
fn set_table_cell_misses_refuse() {
    let fixture = Fixture::new();
    let cases = vec![
        (
            json!([{"op": "set_table_cell", "id": "nope000001", "row": {"id": "row0000001"},
                    "column": {"name": "State"}, "value": "x"}]),
            "nope000001",
        ),
        (
            json!([{"op": "set_table_cell", "id": "tabl000001", "row": {"id": "nope000002"},
                    "column": {"name": "State"}, "value": "x"}]),
            "nope000002",
        ),
        (
            json!([{"op": "set_table_cell", "id": "tabl000001", "row": {"where": {"Name": "missing"}},
                    "column": {"name": "State"}, "value": "x"}]),
            "no data row",
        ),
        (
            json!([{"op": "set_table_cell", "id": "tabl000001", "row": {"id": "row0000001"},
                    "column": {"name": "Nope"}, "value": "x"}]),
            "Nope",
        ),
    ];
    for (ops, needle) in cases {
        let error = fixture.apply_failing(&fixture.note1(), ops);
        assert_eq!(error["code"], "BLOCK_NOT_FOUND", "{error}");
        assert!(
            error["message"].as_str().unwrap().contains(needle),
            "the message names the miss: {error}"
        );
    }
}

/// A duplicated header name poisons the column lookup even when the queried
/// name is unique (the same strict rule as `table get --where`).
#[test]
fn set_table_cell_refuses_on_duplicate_column_names() {
    let fixture = Fixture::new();
    // build a table with duplicate headers in the fixture note via append
    let response = fixture.apply(
        &fixture.note1(),
        json!([{"op": "append_child", "parent_id": null, "nodes": [
            {"type": "table", "attrs": {}, "content": [
                {"type": "tableRow", "attrs": {}, "content": [
                    {"type": "tableHeader", "attrs": {}, "content": [
                        {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "Tag"}]}
                    ]},
                    {"type": "tableHeader", "attrs": {}, "content": [
                        {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "Tag"}]}
                    ]}
                ]},
                {"type": "tableRow", "attrs": {}, "content": [
                    {"type": "tableCell", "attrs": {}, "content": [
                        {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "a"}]}
                    ]},
                    {"type": "tableCell", "attrs": {}, "content": [
                        {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "b"}]}
                    ]}
                ]}
            ]}
        ]}]),
    );
    let table_id = response["created_ids"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // the input nodes carried no IDs (§8.2) — take the generated data-row
    // ID from the response (the last tableRow of the appended table)
    let row_id = response["created_ids"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|entry| entry["type"] == json!("tableRow"))
        .expect("the appended rows are created")["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let error = fixture.apply_failing(
        &fixture.note1(),
        json!([{"op": "set_table_cell", "id": table_id, "row": {"id": row_id},
                "column": {"name": "Tag"}, "value": "x"}]),
    );
    assert_eq!(error["code"], "BLOCK_NOT_FOUND");
    assert_eq!(error["details"]["ambiguous"], true, "{error}");
}

/// A ragged data row (fewer cells than the header defines columns) refuses
/// set_table_cell on the missing cell — cell creation belongs to
/// upsert_table_row.
#[test]
fn set_table_cell_refuses_a_missing_cell() {
    let fixture = Fixture::new();
    // append a short row (one cell against a two-column header); the row
    // receives its ID from the CLI at commit
    let response = fixture.apply(
        &fixture.note1(),
        json!([{"op": "insert_after", "id": "row0000002", "nodes": [
            {"type": "tableRow", "attrs": {}, "content": [
                {"type": "tableCell", "attrs": {}, "content": [
                    {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "ragged"}]}
                ]}
            ]}
        ]}]),
    );
    let row_id = response["created_ids"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let error = fixture.apply_failing(
        &fixture.note1(),
        json!([{"op": "set_table_cell", "id": "tabl000001", "row": {"id": row_id},
                "column": {"name": "State"}, "value": "x"}]),
    );
    assert_eq!(error["code"], "BLOCK_NOT_FOUND", "{error}");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("does not create cells"), "{message}");
    assert!(
        error["hint"].as_str().unwrap().contains("upsert_table_row"),
        "{error}"
    );
}

/// A table carrying a colspan cell is not a plain grid: both state
/// operations refuse rather than guess the mapping.
#[test]
fn state_table_ops_refuse_spanning_cells() {
    let fixture = Fixture::new();
    let response = fixture.apply(
        &fixture.note1(),
        json!([{"op": "append_child", "parent_id": null, "nodes": [
            {"type": "table", "attrs": {}, "content": [
                {"type": "tableRow", "attrs": {}, "content": [
                    {"type": "tableCell", "attrs": {"colspan": 2}, "content": [
                        {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "wide"}]}
                    ]}
                ]}
            ]}
        ]}]),
    );
    let table_id = response["created_ids"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let row_id = response["created_ids"][1]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let error = fixture.apply_failing(
        &fixture.note1(),
        json!([{"op": "set_table_cell", "id": table_id, "row": {"id": row_id},
                "column": {"name": "wide"}, "value": "x"}]),
    );
    assert_eq!(error["code"], "INVALID_OPERATION", "{error}");
    assert!(
        error["message"].as_str().unwrap().contains("colspan"),
        "{error}"
    );

    let error = fixture.apply_failing(
        &fixture.note1(),
        json!([{"op": "upsert_table_row", "id": table_id, "row": {"wide": "x"}}]),
    );
    assert_eq!(error["code"], "INVALID_OPERATION", "{error}");
}

/// A block ID that is not a table refuses with the type named.
#[test]
fn set_table_cell_refuses_a_non_table_target() {
    let fixture = Fixture::new();
    let error = fixture.apply_failing(
        &fixture.note1(),
        json!([{"op": "set_table_cell", "id": "para000005", "row": {"id": "row0000001"},
                "column": {"name": "State"}, "value": "x"}]),
    );
    assert_eq!(error["code"], "INVALID_OPERATION", "{error}");
    assert!(
        error["message"].as_str().unwrap().contains("paragraph"),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// upsert_table_row
// ---------------------------------------------------------------------------

/// upsert_table_row with no key match inserts a fresh row on top (right
/// after the header row): every cell of the row is new — row, cells AND
/// their paragraphs all land in `created_ids` (nested included).
#[test]
fn upsert_table_row_inserts_on_top_after_the_header() {
    let fixture = Fixture::new();
    let response = fixture.apply(
        &fixture.note1(),
        json!([{"op": "upsert_table_row", "id": "tabl000001",
                "row": {"Name": "task three", "State": "Todo"},
                "position": "top"}]),
    );
    let created: Vec<Value> = response["created_ids"].as_array().unwrap().clone();
    // row + 2 cells + their 2 paragraphs = 5 new addressable blocks
    assert_eq!(
        created.len(),
        5,
        "row, cells and nested paragraphs: {response}"
    );
    let pointers: Vec<&str> = created
        .iter()
        .map(|entry| entry["pointer"].as_str().unwrap())
        .collect();
    assert!(
        pointers
            .iter()
            .all(|p| p.starts_with("/content/4/content/1")),
        "the new row sits right after the header row (index 1): {pointers:?}"
    );
    assert_eq!(created[0]["type"], "tableRow");
    assert_eq!(response["changed_ids"], json!([]));

    let doc = fixture.payload(&fixture.note1());
    let rows = doc["content"][4]["content"].as_array().unwrap();
    assert_eq!(rows.len(), 4, "header + 2 data rows + the inserted one");
    let inserted = &rows[1];
    assert_eq!(inserted["type"], "tableRow");
    let cells = inserted["content"].as_array().unwrap();
    assert_eq!(cells.len(), 2, "one cell per header column");
    assert_eq!(cells[0]["content"][0]["content"][0]["text"], "task three");
    assert_eq!(cells[1]["content"][0]["content"][0]["text"], "Todo");

    let validate = insum()
        .args(["validate", &fixture.file, "--schema", &fixture.artifact])
        .output()
        .unwrap();
    let verdict: Value = serde_json::from_slice(&validate.stdout).unwrap();
    assert_eq!(verdict["valid"], true, "{verdict}");
}

/// upsert_table_row without a `position` appends at the bottom (the
/// default).
#[test]
fn upsert_table_row_inserts_at_the_bottom_by_default() {
    let fixture = Fixture::new();
    let response = fixture.apply(
        &fixture.note1(),
        json!([{"op": "upsert_table_row", "id": "tabl000001",
                "row": {"Name": "task four", "State": "Done"}}]),
    );
    let created: Vec<Value> = response["created_ids"].as_array().unwrap().clone();
    let pointers: Vec<&str> = created
        .iter()
        .map(|entry| entry["pointer"].as_str().unwrap())
        .collect();
    assert!(
        pointers
            .iter()
            .all(|p| p.starts_with("/content/4/content/3")),
        "the new row sits after the last data row: {pointers:?}"
    );

    let doc = fixture.payload(&fixture.note1());
    let rows = doc["content"][4]["content"].as_array().unwrap();
    assert_eq!(
        rows[3]["content"][0]["content"][0]["content"][0]["text"],
        "task four"
    );
}

/// upsert_table_row with a key match rewrites only the mentioned cells of
/// the matched row: written cells land in `changed_ids`, unmentioned cells
/// keep their content, nothing new is created.
#[test]
fn upsert_table_row_updates_the_matched_row() {
    let fixture = Fixture::new();
    let response = fixture.apply(
        &fixture.note1(),
        json!([{"op": "upsert_table_row", "id": "tabl000001",
                "match": {"Name": "task two"},
                "row": {"State": "Review"}}]),
    );
    let changed: Vec<&str> = response["changed_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        changed,
        vec!["cell000004"],
        "only the written cell: {response}"
    );
    assert_eq!(response["created_ids"], json!([]));
    assert_eq!(response["deleted_ids"], json!([]));

    // the unmentioned Name cell is untouched
    let doc = fixture.payload(&fixture.note1());
    let row = &doc["content"][4]["content"][2];
    assert_eq!(
        row["content"][0]["content"][0]["content"][0]["text"],
        "task two"
    );
    assert_eq!(
        row["content"][1]["content"][0]["content"][0]["text"],
        "Review"
    );
}

/// upsert_table_row creates missing cells when the matched row is short:
/// the target cell carries the text, the created cells land in
/// `created_ids`, and the row (its child list changed) is reported
/// changed.
#[test]
fn upsert_table_row_creates_missing_cells_of_a_short_row() {
    let fixture = Fixture::new();
    // append a one-cell row against the two-column header; the row
    // receives its ID from the CLI at commit
    let appended = fixture.apply(
        &fixture.note1(),
        json!([{"op": "insert_after", "id": "row0000002", "nodes": [
            {"type": "tableRow", "attrs": {}, "content": [
                {"type": "tableCell", "attrs": {}, "content": [
                    {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "ragged"}]}
                ]}
            ]}
        ]}]),
    );
    let short_row_id = appended["created_ids"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let response = fixture.apply(
        &fixture.note1(),
        json!([{"op": "upsert_table_row", "id": "tabl000001",
                "match": {"Name": "ragged"},
                "row": {"Name": "ragged", "State": "Todo"}}]),
    );
    // the Name cell exists (its paragraph keeps its identity), the State
    // cell and its paragraph are new, the row gained a cell
    let changed: Vec<&str> = response["changed_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    assert!(
        changed.contains(&short_row_id.as_str()),
        "the row gained cells: {response}"
    );
    let created: Vec<Value> = response["created_ids"].as_array().unwrap().clone();
    let types: Vec<&str> = created
        .iter()
        .map(|entry| entry["type"].as_str().unwrap())
        .collect();
    assert!(
        types.contains(&"tableCell"),
        "the created target cell is reported: {response}"
    );

    let doc = fixture.payload(&fixture.note1());
    let rows = doc["content"][4]["content"].as_array().unwrap();
    let short = &rows[3];
    assert_eq!(short["content"].as_array().unwrap().len(), 2, "{short}");
    assert_eq!(
        short["content"][0]["content"][0]["content"][0]["text"],
        "ragged"
    );
    assert_eq!(
        short["content"][1]["content"][0]["content"][0]["text"],
        "Todo"
    );

    let validate = insum()
        .args(["validate", &fixture.file, "--schema", &fixture.artifact])
        .output()
        .unwrap();
    let verdict: Value = serde_json::from_slice(&validate.stdout).unwrap();
    assert_eq!(verdict["valid"], true, "{verdict}");
}

/// upsert_table_row with several key matches refuses (ambiguity), with a
/// payload naming an unknown column refuses, and an empty `row` map is a
/// malformed operation.
#[test]
fn upsert_table_row_refuses_ambiguous_and_unknown_columns() {
    let fixture = Fixture::new();
    // two rows share the State "Todo" (the fixture has one; create a second)
    fixture.apply(
        &fixture.note1(),
        json!([{"op": "upsert_table_row", "id": "tabl000001",
                "row": {"Name": "task three", "State": "Todo"}}]),
    );
    let error = fixture.apply_failing(
        &fixture.note1(),
        json!([{"op": "upsert_table_row", "id": "tabl000001",
                "match": {"State": "Todo"},
                "row": {"Name": "x"}}]),
    );
    assert_eq!(error["code"], "BLOCK_NOT_FOUND", "{error}");
    assert_eq!(error["details"]["ambiguous"], true);

    let error = fixture.apply_failing(
        &fixture.note1(),
        json!([{"op": "upsert_table_row", "id": "tabl000001",
                "row": {"Nope": "x"}}]),
    );
    assert_eq!(error["code"], "BLOCK_NOT_FOUND", "{error}");
    assert!(
        error["message"].as_str().unwrap().contains("Nope"),
        "{error}"
    );

    let error = fixture.apply_failing(
        &fixture.note1(),
        json!([{"op": "upsert_table_row", "id": "tabl000001", "row": {}}]),
    );
    assert_eq!(error["code"], "INVALID_OPERATION", "{error}");
}

// ---------------------------------------------------------------------------
// package-level guarantees inherited by the state operations
// ---------------------------------------------------------------------------

/// A failing operation anywhere in the package leaves the file untouched
/// (§8.7 all-or-nothing): the set_property succeeds in memory but the
/// second operation misses, and the container must stay byte-identical.
#[test]
fn a_failing_state_operation_keeps_the_file_untouched() {
    let fixture = Fixture::new();
    let error = fixture.apply_failing(
        &fixture.note1(),
        json!([
            {"op": "set_property", "name": "description", "value": "should not land"},
            {"op": "set_table_cell", "id": "nope000009", "row": {"id": "row0000001"},
             "column": {"name": "State"}, "value": "x"}
        ]),
    );
    assert_eq!(error["code"], "BLOCK_NOT_FOUND");
    let doc = fixture.payload(&fixture.note1());
    assert_eq!(
        doc["content"][1]["content"][1]["content"][0]["text"], "first value",
        "the first operation was rolled back with the package"
    );
}

/// Untouched blocks keep their bytes: the tail paragraph's unknown-to-the-
/// artifact attribute (`zz-custom`) and its ID survive the state writes
/// (§3.10 — no silent losses outside the operation's own targets).
#[test]
fn state_writes_preserve_unrelated_blocks_and_unknown_attrs() {
    let fixture = Fixture::new();
    fixture.apply(
        &fixture.note1(),
        json!([
            {"op": "set_property", "name": "description", "value": "preserved around"},
            {"op": "set_table_cell", "id": "tabl000001", "row": {"id": "row0000001"},
             "column": {"name": "State"}, "value": "Done"}
        ]),
    );
    let doc = fixture.payload(&fixture.note1());
    let tail = &doc["content"][5];
    assert_eq!(tail["type"], "paragraph");
    assert_eq!(tail["attrs"]["id"], "para000005");
    assert_eq!(tail["attrs"]["zz-custom"], "keep-me");
    assert_eq!(tail["content"][0]["text"], "tail");
}

/// A live dirty lease blocks the state operations exactly like every other
/// write (§9.2 — the lock pipeline is shared, not reimplemented).
#[test]
fn a_live_dirty_lease_blocks_the_state_writes() {
    let fixture = Fixture::new();
    let rev = note_rev(&mut insum(), &fixture.file, &fixture.note1());
    let ops_path = package(
        fixture.dir.path(),
        Some(&fixture.note1()),
        &rev,
        json!([{"op": "set_property", "name": "description", "value": "blocked"}]),
    );
    let lease = json!({
        "format": 1,
        "kind": "lease",
        "holder": {"kind": "plugin", "pid": std::process::id(), "hostname": hostname()},
        "op_id": "leasetest01",
        "created_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    });
    let file_name = Path::new(&fixture.file)
        .file_name()
        .unwrap()
        .to_string_lossy();
    let lease_path = fixture.dir.path().join(format!(".{file_name}.insum-lease"));
    fs::write(&lease_path, serde_json::to_vec(&lease).unwrap()).unwrap();

    let before = fs::read(&fixture.file).unwrap();
    let output = insum()
        .args([
            "apply",
            &fixture.file,
            "--ops",
            &ops_path,
            "--schema",
            &fixture.artifact,
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = stderr_json(&output);
    assert_eq!(error["code"], "WRITE_LOCKED");
    assert_eq!(output.status.code(), Some(7), "exit 7 per §11");
    assert_eq!(
        fs::read(&fixture.file).unwrap(),
        before,
        "the blocked write touched nothing"
    );
}

/// The hostname a live-holder check compares against — the same source
/// lock.rs `current_hostname` reads (see apply_write.rs for the platform
/// notes; duplicated here because integration tests are separate crates).
#[cfg(unix)]
fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most `buf.len()` bytes
    let failed = unsafe { gethostname(buf.as_mut_ptr(), buf.len()) } != 0;
    if failed {
        return String::from("unknown");
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(255);
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[cfg(windows)]
fn hostname() -> String {
    use windows_sys::Win32::System::SystemInformation::{
        ComputerNameDnsHostname, GetComputerNameExW,
    };
    // SAFETY: the documented two-call pattern — a null-buffer call for the
    // required size in UTF-16 units, then a buffer of exactly that size
    unsafe {
        let mut size: u32 = 0;
        let _ = GetComputerNameExW(ComputerNameDnsHostname, std::ptr::null_mut(), &mut size);
        if size == 0 {
            return String::from("unknown");
        }
        let mut buffer = vec![0u16; size as usize];
        if GetComputerNameExW(ComputerNameDnsHostname, buffer.as_mut_ptr(), &mut size) == 0 {
            return String::from("unknown");
        }
        String::from_utf16_lossy(&buffer[..size as usize])
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn gethostname(name: *mut u8, size: usize) -> i32;
}
