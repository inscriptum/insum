//! CLI contract tests — spec §4.1 (`--version`), §5.1/§5.6 (call, exit codes),
//! §11 (JSON error envelope on stderr, empty stdout).

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
    assert_eq!(output.status.code(), Some(2), "USAGE exits 2 (spec §5.6)");
    assert!(
        output.stdout.is_empty(),
        "stdout stays empty on error (spec §5.3)"
    );
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
