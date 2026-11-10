//! `insum schema` and schema-aware `insum validate` — contract tests (§7.1,
//! §6.6). The artifact fixture is built with a correct self-hash via
//! `insum_core::schema::compute_hash`; a fresh temp directory stands in for
//! a non-vault location.

use std::path::Path;

use assert_cmd::Command;
use serde_json::{Value, json};

use insum_core::schema::compute_hash;
use insum_test_utils::{FixtureNote, paragraph, text, write_um};

fn insum() -> Command {
    Command::cargo_bin("insum").expect("insum binary builds")
}

/// A minimal but structurally honest artifact: noteDoc root, paragraph and
/// heading declared, one mark, correct self-hash.
fn fixture_artifact() -> Value {
    let mut raw = json!({
        "schema_id": "inscriptum",
        "schema_version": 1,
        "schema_hash": "",
        "top_node": "noteDoc",
        "validation_capability": "partial",
        "nodes": {
            "noteDoc": {"content": "noteTitle block*", "groups": [], "attrs": {},
                        "marks": [], "addressable": false, "inline_content": false,
                        "searchable": false},
            "noteTitle": {"content": "inline*", "groups": ["block"], "attrs": {},
                          "marks": [], "addressable": false, "inline_content": true,
                          "searchable": true},
            "paragraph": {"content": "inline*", "groups": ["block"],
                          "attrs": {"id": {"default": null}}, "marks": ["bold"],
                          "addressable": true, "inline_content": true, "searchable": true,
                          "example": {"type": "paragraph", "attrs": {"id": null}, "content": []}},
            "heading": {"content": "inline*", "groups": ["block"],
                        "attrs": {"id": {"default": null}, "level": {"default": 2}},
                        "marks": [], "addressable": true, "inline_content": true,
                        "searchable": true},
            "text": {"content": null, "groups": ["inline"], "attrs": {},
                     "marks": [], "addressable": false, "inline_content": true,
                     "searchable": false}
        },
        "marks": {"bold": {"group": null, "attrs": {}}}
    });
    raw["schema_hash"] = json!(compute_hash(&raw));
    raw
}

fn write_artifact(dir: &Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, serde_json::to_vec(&fixture_artifact()).unwrap()).unwrap();
    path
}

fn tampered_artifact(dir: &Path, name: &str) -> std::path::PathBuf {
    let mut raw = fixture_artifact();
    raw["validation_capability"] = json!("full"); // shift the fingerprint
    let path = dir.join(name);
    std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
    path
}

fn fixture_um(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("page.um");
    let doc = json!({
        "type": "noteDoc",
        "content": [
            {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
            paragraph(&[text("hello")])
        ]
    });
    write_um(
        &path,
        &[FixtureNote {
            id: "01TESTNOTE000000000000000".to_owned(),
            title: "T".to_owned(),
            schema: "title",
            doc,
        }],
    )
    .unwrap();
    path
}

#[test]
fn schema_reports_explicit_artifact_contract() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = write_artifact(dir.path(), "schema.json");
    let output = insum()
        .arg("--schema")
        .arg(&artifact)
        .arg("schema")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema = &value["schema"];
    assert_eq!(value["api_version"], 1);
    assert_eq!(schema["id"], "inscriptum");
    assert_eq!(schema["schema_version"], 1);
    assert!(
        schema["schema_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(schema["source"], "explicit");
    assert_eq!(
        schema["source_path"].as_str().unwrap(),
        artifact.display().to_string()
    );
    assert_eq!(schema["validation_capability"], "partial");
    assert_eq!(schema["hash_verified"], true);
    assert_eq!(schema["warnings"], json!([]));
}

#[test]
fn schema_without_vault_falls_back_to_bundled() {
    let dir = tempfile::tempdir().unwrap();
    let output = insum()
        .arg("schema")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema = &value["schema"];
    assert_eq!(schema["source"], "bundled-fallback");
    assert!(schema["source_path"].is_null());
    assert_eq!(schema["hash_verified"], true);
    // the bundled snapshot is a real Inscriptum artifact
    assert_eq!(schema["id"], "inscriptum");
    assert_eq!(schema["validation_capability"], "partial");
}

#[test]
fn schema_type_outputs_descriptor_and_example() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = write_artifact(dir.path(), "schema.json");
    let output = insum()
        .arg("--schema")
        .arg(&artifact)
        .arg("schema")
        .arg("--type")
        .arg("paragraph")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let node = &value["node"];
    assert_eq!(node["name"], "paragraph");
    assert_eq!(node["content"], "inline*");
    assert_eq!(node["addressable"], true);
    assert_eq!(node["inline_content"], true);
    assert_eq!(node["searchable"], true);
    assert_eq!(node["marks"], json!(["bold"]));
    assert!(node["attrs"]["id"].is_object());
    assert_eq!(node["example"]["type"], "paragraph");
}

#[test]
fn schema_type_unknown_is_exit6_with_available_list() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = write_artifact(dir.path(), "schema.json");
    let output = insum()
        .arg("--schema")
        .arg(&artifact)
        .arg("schema")
        .arg("--type")
        .arg("noSuchNode")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(6));
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], "UNSUPPORTED_NODE_TYPE");
    let available = value["error"]["details"]["available"].as_array().unwrap();
    assert!(available.contains(&json!("paragraph")));
    // deterministic: sorted inventory
    let mut sorted = available.clone();
    sorted.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    assert_eq!(available, &sorted);
}

