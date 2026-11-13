//! `insum apply` — stage D contract tests (§8, criteria 30–44 of §15.4 and
//! the §15.5 registry checks). The artifact fixture is self-hashed via
//! `insum_core::schema::compute_hash`; write effects are verified by
//! unpacking the result. Race-style criteria (38–40) live as unit tests in
//! `insum_core::apply` — they need to mutate the file between plan and
//! commit, which a black-box process test cannot do.

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

/// A minimal runtime artifact: the title-family nodes, heading/paragraph,
/// lists and a strong mark. Self-hashed so `SchemaArtifact::parse` accepts
/// it and the write policy sees an explicit artifact (§4.7).
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
            "noteSummary": {"content": "inline*", "groups": ["block"],
                            "attrs": {"data-label": {"default": "Summary"},
                                      "role": {"default": "definition"},
                                      "id": {"default": null}},
                            "marks": [], "addressable": true, "inline_content": true,
                            "searchable": true},
            "heading": {"content": "inline*", "groups": ["block"],
                        "attrs": {"level": {"default": 2}, "id": {"default": null}},
                        "marks": ["strong"], "addressable": true, "inline_content": true,
                        "searchable": true},
            "paragraph": {"content": "inline*", "groups": ["block"],
                          "attrs": {"id": {"default": null}}, "marks": ["strong"],
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
        "marks": {
            "strong": {"attrs": {}}
        }
    });
    raw["schema_hash"] = json!(compute_hash(&raw));
    raw
}

fn write_artifact(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("schema.json");
    fs::write(&path, serde_json::to_vec(&artifact_json()).unwrap()).unwrap();
    path
}

fn stderr_json(output: &ProcessOutput) -> Value {
    assert!(output.stdout.is_empty(), "stdout stays empty on error");
    serde_json::from_slice(&output.stderr).expect("stderr is one JSON error object")
}

/// Unpack one entry of a `.um` container (zip crate — no external tools).
fn read_entry(file: &str, name: &str) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(fs::File::open(file).unwrap()).unwrap();
    let mut entry = archive.by_name(name).unwrap();
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut out).unwrap();
    out
}

