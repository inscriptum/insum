//! CLI contract tests — (`--version`),/ (call, exit codes),
//! (JSON error envelope on stderr, empty stdout).

use assert_cmd::Command;
use serde_json::Value;

fn insum() -> Command {
    Command::cargo_bin("insum").expect("insum binary builds")
}

#[test]
fn version_outputs_json_contract() {
    let output = insum().arg("--version").output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
    assert_eq!(value["api_version"], 1);
    assert!(value["cli_version"].as_str().unwrap().starts_with("0."));
    assert_eq!(value["api_versions"], serde_json::json!([1]));
    assert_eq!(value["um_versions"], serde_json::json!([1]));
    // exactly one JSON document and a trailing newline
    assert!(stdout.ends_with('\n'));
}

#[test]
fn pretty_flag_indents_version_output() {
    let compact = insum().arg("--version").output().unwrap();
    let pretty = insum().arg("--pretty").arg("--version").output().unwrap();
    let compact_text = String::from_utf8(compact.stdout).unwrap();
    let pretty_text = String::from_utf8(pretty.stdout).unwrap();
    // same document, different formatting
    let a: Value = serde_json::from_str(&compact_text).unwrap();
    let b: Value = serde_json::from_str(&pretty_text).unwrap();
    assert_eq!(a, b);
    assert!(pretty_text.contains('\n'), "pretty output is multi-line");
    // --pretty works before and after the subcommand-less flag position
    let after = insum().arg("--version").arg("--pretty").output().unwrap();
    assert!(String::from_utf8(after.stdout).unwrap().contains('\n'));
}

#[test]
fn help_outputs_text_and_exits_zero() {
    let output = insum().arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("insum"));
    assert!(stdout.contains("Usage"));
}

#[test]
fn no_arguments_print_help_and_exit_zero() {
    let output = insum().output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Usage"));
}

#[test]
fn unknown_command_is_json_usage_error() {
    let output = insum().arg("frobnicate").output().unwrap();
    assert_eq!(output.status.code(), Some(2), "USAGE exits 2");
    assert!(output.stdout.is_empty(), "stdout stays empty on error");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let value: Value = serde_json::from_str(&stderr).expect("stderr is one JSON error object");
    assert_eq!(value["api_version"], 1);
    assert_eq!(value["error"]["code"], "USAGE");
}

#[test]
fn unknown_flag_is_json_usage_error() {
    let output = insum().arg("--definitely-not-a-flag").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_str(&String::from_utf8(output.stderr).unwrap()).unwrap();
    assert_eq!(value["error"]["code"], "USAGE");
}

#[test]
fn named_non_um_file_is_reported_not_silently_dropped() {
    // §3.5: an explicitly named input that is not a .um/.note must not
    // produce a silent valid:true / complete:true (K-01)
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain.txt");
    std::fs::write(&plain, b"not a document").unwrap();

    // validate: the file is an invalid entry, not an empty success
    let output = Command::cargo_bin("insum")
        .unwrap()
        .arg("validate")
        .arg(&plain)
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["valid"],
        false,
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let file = &value["files"][0];
    assert_eq!(file["path"].as_str().unwrap(), plain.display().to_string());
    assert_eq!(file["valid"], false);
    assert!(
        !file["errors"].as_array().unwrap().is_empty(),
        "an error names the unsupported input: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    // search: the skipped entry lowers complete — zero matches with
    // complete:true would be a false absence proof
    let output = Command::cargo_bin("insum")
        .unwrap()
        .arg("search")
        .arg("-F")
        .arg("anything")
        .arg(&plain)
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["complete"],
        false,
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !value["skipped"].as_array().unwrap().is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