#[test]
fn schema_stale_artifact_is_explicit_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = tampered_artifact(dir.path(), "schema.json");
    let output = insum()
        .arg("--schema")
        .arg(&artifact)
        .arg("schema")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "stale artifact shows, not crashes");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema"]["hash_verified"], false);
    assert_eq!(value["schema"]["warnings"][0]["code"], "SCHEMA_MISMATCH");
}

#[test]
fn validate_uses_schema_level_with_explicit_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = write_artifact(dir.path(), "schema.json");
    let um = fixture_um(dir.path());
    let output = insum()
        .arg("validate")
        .arg("--schema")
        .arg(&artifact)
        .arg(&um)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["validation_level"], "schema");
    let file = &value["files"][0];
    assert_eq!(file["validation_level"], "schema");
    assert_eq!(file["schema"]["source"], "explicit");
    assert_eq!(file["schema"]["hash_verified"], true);
    assert_eq!(file["valid"], true);
}

#[test]
fn validate_stays_structural_outside_a_vault() {
    let dir = tempfile::tempdir().unwrap();
    let um = fixture_um(dir.path());
    let output = insum().arg("validate").arg(&um).output().unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["validation_level"], "structural");
    let file = &value["files"][0];
    assert_eq!(file["validation_level"], "structural");
    // the fallback is consulted and labeled, but lifts nothing
    assert_eq!(file["schema"]["source"], "bundled-fallback");
}

#[test]
fn validate_finds_vault_artifact_by_walking_up() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_dir = dir.path().join(".obsidian/plugins/inscriptum");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("schema.json"),
        serde_json::to_vec(&fixture_artifact()).unwrap(),
    )
    .unwrap();
    let nested = dir.path().join("kb/deep");
    std::fs::create_dir_all(&nested).unwrap();
    let um_path = nested.join("page.um");
    std::fs::copy(fixture_um(dir.path()), &um_path).unwrap();

    let output = insum().arg("validate").arg(&um_path).output().unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["validation_level"], "schema");
    let file = &value["files"][0];
    assert_eq!(file["schema"]["source"], "plugin-runtime");
    assert_eq!(file["schema"]["hash_verified"], true);
}

#[test]
fn validate_with_stale_artifact_reports_mismatch_and_stays_structural() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = tampered_artifact(dir.path(), "schema.json");
    let um = fixture_um(dir.path());
    let output = insum()
        .arg("validate")
        .arg("--schema")
        .arg(&artifact)
        .arg(&um)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "staleness warns, not fails");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["validation_level"], "structural");
    let file = &value["files"][0];
    assert_eq!(file["validation_level"], "structural");
    assert_eq!(file["schema"]["hash_verified"], false);
    assert_eq!(file["warnings"][0]["code"], "SCHEMA_MISMATCH");
    assert_eq!(file["valid"], true, "a warning does not flip validity");
}