fn entry_names(file: &str) -> Vec<String> {
    let mut archive = zip::ZipArchive::new(fs::File::open(file).unwrap()).unwrap();
    (0..archive.len())
        .map(|index| {
            archive
                .by_index_raw(index)
                .unwrap()
                .name()
                .unwrap()
                .into_owned()
        })
        .collect()
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

/// The shared fixture: a marked two-note container plus an asset entry.
/// Note 1 (title) carries ids on every addressable block; note 2 (plain)
/// is a single paragraph. The manifest stamps `features.block_ids`.
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

    fn note1(&self) -> String {
        insum_test_utils::ulid_seq(1)
    }

    fn note2(&self) -> String {
        insum_test_utils::ulid_seq(2)
    }

    /// Two notes, both marked; an asset rides along to prove preservation.
    fn container(&self) -> String {
        let path = self.dir.path().join("apply.um");
        let doc1 = json!({
            "type": "noteDoc",
            "content": [
                {"type": "noteTitle", "attrs": {"id": "title0id1"}, "content": [{"type": "text", "text": "Alpha"}]},
                {"type": "noteSummary", "attrs": {"id": "summ1a2r"}, "content": [{"type": "text", "text": "first"}]},
                {"type": "paragraph", "attrs": {"id": "para000001"}, "content": [{"type": "text", "text": "one"}]},
                {"type": "paragraph", "attrs": {"id": "para000002"}, "content": [{"type": "text", "text": "two"}]},
                {"type": "bulletList", "attrs": {"id": "list000001"}, "content": [
                    {"type": "listItem", "attrs": {"id": "item000001"}, "content": [
                        {"type": "paragraph", "attrs": {"id": "para000003"}, "content": [
                            {"type": "text", "text": "item one"}
                        ]}
                    ]}
                ]}
            ]
        });
        let doc2 = json!({
            "type": "noteDoc",
            "content": [
                {"type": "paragraph", "attrs": {"id": "para000004"}, "content": [
                    {"type": "text", "text": "other note body"}
                ]}
            ]
        });
        let notes = [
            FixtureNote {
                id: self.note1(),
                title: "Alpha".to_owned(),
                schema: "title",
                doc: doc1,
            },
            FixtureNote {
                id: self.note2(),
                title: "Beta".to_owned(),
                schema: "plain",
                doc: doc2,
            },
        ];
        write_um(&path, &notes).unwrap();
        // stamp the marker on both descriptors + append an asset entry
        let manifest: Value =
            serde_json::from_slice(&read_entry(&path.to_string_lossy(), "manifest.json")).unwrap();
        let mut manifest = manifest.clone();
        for entry in manifest["notes"].as_array_mut().unwrap() {
            entry["features"] = json!({
                "block_ids": 1,
                "schema": {"id": "inscriptum", "schema_version": 1,
                           "schema_hash": artifact_json()["schema_hash"]}
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
        {
            use std::io::Write as _;
            writer
                .start_file("assets/pic.png", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"\x89PNG-fake-bytes").unwrap();
        }
        let buffer = writer.finish().unwrap();
        fs::write(&path, buffer.into_inner()).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn dir_path(&self) -> std::path::PathBuf {
        self.dir.path().to_path_buf()
    }
}

/// Replace one entry's payload with new bytes (zip surgery for races).
fn patch_entry(file: &str, name: &str, new_bytes: &[u8]) {
    let mut archive = zip::ZipArchive::new(fs::File::open(file).unwrap()).unwrap();
    let buffer = std::io::Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(buffer);
    for index in 0..archive.len() {
        let raw = archive.by_index_raw(index).unwrap();
        let entry_name = raw.name().unwrap().into_owned();
        if entry_name == name {
            let last_modified = raw.last_modified().unwrap_or_default();
            drop(raw);
            writer
                .start_file(
                    entry_name.as_str(),
                    zip::write::SimpleFileOptions::default().last_modified_time(last_modified),
                )
                .unwrap();
            use std::io::Write as _;
            writer.write_all(new_bytes).unwrap();
        } else {
            writer.raw_copy_file(raw).unwrap();
        }
    }
    let buffer = writer.finish().unwrap();
    fs::write(file, buffer.into_inner()).unwrap();
}

// ---------------------------------------------------------------------------
// Criterion 30: dry-run is side-effect free
// ---------------------------------------------------------------------------

#[test]
fn dry_run_changes_no_bytes_no_mtime_and_leaves_no_temp() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let dir = fixture.dir_path();

    let before_bytes = fs::read(&file).unwrap();
    let before_mtime = fs::metadata(&file).unwrap().modified().unwrap();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    let ops = package(
        &dir,
        Some(&fixture.note1()),
        &rev,
        json!([{"op": "append_child", "parent_id": null, "nodes": [
            {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "new"}]}
        ]}]),
    );

    let output = insum()
        .args([
            "apply",
            &file,
            "--ops",
            &ops,
            "--dry-run",
            "--schema",
            &artifact,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "dry-run succeeds: {:?}",
        stderr_json(&output)
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["applied"], false);
    assert_eq!(response["dry_run"], true);
    assert_eq!(response["operations_validated"], 1);
    assert_eq!(response["would_create_count"], 1);
    assert_eq!(response["would_delete_ids"], json!([]));
    // no final IDs promised (§8.5)
    assert!(
        response.get("created_ids").is_none(),
        "dry-run carries no created_ids: {response}"
    );

    assert_eq!(fs::read(&file).unwrap(), before_bytes, "bytes untouched");
    assert_eq!(
        fs::metadata(&file).unwrap().modified().unwrap(),
        before_mtime,
        "mtime untouched"
    );
    // no temp artifacts next to the target
    let leftovers: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temp/lock leftovers: {leftovers:?}"
    );
}

// ---------------------------------------------------------------------------
// Criteria 31–36: operation semantics
// ---------------------------------------------------------------------------

#[test]
fn append_child_null_appends_to_the_root_end() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    let ops = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &rev,
        json!([{"op": "append_child", "parent_id": null, "nodes": [
            {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "last"}]}
        ]}]),
    );
    let output = insum()
        .args(["apply", &file, "--ops", &ops, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", stderr_json(&output));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["applied"], true);
    assert_eq!(response["operations_applied"], 1);
    let created = response["created_ids"].as_array().unwrap();
    assert_eq!(created.len(), 1);
    // the appended block is the LAST root child (criterion 34)
    assert_eq!(created[0]["pointer"], "/content/5");
    assert_eq!(created[0]["type"], "paragraph");
    // the created id is grammar-valid and present in the file
    let id = created[0]["id"].as_str().unwrap();
    assert_eq!(id.len(), 10);
    let doc: Value = serde_json::from_slice(&read_entry(
        &file,
        &format!("notes/{}.json", fixture.note1()),
    ))
    .unwrap();
    assert_eq!(
        doc["content"][5]["attrs"]["id"].as_str().unwrap(),
        id,
        "the created id is stamped in the payload"
    );
    assert_eq!(doc["content"][5]["content"][0]["text"], "last");
}

#[test]
fn insert_groups_preserve_their_declared_order() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    // insert_before with two nodes: [first, second, anchor] (criterion 32)
    let ops = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &rev,
        json!([{"op": "insert_before", "id": "para000002", "nodes": [
            {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "before-one"}]},
            {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "before-two"}]}
        ]}]),
    );
    let output = insum()
        .args(["apply", &file, "--ops", &ops, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", stderr_json(&output));
    let doc: Value = serde_json::from_slice(&read_entry(
        &file,
        &format!("notes/{}.json", fixture.note1()),
    ))
    .unwrap();
    let texts: Vec<&str> = doc["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| block["content"][0]["text"].as_str().unwrap_or(""))
        .collect();
    assert!(
        texts.contains(&"before-one") && texts.contains(&"before-two"),
        "texts: {texts:?}"
    );
    let i1 = texts.iter().position(|t| *t == "before-one").unwrap();
    let i2 = texts.iter().position(|t| *t == "before-two").unwrap();
    let anchor = texts.iter().position(|t| *t == "two").unwrap();
    assert!(i1 < i2 && i2 < anchor, "declared order holds: {texts:?}");
}

#[test]
fn multiple_insert_after_on_one_anchor_do_not_reverse() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    // two insert_after operations on the same anchor (criterion 33)
    let ops = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &rev,
        json!([
            {"op": "insert_after", "id": "para000001", "nodes": [
                {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "after-a1"}]},
                {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "after-a2"}]}
            ]},
            {"op": "insert_after", "id": "para000001", "nodes": [
                {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "after-b1"}]}
            ]}
        ]),
    );
    let output = insum()
        .args(["apply", &file, "--ops", &ops, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", stderr_json(&output));
    let doc: Value = serde_json::from_slice(&read_entry(
        &file,
        &format!("notes/{}.json", fixture.note1()),
    ))
    .unwrap();
    let texts: Vec<&str> = doc["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| block["content"][0]["text"].as_str().unwrap_or(""))
        .collect();
    // anchor, then group A (in order), then group B — no reversal
    let ia1 = texts.iter().position(|t| *t == "after-a1").unwrap();
    let ia2 = texts.iter().position(|t| *t == "after-a2").unwrap();
    let ib1 = texts.iter().position(|t| *t == "after-b1").unwrap();
    let anchor_pos = texts.iter().position(|t| *t == "one").unwrap();
    assert!(anchor_pos < ia1, "anchor stays put: {texts:?}");
    assert!(ia1 < ia2 && ia2 < ib1, "operation order kept: {texts:?}");
}

#[test]
fn replace_keeps_root_id_and_reports_nested_created_ids() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    // replace the bulletList with a new list of two items — the root keeps
    // its id, all nested addressable blocks are new (criterion 36)
    let ops = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &rev,
        json!([{"op": "replace", "id": "list000001", "node": {"type": "bulletList", "attrs": {}, "content": [
            {"type": "listItem", "attrs": {}, "content": [
                {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "fresh item"}]}
            ]},
            {"type": "listItem", "attrs": {}, "content": [
                {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "fresh item two"}]}
            ]}
        ]}}]),
    );
    let output = insum()
        .args(["apply", &file, "--ops", &ops, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", stderr_json(&output));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let created = response["created_ids"].as_array().unwrap();
    // two listItems + two nested paragraphs = 4 created addressable blocks
    assert_eq!(created.len(), 4, "nested blocks reported: {created:?}");
    let pointers: Vec<&str> = created
        .iter()
        .map(|entry| entry["pointer"].as_str().unwrap())
        .collect();
    // the list lives at /content/4: its pointer, items and paragraphs
    // the root keeps its id (not "created"); the first created block is the
    // first nested node of the new subtree
    assert_eq!(pointers[0], "/content/4/content/0");
    assert!(
        pointers[1].starts_with("/content/4/content/"),
        "{pointers:?}"
    );
    let doc: Value = serde_json::from_slice(&read_entry(
        &file,
        &format!("notes/{}.json", fixture.note1()),
    ))
    .unwrap();
    // the root id survived the replace
    assert_eq!(doc["content"][4]["attrs"]["id"], "list000001");
    // the deleted subtree ids are reported
    let deleted = response["deleted_ids"].as_array().unwrap();
    assert!(
        deleted.contains(&json!("item000001")) && deleted.contains(&json!("para000003")),
        "old subtree ids deleted: {deleted:?}"
    );
    // a deleted id is never reused
    let created_ids: Vec<&str> = created
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    assert!(
        !created_ids
            .iter()
            .any(|id| *id == "item000001" || *id == "para000003"),
        "created ids are fresh"
    );
}

#[test]
fn replace_content_keeps_root_id_and_preserves_subtree_ids() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    // replace_content on the list item's paragraph: the new inline content
    // replaces the text; the root id stays (criterion 35)
    let ops = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &rev,
        json!([{"op": "replace_content", "id": "para000003", "content": [
            {"type": "text", "text": "replaced "},
            {"type": "text", "text": "inline", "marks": [{"type": "strong"}]}
        ]}]),
    );
    let output = insum()
        .args(["apply", &file, "--ops", &ops, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", stderr_json(&output));
    let doc: Value = serde_json::from_slice(&read_entry(
        &file,
        &format!("notes/{}.json", fixture.note1()),
    ))
    .unwrap();
    let item_paragraph = &doc["content"][4]["content"][0]["content"][0];
    assert_eq!(item_paragraph["attrs"]["id"], "para000003", "root id kept");
    assert_eq!(item_paragraph["content"][0]["text"], "replaced ");
    assert_eq!(item_paragraph["content"][1]["marks"][0]["type"], "strong");
}

#[test]
fn set_attrs_merges_unsets_and_keeps_the_root_id() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    let ops = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &rev,
        json!([{"op": "set_attrs", "id": "para000001", "set": {"level": 3}, "unset": []}]),
    );
    let output = insum()
        .args(["apply", &file, "--ops", &ops, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", stderr_json(&output));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(response["created_ids"].as_array().unwrap().is_empty());
    let doc: Value = serde_json::from_slice(&read_entry(
        &file,
        &format!("notes/{}.json", fixture.note1()),
    ))
    .unwrap();
    assert_eq!(doc["content"][2]["attrs"]["level"], 3);
    assert_eq!(
        doc["content"][2]["attrs"]["id"], "para000001",
        "root id kept"
    );

    // unset removes the key again
    let rev2 = note_rev(&mut insum(), &file, &fixture.note1());
    let ops2 = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &rev2,
        json!([{"op": "set_attrs", "id": "para000001", "unset": ["level"]}]),
    );
    let output2 = insum()
        .args(["apply", &file, "--ops", &ops2, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output2.status.success(), "{:?}", stderr_json(&output2));
    let doc2: Value = serde_json::from_slice(&read_entry(
        &file,
        &format!("notes/{}.json", fixture.note1()),
    ))
    .unwrap();
    assert!(
        doc2["content"][2]["attrs"].get("level").is_none(),
        "unset removes the attr: {}",
        doc2["content"][2]["attrs"]
    );
}

#[test]
fn delete_removes_the_subtree_and_reports_the_ids() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    let ops = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &rev,
        json!([{"op": "delete", "id": "list000001"}]),
    );
    let output = insum()
        .args(["apply", &file, "--ops", &ops, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", stderr_json(&output));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let deleted = response["deleted_ids"].as_array().unwrap();
    // the subtree ids all leave the note
    assert_eq!(
        json!(deleted.clone()),
        json!(["item000001", "list000001", "para000003"]),
        "deleted: {}",
        serde_json::to_string(&deleted).unwrap()
    );
    let doc: Value = serde_json::from_slice(&read_entry(
        &file,
        &format!("notes/{}.json", fixture.note1()),
    ))
    .unwrap();
    assert_eq!(doc["content"].as_array().unwrap().len(), 4);
}

// ---------------------------------------------------------------------------
// Criterion 37: a failing operation leaves the file untouched
// ---------------------------------------------------------------------------

#[test]
fn a_failing_operation_leaves_the_file_untouched() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let before = fs::read(&file).unwrap();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    // first op is fine, second targets a deleted block (§8.4)
    let ops = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &rev,
        json!([
            {"op": "delete", "id": "list000001"},
            {"op": "delete", "id": "item000001"}
        ]),
    );
    let output = insum()
        .args(["apply", &file, "--ops", &ops, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = stderr_json(&output);
    assert_eq!(error["error"]["code"], "BLOCK_NOT_FOUND");
    assert_eq!(output.status.code(), Some(3), "exit 3 per §11");
    assert!(!error["error"]["hint"].is_null(), "hint present");
    assert_eq!(fs::read(&file).unwrap(), before, "bytes untouched");
}

// ---------------------------------------------------------------------------
// Criterion 41: the write honors the dirty lease
// ---------------------------------------------------------------------------

#[test]
fn a_live_dirty_lease_blocks_the_write() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    let ops = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &rev,
        json!([{"op": "delete", "id": "list000001"}]),
    );
    // a live lease of THIS process (the pid check passes on the same host)
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
    let file_name = Path::new(&file).file_name().unwrap().to_string_lossy();
    let lease_path = fixture.dir_path().join(format!(".{file_name}.insum-lease"));
    fs::write(&lease_path, serde_json::to_vec(&lease).unwrap()).unwrap();

    let before = fs::read(&file).unwrap();
    let output = insum()
        .args(["apply", &file, "--ops", &ops, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = stderr_json(&output);
    assert_eq!(error["error"]["code"], "WRITE_LOCKED");
    assert_eq!(output.status.code(), Some(7), "exit 7 per §11");
    assert_eq!(
        fs::read(&file).unwrap(),
        before,
        "the blocked write touched nothing"
    );
}

/// The hostname a live-holder check compares against (lock.rs reads it from
/// the OS; the test needs the same value for a lease this process "owns").
fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        let size = unsafe { libc_stub_hostname(&mut buf) };
        if size > 0 {
            return String::from_utf8_lossy(&buf[..size.min(255)]).into_owned();
        }
    }
    String::from("unknown")
}

