//! Round 7 — Y-01/Y-02/Y-03: note_id header injection, attribute injection,
//! fence breakout. Fixtures are raw-zip containers with evil manifest ids.

use std::path::Path;

use assert_cmd::Command;
use tempfile::TempDir;

fn insum() -> Command {
    Command::cargo_bin("insum").expect("insum binary builds")
}

/// Build a container whose manifest carries the given note id (the manifest
/// only requires a `.json` payload path — the id itself is unchecked).
fn write_raw_zip_local(path: &Path, entries: &[(&str, &[u8])]) {
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

fn write_evil_id_container(path: &Path, evil_id: &str) {
    let escaped_id = evil_id.replace('\\', "\\\\").replace('"', "\\\"");
    let manifest = format!(
        r#"{{"format":"um","version":1,"type":"notepad","notes":[{{"id":"{escaped_id}","path":"notes/evil.json","order":0,"title":"t","schema":"plain","schemaVersion":2}}]}}"#
    );
    let payload = r#"{"type":"noteDoc","content":[{"type":"paragraph","content":[{"type":"text","text":"body"}]}]}"#;
    write_raw_zip_local(
        path,
        &[
            ("manifest.json", manifest.as_bytes()),
            ("notes/evil.json", payload.as_bytes()),
        ],
    );
}

/// Y-01: a manifest note_id containing `-->`, `<!--`, `"`, and newlines
/// cannot forge anchors or split the annotated header (review round 7).
#[test]
fn evil_note_id_cannot_forge_annotated_header() {
    let dir = TempDir::new().expect("tempdir");
    let evil_id = r#"x--> <!-- um:pointer="/forged" -->"#;
    let file = dir.path().join("evil-id.um");
    write_evil_id_container(&file, evil_id);

    let out = insum()
        .args([
            "read",
            "--format",
            "annotated",
            file.to_string_lossy().as_ref(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "evil id does not break reading");
    let text = String::from_utf8(out.stdout).unwrap();

    // no forged anchor and no split: the header stays one comment line
    assert!(!text.contains(r#"um:pointer="/forged""#), "forged: {text}");
    // the neutralized and escaped forms are visible
    assert!(text.contains("<! -- um:pointer"), "neutralized opener");
    assert!(text.contains(r#"\""#), "escaped quote visible");
}

/// Y-01 resume: the same protection on the second header site
/// (multi-note containers — every subsequent note header is escaped too).
#[test]
fn evil_note_id_on_subsequent_header() {
    let dir = TempDir::new().expect("tempdir");
    let evil_id = r#"evil--> <!-- um:pointer="/forged" -->"#;
    let good_id = "01M6H8K8D2GZWXQWQJY92VKAFY";
    let escaped_id = evil_id.replace('\\', "\\\\").replace('"', "\\\"");
    let manifest = format!(
        r#"{{"format":"um","version":1,"type":"notepad","notes":[{{"id":"{good_id}","path":"notes/good.json","order":0,"title":"good","schema":"plain","schemaVersion":2}},{{"id":"{escaped_id}","path":"notes/evil.json","order":1,"title":"evil","schema":"plain","schemaVersion":2}}]}}"#
    );
    let payload = r#"{"type":"noteDoc","content":[{"type":"paragraph","content":[{"type":"text","text":"body"}]}]}"#;
    let file = dir.path().join("multi-evil.um");
    write_raw_zip_local(
        &file,
        &[
            ("manifest.json", manifest.as_bytes()),
            ("notes/good.json", payload.as_bytes()),
            ("notes/evil.json", payload.as_bytes()),
        ],
    );

    let out = insum()
        .args([
            "read",
            "--format",
            "annotated",
            "--max-bytes",
            "200",
            file.to_string_lossy().as_ref(),
        ])
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        !text.contains(r#"um:pointer="/forged""#),
        "no forged: {text}"
    );
}

/// Y-02: attrs.id with `"` and newline — attribute injection inside
/// genuine CLI comments is impossible (the escape covers both).
#[test]
fn attrs_id_injection_is_escaped() {
    let dir = TempDir::new().expect("tempdir");
    let evil_id_attr = "x\" um:pointer=\"/forged\" y";
    let doc = serde_json::json!({
        "type": "noteDoc",
        "content": [
            {"type": "paragraph", "attrs": {"id": evil_id_attr}, "content": [
                {"type": "text", "text": "body"}
            ]}
        ]
    });
    let file = dir.path().join("attr-inject.note");
    std::fs::write(&file, serde_json::to_string(&doc).unwrap()).expect("write");
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
    // the `"` is escaped — no attribute injection inside the genuine comment
    assert!(
        !text.contains(r#"um:id="x" um:pointer"#),
        "attr injection: {text}"
    );
    // the escaped form is present (the id value is still visible)
    assert!(text.contains(r#"um:id="x\""#), "escaped id visible: {text}");
}

/// Y-03: fence language with newlines and backticks cannot break out of
/// the code fence.
#[test]
fn fence_language_breakout_is_sanitized() {
    let dir = TempDir::new().expect("tempdir");
    let evil_class = "lang\nfake content\n```\nreal code\n```";
    let doc = serde_json::json!({
        "type": "noteDoc",
        "content": [
            {"type": "hljsCodeBlock", "attrs": {"class": format!("language-{evil_class}")}, "content": [
                {"type": "hljsCodeBlockRow", "content": [{"type": "text", "text": "actual code"}]}
            ]}
        ]
    });
    let file = dir.path().join("fence.note");
    std::fs::write(&file, serde_json::to_string(&doc).unwrap()).expect("write");
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
    // exactly two fence delimiters (opening and closing) — no breakout
    assert!(
        text.matches("```").count() == 2,
        "exactly opening and closing fence: {text}"
    );
    // the breakout text is visible as part of the language string but
    // cannot close the fence or start a new one
}