#[test]
fn validate_flags_alien_node_type_at_schema_level() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = write_artifact(dir.path(), "schema.json");
    let path = dir.path().join("alien.um");
    let doc = json!({
        "type": "noteDoc",
        "content": [
            {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
            {"type": "exoticWidget", "content": [{"type": "text", "text": "?"}]}
        ]
    });
    write_um(
        &path,
        &[FixtureNote {
            id: "01TESTNOTE000000000000000".to_owned(),
            title: "T".to_owned(),
            schema: "title",
            doc,
        }],
    )
    .unwrap();
    let output = insum()
        .arg("validate")
        .arg("--schema")
        .arg(&artifact)
        .arg(&path)
        .current_dir(dir.path())
        .output()
        .unwrap();
    // the command still succeeds as a process; the document is invalid
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["validation_level"], "schema");
    let file = &value["files"][0];
    assert_eq!(file["valid"], false);
    assert_eq!(file["notes"][0]["errors"][0]["code"], "UNKNOWN_NODE_TYPE");
}

#[test]
fn validate_reports_legacy_type_as_warning_at_schema_level() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = write_artifact(dir.path(), "schema.json");
    let path = dir.path().join("legacy.um");
    let doc = json!({
        "type": "noteDoc",
        "content": [
            {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
            {"type": "noteSummary", "content": [{"type": "text", "text": "old line"}]}
        ]
    });
    write_um(
        &path,
        &[FixtureNote {
            id: "01TESTNOTE000000000000000".to_owned(),
            title: "T".to_owned(),
            schema: "title",
            doc,
        }],
    )
    .unwrap();
    let output = insum()
        .arg("validate")
        .arg("--schema")
        .arg(&artifact)
        .arg(&path)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let file = &value["files"][0];
    assert_eq!(
        file["valid"], true,
        "legacy version semantics are not judged"
    );
    let warning_codes: Vec<&str> = file["notes"][0]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(
        warning_codes.contains(&"SCHEMA_NODE_LEGACY"),
        "{warning_codes:?}"
    );
    // --strict promotes every warning, per §6.6
    let strict = insum()
        .arg("validate")
        .arg("--strict")
        .arg("--schema")
        .arg(&artifact)
        .arg(&path)
        .current_dir(dir.path())
        .output()
        .unwrap();
    let strict_value: Value = serde_json::from_slice(&strict.stdout).unwrap();
    assert_eq!(strict_value["files"][0]["valid"], false);
}

#[test]
fn title_page_fixture_helper_matches_docs() {
    // sanity: the shared fixture helper builds a document the artifact
    // fixture accepts at schema level
    let dir = tempfile::tempdir().unwrap();
    let artifact = write_artifact(dir.path(), "schema.json");
    let um = dir.path().join("t.um");
    write_um(
        &um,
        &[FixtureNote::title_page(
            "01TESTNOTE000000000000000",
            "T",
            "S",
            &[paragraph(&[text("ok")])],
        )],
    )
    .unwrap();
    let output = insum()
        .arg("validate")
        .arg("--schema")
        .arg(&artifact)
        .arg(&um)
        .current_dir(dir.path())
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["validation_level"], "schema");
    assert_eq!(
        value["files"][0]["valid"],
        true,
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn validate_finds_vault_artifact_for_bare_filename() {
    // A bare file name has an empty parent — the vault rule must fall back
    // to the working directory instead of silently missing the artifact.
    let dir = tempfile::tempdir().unwrap();
    let plugin_dir = dir.path().join(".obsidian/plugins/inscriptum");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("schema.json"),
        serde_json::to_vec(&fixture_artifact()).unwrap(),
    )
    .unwrap();
    let um = dir.path().join("page.um");
    write_um(
        &um,
        &[FixtureNote {
            id: "01TESTNOTE000000000000000".to_owned(),
            title: "T".to_owned(),
            schema: "title",
            doc: json!({
                "type": "noteDoc",
                "content": [
                    {"type": "noteTitle", "content": [{"type": "text", "text": "T"}]},
                    paragraph(&[text("hello")])
                ]
            }),
        }],
    )
    .unwrap();

    let output = insum()
        .arg("validate")
        .arg("page.um")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let file = &value["files"][0];
    assert_eq!(
        file["schema"]["source"],
        "plugin-runtime",
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(value["validation_level"], "schema");
}

#[test]
fn validate_empty_directory_never_claims_schema_level() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let output = insum().arg("validate").arg(&empty).output().unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["validation_level"], "structural");
    assert_eq!(value["files"], json!([]));
    assert_eq!(value["valid"], true);
}