// A tiny indirection so the test does not need a libc dev-dependency: the
// lease hostname only matters for the live-holder check, and the empty
// fallback still blocks conservatively (hostname recorded ≠ local hostname
// makes the holder check age-based, and a fresh lease blocks either way).
#[cfg(unix)]
unsafe fn libc_stub_hostname(buf: &mut [u8; 256]) -> usize {
    // gethostname(3) via extern — no extra dev-dependency
    unsafe extern "C" {
        fn gethostname(name: *mut u8, size: usize) -> i32;
    }
    // SAFETY: gethostname writes at most `size` bytes into the buffer
    if unsafe { gethostname(buf.as_mut_ptr(), buf.len()) } == 0 {
        buf.iter().position(|b| *b == 0).unwrap_or(255)
    } else {
        0
    }
}

#[cfg(not(unix))]
unsafe fn libc_stub_hostname(_buf: &mut [u8; 256]) -> usize {
    0
}

// ---------------------------------------------------------------------------
// Criterion 42: source safety and platform hygiene
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn symlink_source_is_refused_for_dry_run_and_commit() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let link = fixture.dir_path().join("link.um");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&file, &link).unwrap();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    let ops = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &rev,
        json!([{"op": "delete", "id": "list000001"}]),
    );
    let run_with: [&[&str]; 2] = [&["--dry-run"], &[]];
    for extra in run_with {
        let mut args: Vec<String> = vec![
            "apply".to_owned(),
            link.to_string_lossy().into_owned(),
            "--ops".to_owned(),
            ops.clone(),
            "--schema".to_owned(),
            artifact.clone(),
        ];
        args.extend(extra.iter().map(|s| (*s).to_owned()));
        let output = insum().args(&args).output().unwrap();
        assert!(!output.status.success(), "args: {args:?}");
        let error = stderr_json(&output);
        assert_eq!(
            error["error"]["code"], "IO_ERROR",
            "the link is refused before any work: {}",
            error["error"]["code"]
        );
    }
    // the target never got written through the link
    let doc: Value = serde_json::from_slice(&read_entry(
        &file,
        &format!("notes/{}.json", fixture.note1()),
    ))
    .unwrap();
    assert_eq!(doc["content"].as_array().unwrap().len(), 5);
}

#[cfg(unix)]
#[test]
fn commit_preserves_source_permissions() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    let ops = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &rev,
        json!([{"op": "delete", "id": "list000001"}]),
    );
    let output = insum()
        .args(["apply", &file, "--ops", &ops, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", stderr_json(&output));
    let mode = fs::metadata(&file).unwrap().permissions().mode();
    assert_eq!(
        mode & 0o777,
        0o640,
        "permissions preserved after the rename"
    );
}

// ---------------------------------------------------------------------------
// Criteria 43–44: container integrity and revision truth
// ---------------------------------------------------------------------------

#[test]
fn untouched_entries_keep_names_and_uncompressed_content() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let names_before = entry_names(&file);
    let asset_before = read_entry(&file, "assets/pic.png");
    let other_before = read_entry(&file, &format!("notes/{}.json", fixture.note2()));

    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    let ops = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &rev,
        json!([{"op": "delete", "id": "list000001"}]),
    );
    let output = insum()
        .args(["apply", &file, "--ops", &ops, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", stderr_json(&output));

    assert_eq!(entry_names(&file), names_before, "entry set and order kept");
    assert_eq!(read_entry(&file, "assets/pic.png"), asset_before);
    assert_eq!(
        read_entry(&file, &format!("notes/{}.json", fixture.note2())),
        other_before,
        "the other note payload is byte-identical"
    );
}

#[test]
fn new_revs_match_the_final_file() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let old_rev = note_rev(&mut insum(), &file, &fixture.note1());
    let ops = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &old_rev,
        json!([{"op": "delete", "id": "list000001"}]),
    );
    let output = insum()
        .args(["apply", &file, "--ops", &ops, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", stderr_json(&output));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();

    // new_note_rev is exactly what `insum tree` now reports (criterion 44)
    assert_eq!(
        note_rev(&mut insum(), &file, &fixture.note1()),
        response["new_note_rev"].as_str().unwrap(),
    );
    // new_file_rev is the hash of the file on disk
    let mut hasher = sha2::Sha256::new();
    use sha2::Digest as _;
    hasher.update(fs::read(&file).unwrap());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        response["new_file_rev"],
        json!(format!("sha256:{hex}")),
        "new_file_rev matches the final bytes"
    );
    assert_ne!(response["old_file_rev"], response["new_file_rev"]);
}