#[test]
fn validate_strict_promotes_artifact_config_warnings_to_errors() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = tampered_artifact(dir.path(), "schema.json");
    let um = fixture_um(dir.path());
    let output = insum()
        .arg("validate")
        .arg("--strict")
        .arg("--schema")
        .arg(&artifact)
        .arg(&um)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let file = &value["files"][0];
    assert_eq!(file["valid"], false);
    let error_codes: Vec<&str> = file["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_str().unwrap())
        .collect();
    assert!(
        error_codes.contains(&"SCHEMA_MISMATCH"),
        "config warnings land in errors under --strict: {file}"
    );
}

#[test]
fn validate_keeps_the_registry_code_of_an_unusable_runtime_artifact() {
    // An artifact above UM_MAX_SCHEMA_BYTES is a limit failure, not a
    // corrupt file: the report keeps the registry code of the actual class.
    let dir = tempfile::tempdir().unwrap();
    let um = fixture_um(dir.path());
    let plugin_dir = dir.path().join(".obsidian/plugins/inscriptum");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let raw = fixture_artifact().to_string();
    std::fs::write(
        plugin_dir.join("schema.json"),
        format!("{}{}", raw, " ".repeat(300)),
    )
    .unwrap();

    let output = insum()
        .env("UM_MAX_SCHEMA_BYTES", "100")
        .arg("validate")
        .arg(&um)
        .current_dir(dir.path()) // the vault rule finds the oversized artifact
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let file = &value["files"][0];
    assert_eq!(file["validation_level"], "structural");
    let warnings = file["warnings"].as_array().unwrap();
    assert_eq!(
        warnings[0]["code"],
        "LIMIT_EXCEEDED",
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn validate_degrades_a_non_compilable_artifact_to_structural() {
    // T-04: a self-consistent artifact whose content expression cannot be
    // compiled (null cycle) must degrade validate to `structural` with an
    // explicit warning — never crash, never silently claim the schema level
    let dir = tempfile::tempdir().unwrap();
    let um = fixture_um(dir.path());

    let mut raw = fixture_artifact();
    raw["nodes"]["doc"]["content"] = json!("(((a{0,0} a{0,0}))*)");
    raw["nodes"]["a"] = json!({"content": null, "groups": ["block"], "attrs": {},
                              "marks": [], "addressable": true, "inline_content": false,
                              "searchable": false});
    raw["schema_hash"] = json!(compute_hash(&raw)); // self-consistent
    let artifact = dir.path().join("cycle.json");
    std::fs::write(&artifact, serde_json::to_vec(&raw).unwrap()).unwrap();

    let output = insum()
        .arg("validate")
        .arg("--schema")
        .arg(&artifact)
        .arg(&um)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "degradation is not a crash");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["validation_level"], "structural");
    let file = &value["files"][0];
    assert_eq!(file["validation_level"], "structural");
    assert_eq!(file["valid"], true);
    let codes: Vec<&str> = file["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"SCHEMA_ARTIFACT_INVALID"), "{codes:?}");

    // --strict flips validity: the degraded configuration is a warning,
    // and §6.6 promotes warnings
    let strict = insum()
        .arg("validate")
        .arg("--strict")
        .arg("--schema")
        .arg(&artifact)
        .arg(&um)
        .current_dir(dir.path())
        .output()
        .unwrap();
    let strict_value: Value = serde_json::from_slice(&strict.stdout).unwrap();
    assert_eq!(strict_value["files"][0]["valid"], false);
    assert_eq!(strict_value["valid"], false);
}

#[test]
fn annotated_format_rejects_max_bytes() {
    // T-08: the refusal is explicit (USAGE), never a silent ignore
    let dir = tempfile::tempdir().unwrap();
    let um = fixture_um(dir.path());
    let output = insum()
        .arg("history")
        .arg(&um)
        .arg("--format")
        .arg("annotated")
        .arg("--max-bytes")
        .arg("5000")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], "USAGE");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("annotated")
    );
}