// ---------------------------------------------------------------------------
// §15.5: exit codes and hints for the operation-level registry
// ---------------------------------------------------------------------------

fn assert_registry_code(fixture: &Fixture, ops: Value, code: &str, exit: i32) {
    let artifact = fixture.artifact();
    let file = fixture.container();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    let package = package(&fixture.dir_path(), Some(&fixture.note1()), &rev, ops);
    let output = insum()
        .args(["apply", &file, "--ops", &package, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(!output.status.success(), "expected {code} to fail");
    let error = stderr_json(&output);
    assert_eq!(error["error"]["code"], json!(code), "error: {error}");
    assert_eq!(output.status.code(), Some(exit), "{code} exit per §11");
    assert!(
        error["error"].get("hint").is_some(),
        "{code} carries a hint"
    );
}

#[test]
fn registry_codes_carry_exit_codes_and_hints() {
    let fixture = Fixture::new();

    // BLOCK_ID_IMMUTABLE (2): set_attrs touches the id attr
    assert_registry_code(
        &fixture,
        json!([{"op": "set_attrs", "id": "para000001", "set": {"id": "hacked0001"}}]),
        "BLOCK_ID_IMMUTABLE",
        2,
    );
    // BLOCK_ID_IMMUTABLE (2): a fresh id on an inserted node
    assert_registry_code(
        &fixture,
        json!([{"op": "insert_after", "id": "para000001", "nodes": [
            {"type": "paragraph", "attrs": {"id": "freshid001"}, "content": [{"type": "text", "text": "x"}]}
        ]}]),
        "BLOCK_ID_IMMUTABLE",
        2,
    );
    // ID_CONFLICT (4): an inserted node steals an existing id
    assert_registry_code(
        &fixture,
        json!([{"op": "insert_after", "id": "para000001", "nodes": [
            {"type": "paragraph", "attrs": {"id": "para000002"}, "content": [{"type": "text", "text": "x"}]}
        ]}]),
        "ID_CONFLICT",
        4,
    );
    // INVALID_OPERATION (2): unknown op
    assert_registry_code(
        &fixture,
        json!([{"op": "move", "id": "para000001", "to": "x"}]),
        "INVALID_OPERATION",
        2,
    );
    // INVALID_NODE (4): a node that is not an object
    assert_registry_code(
        &fixture,
        json!([{"op": "insert_after", "id": "para000001", "nodes": ["not-an-object"]}]),
        "INVALID_NODE",
        4,
    );
    // BLOCK_NOT_FOUND (3)
    assert_registry_code(
        &fixture,
        json!([{"op": "delete", "id": "nope0000001"}]),
        "BLOCK_NOT_FOUND",
        3,
    );
    // CONTENT_NOT_ALLOWED (4): a text (inline) node into the root block slot
    assert_registry_code(
        &fixture,
        json!([{"op": "append_child", "parent_id": null, "nodes": [
            {"type": "text", "text": "inline into block slot"}
        ]}]),
        "CONTENT_NOT_ALLOWED",
        4,
    );
    // UNSUPPORTED_NODE_TYPE (6): a type the artifact does not declare
    assert_registry_code(
        &fixture,
        json!([{"op": "append_child", "parent_id": null, "nodes": [
            {"type": "fancyWidget", "content": [{"type": "text", "text": "?"}]}
        ]}]),
        "UNSUPPORTED_NODE_TYPE",
        6,
    );
    // BLOCK_IDS_NOT_ENABLED (6): a note without the marker (write by ID
    // requires the guarantee only migrate-ids stamps, §4.6)
    {
        let artifact = fixture.artifact();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("apply-unmarked.um");
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "paragraph", "attrs": {"id": "unmarked01"}, "content": [{"type": "text", "text": "no marker"}]}
            ]
        });
        write_um(
            &path,
            &[FixtureNote {
                id: insum_test_utils::ulid_seq(8),
                title: "Unmarked".to_owned(),
                schema: "plain",
                doc,
            }],
        )
        .unwrap();
        let file = path.to_string_lossy().into_owned();
        let rev = note_rev(&mut insum(), &file, &insum_test_utils::ulid_seq(8));
        let package = package(
            dir.path(),
            Some(&insum_test_utils::ulid_seq(8)),
            &rev,
            json!([{"op": "delete", "id": "unmarked01"}]),
        );
        let output = insum()
            .args(["apply", &file, "--ops", &package, "--schema", &artifact])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = stderr_json(&output);
        assert_eq!(error["error"]["code"], "BLOCK_IDS_NOT_ENABLED");
        assert_eq!(output.status.code(), Some(6));
    }
    // SCHEMA_UNAVAILABLE (6): no artifact applicable, bundled fallback refused
    {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("apply-solo.um");
        let doc = json!({
            "type": "noteDoc",
            "content": [
                {"type": "paragraph", "attrs": {"id": "solo0para"}, "content": [{"type": "text", "text": "solo"}]}
            ]
        });
        write_um(
            &path,
            &[FixtureNote {
                id: insum_test_utils::ulid_seq(9),
                title: "Solo".to_owned(),
                schema: "plain",
                doc,
            }],
        )
        .unwrap();
        // stamp the marker so the schema check is the first refusal
        let manifest: Value =
            serde_json::from_slice(&read_entry(&path.to_string_lossy(), "manifest.json")).unwrap();
        let mut manifest = manifest;
        manifest["notes"][0]["features"] = json!({"block_ids": 1});
        patch_entry(
            &path.to_string_lossy(),
            "manifest.json",
            &serde_json::to_vec(&manifest).unwrap(),
        );
        let file = path.to_string_lossy().into_owned();
        let rev = note_rev(&mut insum(), &file, &insum_test_utils::ulid_seq(9));
        let package = package(
            dir.path(),
            Some(&insum_test_utils::ulid_seq(9)),
            &rev,
            json!([{"op": "delete", "id": "solo0para"}]),
        );
        let output = insum()
            .current_dir(dir.path())
            .args(["apply", &file, "--ops", &package])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = stderr_json(&output);
        assert_eq!(error["error"]["code"], "SCHEMA_UNAVAILABLE", "{error}");
        assert_eq!(output.status.code(), Some(6));
    }
    // NOTE_REVISION_MISMATCH (7): stale base (built from an old revision)
    {
        let artifact = fixture.artifact();
        let file = fixture.container();
        let stale = note_rev(&mut insum(), &file, &fixture.note1());
        // a commit changes the note → the recorded revision goes stale
        let ops_first = package(
            &fixture.dir_path(),
            Some(&fixture.note1()),
            &stale,
            json!([{"op": "delete", "id": "list000001"}]),
        );
        let output = insum()
            .args(["apply", &file, "--ops", &ops_first, "--schema", &artifact])
            .output()
            .unwrap();
        assert!(output.status.success());
        let ops_stale = package(
            &fixture.dir_path(),
            Some(&fixture.note1()),
            &stale,
            json!([{"op": "delete", "id": "para000002"}]),
        );
        let output2 = insum()
            .args(["apply", &file, "--ops", &ops_stale, "--schema", &artifact])
            .output()
            .unwrap();
        let error = stderr_json(&output2);
        assert_eq!(error["error"]["code"], "NOTE_REVISION_MISMATCH", "{error}");
        assert_eq!(output2.status.code(), Some(7));
        assert!(error["error"].get("hint").is_some());
    }
}

#[test]
fn package_envelope_violations_are_usage_errors() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    let dir = &fixture.dir_path();

    // api_version 2 → USAGE (2)
    let bad = dir.join("ops-api.json");
    fs::write(
        &bad,
        json!({
            "api_version": 2,
            "note_id": fixture.note1(),
            "base_note_rev": rev,
            "operations": [],
        })
        .to_string(),
    )
    .unwrap();
    let output = insum()
        .args([
            "apply",
            &file,
            "--ops",
            &bad.to_string_lossy(),
            "--schema",
            &artifact,
        ])
        .output()
        .unwrap();
    let error = stderr_json(&output);
    assert_eq!(error["error"]["code"], "USAGE");
    assert_eq!(output.status.code(), Some(2));

    // missing note_id for a .um → USAGE
    let bad2 = dir.join("ops-noid.json");
    fs::write(
        &bad2,
        json!({
            "api_version": 1,
            "base_note_rev": rev,
            "operations": [],
        })
        .to_string(),
    )
    .unwrap();
    let output2 = insum()
        .args([
            "apply",
            &file,
            "--ops",
            &bad2.to_string_lossy(),
            "--schema",
            &artifact,
        ])
        .output()
        .unwrap();
    assert_eq!(stderr_json(&output2)["error"]["code"], "USAGE");

    // --if-match that disagrees with base_note_rev → USAGE
    let ok_ops = package(
        dir,
        Some(&fixture.note1()),
        &rev,
        json!([{"op": "delete", "id": "list000001"}]),
    );
    let output3 = insum()
        .args([
            "apply",
            &file,
            "--ops",
            &ok_ops,
            "--schema",
            &artifact,
            "--dry-run",
            "--if-match",
            &format!("sha256:{}a", &rev[7..71]),
        ])
        .output()
        .unwrap();
    assert_eq!(
        stderr_json(&output3)["error"]["code"],
        "USAGE",
        "disagreeing --if-match is a request error"
    );

    // --if-match shorter than 16 hex → USAGE
    let output4 = insum()
        .args([
            "apply",
            &file,
            "--ops",
            &ok_ops,
            "--schema",
            &artifact,
            "--dry-run",
            "--if-match",
            "sha256:abc",
        ])
        .output()
        .unwrap();
    assert_eq!(stderr_json(&output4)["error"]["code"], "USAGE");
}

#[test]
fn limits_bound_the_package() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let file = fixture.container();
    let rev = note_rev(&mut insum(), &file, &fixture.note1());
    let ops = package(
        &fixture.dir_path(),
        Some(&fixture.note1()),
        &rev,
        json!([
            {"op": "delete", "id": "list000001"},
            {"op": "delete", "id": "para000002"}
        ]),
    );
    let output = insum()
        .env("UM_MAX_APPLY_OPS", "1")
        .args([
            "apply",
            &file,
            "--ops",
            &ops,
            "--schema",
            &artifact,
            "--dry-run",
        ])
        .output()
        .unwrap();
    let error = stderr_json(&output);
    assert_eq!(error["error"]["code"], "LIMIT_EXCEEDED", "{error}");
    assert_eq!(output.status.code(), Some(5));
}

// ---------------------------------------------------------------------------
// .note path
// ---------------------------------------------------------------------------

#[test]
fn note_file_apply_commits_and_preserves_envelope_fields() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let dir = fixture.dir_path();
    let path = dir.join("apply.note");
    let doc = json!({
        "type": "noteDoc",
        "content": [
            {"type": "paragraph", "attrs": {"id": "notepara01"}, "content": [{"type": "text", "text": "note body"}]}
        ]
    });
    let envelope = json!({
        "format": "inscriptum-note",
        "version": 1,
        "features": {"block_ids": 1},
        "schema": {"id": "inscriptum", "schema_version": 1,
                   "schema_hash": artifact_json()["schema_hash"]},
        "editor_unknown": {"keep": "me"},
        "document": doc,
    });
    fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    let file = path.to_string_lossy().into_owned();

    // for .note note_rev == file_rev == hash of the whole file
    let mut hasher = sha2::Sha256::new();
    use sha2::Digest as _;
    hasher.update(fs::read(&file).unwrap());
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let rev = format!("sha256:{hex}");

    let ops = package(
        &dir,
        None,
        &rev,
        json!([{"op": "append_child", "parent_id": null, "nodes": [
            {"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "added"}]}
        ]}]),
    );
    let output = insum()
        .args(["apply", &file, "--ops", &ops, "--schema", &artifact])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", stderr_json(&output));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["note_id"], Value::Null);
    assert_eq!(response["old_note_rev"], json!(rev));
    assert_ne!(response["new_note_rev"], json!(rev));
    assert_eq!(
        response["new_file_rev"], response["new_note_rev"],
        ".note revs coincide"
    );
    let after: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(
        after["editor_unknown"],
        json!({"keep": "me"}),
        "unknown fields preserved"
    );
    assert_eq!(after["document"]["content"].as_array().unwrap().len(), 2);
    assert_eq!(after["document"]["content"][0]["attrs"]["id"], "notepara01");
    // a .note package with note_id is a request error
    let ops2 = package(
        &dir,
        Some("some-note"),
        &rev,
        json!([{"op": "delete", "id": "notepara01"}]),
    );
    let output2 = insum()
        .args(["apply", &file, "--ops", &ops2, "--schema", &artifact])
        .output()
        .unwrap();
    assert_eq!(stderr_json(&output2)["error"]["code"], "USAGE");
}

#[test]
fn legacy_note_without_the_marker_refuses_the_write() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let path = fixture.dir_path().join("legacy.note");
    let doc = json!({
        "type": "noteDoc",
        "content": [
            {"type": "paragraph", "attrs": {"id": "legacypara"}, "content": [{"type": "text", "text": "legacy"}]}
        ]
    });
    fs::write(&path, doc.to_string()).unwrap();
    let file = path.to_string_lossy().into_owned();
    let mut hasher = sha2::Sha256::new();
    use sha2::Digest as _;
    hasher.update(fs::read(&file).unwrap());
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let ops = package(
        &fixture.dir_path(),
        None,
        &format!("sha256:{hex}"),
        json!([{"op": "delete", "id": "legacypara"}]),
    );
    let output = insum()
        .args(["apply", &file, "--ops", &ops, "--schema", &artifact])
        .output()
        .unwrap();
    let error = stderr_json(&output);
    assert_eq!(error["error"]["code"], "BLOCK_IDS_NOT_ENABLED", "{error}");
    assert_eq!(output.status.code(), Some(6));
    assert!(
        error["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("migrate-ids"),
        "the hint points to migrate-ids: {error}"
    );
}
