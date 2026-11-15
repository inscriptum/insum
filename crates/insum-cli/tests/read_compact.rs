//! `read --format compact` acceptance: the anchor-free projection of every
//! supported block type, byte parity of `--with-anchors` with `annotated`,
//! the budget/cursor contract, and the format/flag combinations.
//! Fixtures come from `insum-test-utils` and are generated into a temp
//! directory per test; nothing outside it is touched. No assertion reads a
//! path out of an output — paths stay inside the opaque cursor token, so
//! the suite is separator-independent (the CI matrix runs on three OSes).

use assert_cmd::Command;
use serde_json::json;
use tempfile::TempDir;

use insum_test_utils as fixtures;

fn insum() -> Command {
    Command::cargo_bin("insum").expect("insum binary builds")
}

/// A document covering every block/inline shape the projection supports,
/// including the loss-marker paths (unknown node, unknown inline node,
/// unknown mark) and a table with pipes, a multiline cell, a ragged row
/// and an empty row.
fn rich_doc() -> serde_json::Value {
    let text = |s: &str| json!({ "type": "text", "text": s });
    let paragraph =
        |content: &[serde_json::Value]| json!({ "type": "paragraph", "content": content });
    let cell = |text_value: &str| {
        json!({
            "type": "tableCell",
            "attrs": { "colspan": 1, "rowspan": 1 },
            "content": [paragraph(&[text(text_value)])]
        })
    };
    let header = |text_value: &str| {
        json!({
            "type": "tableHeader",
            "attrs": { "colspan": 1, "rowspan": 1 },
            "content": [paragraph(&[text(text_value)])]
        })
    };
    json!({ "type": "noteDoc", "content": [
        json!({"type": "heading", "attrs": {"level": 2}, "content": [text("Раздел")]}),
        paragraph(&[
            text("Обычный "),
            json!({"type": "text", "text": "жирный", "marks": [json!({"type": "bold"})]}),
            text(", "),
            json!({"type": "text", "text": "код", "marks": [json!({"type": "code"})]}),
            text("."),
        ]),
        json!({"type": "bulletList", "content": [
            json!({"type": "listItem", "content": [paragraph(&[text("первый")])]}),
            json!({"type": "listItem", "content": [
                paragraph(&[text("второй")]),
                json!({"type": "bulletList", "content": [
                    json!({"type": "listItem", "content": [paragraph(&[text("вложенный")])]}),
                ]}),
            ]}),
        ]}),
        json!({"type": "orderedList", "attrs": {"start": 3}, "content": [
            json!({"type": "listItem", "content": [paragraph(&[text("третий")])]}),
        ]}),
        json!({"type": "taskList", "content": [
            json!({"type": "listItem", "attrs": {"checked": true}, "content": [paragraph(&[text("сделано")])]}),
            json!({"type": "listItem", "attrs": {"checked": false}, "content": [paragraph(&[text("не сделано")])]}),
        ]}),
        json!({"type": "blockquote", "content": [paragraph(&[text("цитата")])]}),
        json!({"type": "hljsCodeBlock", "attrs": {"class": "language-rust"}, "content": [
            json!({"type": "hljsCodeBlockRow", "attrs": {"class": "l"}, "content": [text("let x = 1;")]}),
        ]}),
        json!({"type": "mermaid", "attrs": {"code": "graph TD; A-->B;"}}),
        json!({"type": "horizontalRule"}),
        paragraph(&[
            text("строка 1"),
            json!({"type": "hardBreak"}),
            text("строка 2"),
            json!({"type": "wikiLink", "attrs": {"text": "метка", "path": "стр.md"}}),
        ]),
        json!({"type": "table", "content": [
            json!({"type": "tableRow", "content": [header("A|B"), header("C")]}),
            json!({"type": "tableRow", "content": [cell("x|y\nz"), cell("ok")]}),
            json!({"type": "tableRow", "content": [cell("один")]}),
            json!({"type": "tableRow", "content": []}),
        ]}),
        json!({"type": "customWidget", "attrs": {}, "content": [paragraph(&[text("внутри")])]}),
        paragraph(&[
            text("текст "),
            json!({"type": "text", "text": "с пометкой", "marks": [json!({"type": "highlight"})]}),
            json!({"type": "fancySpan", "content": [text("таинственный")]}),
        ]),
    ]})
}

struct Fixture {
    _dir: TempDir,
    path: String,
}

impl Fixture {
    fn new() -> Self {
        Self::with_doc(rich_doc())
    }

    /// A one-note container holding `doc` under the fixed note id.
    fn with_doc(doc: serde_json::Value) -> Self {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("rich.um");
        let note = fixtures::FixtureNote {
            id: "compactnote01".to_owned(),
            title: "Projection".to_owned(),
            schema: "plain",
            doc,
        };
        fixtures::write_um(&path, &[note]).expect("container fixture");
        Self {
            _dir: dir,
            path: path.to_string_lossy().into_owned(),
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        insum()
            .args(args)
            .arg(&self.path)
            .output()
            .expect("insum runs")
    }
}

fn text_out(output: &std::process::Output) -> String {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).expect("stdout is utf-8")
}

fn error_out(output: &std::process::Output) -> serde_json::Value {
    assert!(output.stdout.is_empty(), "stdout stays empty on error");
    serde_json::from_slice(&output.stderr).expect("stderr is one JSON error object")
}

#[test]
fn compact_projects_every_supported_shape_without_comments() {
    let fixture = Fixture::new();
    let out = text_out(&fixture.run(&["read", "--format", "compact"]));

    // not a single service comment, not even a stray opener
    assert!(!out.contains("<!--"), "compact must not contain comments");
    assert!(!out.contains("um:pointer"), "no anchors leak into compact");
    assert!(
        !out.contains("um:api_version"),
        "no header leaks into compact"
    );

    // the whole supported composition (§ task scope)
    assert!(out.contains("## Раздел\n"));
    assert!(out.contains("Обычный **жирный**, `код`.\n"));
    assert!(out.contains("- первый\n"));
    assert!(out.contains("- второй\n  - вложенный\n"));
    assert!(out.contains("3. третий\n"));
    assert!(out.contains("- [x] сделано\n"));
    assert!(out.contains("- [ ] не сделано\n"));
    assert!(out.contains("> цитата\n"));
    assert!(out.contains("```rust\nlet x = 1;\n```\n"));
    assert!(out.contains("```mermaid\ngraph TD; A-->B;\n```\n"));
    assert!(out.contains("---\n"));
    assert!(out.contains("строка 1\nстрока 2[метка](стр.md)"));
    // GFM table: pipes escaped, newlines as <br>, first row is the header,
    // ragged rows keep their stored shape (no padding — the annotated
    // behavior compact must reproduce), an empty row stays visible
    assert!(out.contains("| A\\|B | C |\n"));
    assert!(out.contains("| --- | --- |\n"));
    assert!(out.contains("| x\\|y<br>z | ok |\n"));
    assert!(out.contains("| один |\n"));
    assert!(out.contains("|  |\n"));
}

#[test]
fn compact_losses_are_visible_text_markers() {
    let fixture = Fixture::new();
    let out = text_out(&fixture.run(&["read", "--format", "compact"]));

    // §3.5: unknown nodes, unknown inline nodes and unknown marks stay
    // explicit — as visible text markers, not comments
    assert!(out.contains("[um:unrendered type=\"customWidget\" pointer=\"/content/11\"]"));
    assert!(out.contains("[um:unrendered-marks=\"highlight\"]"));
    assert!(out.contains("[um:unrendered-inline type=\"fancySpan\"]"));

    // annotated keeps the very same markers as comments
    let annotated = text_out(&fixture.run(&["read", "--format", "annotated"]));
    assert!(
        annotated.contains("<!-- um:unrendered type=\"customWidget\" pointer=\"/content/11\" -->")
    );
    assert!(annotated.contains("<!-- um:unrendered-marks=\"highlight\" -->"));
    assert!(annotated.contains("<!-- um:unrendered-inline type=\"fancySpan\" -->"));
}

#[test]
fn with_anchors_reproduces_annotated_byte_for_byte() {
    let fixture = Fixture::new();
    let compact_anchors = fixture.run(&["read", "--format", "compact", "--with-anchors"]);
    let annotated = fixture.run(&["read", "--format", "annotated"]);
    assert!(compact_anchors.status.success());
    assert!(annotated.status.success());
    assert_eq!(
        compact_anchors.stdout, annotated.stdout,
        "compact --with-anchors is annotated by another name"
    );
}

#[test]
fn annotated_and_json_are_unchanged() {
    let fixture = Fixture::new();
    let annotated = text_out(&fixture.run(&["read", "--format", "annotated"]));
    // the annotated header and per-block anchors keep their historical shape
    assert!(annotated.starts_with("<!-- um:api_version=1 note_id=\"compactnote01\""));
    assert!(
        annotated.contains("<!-- um:pointer=\"/content/0\" um:id=")
            || annotated.contains("<!-- um:pointer=\"/content/0\" -->")
    );

    // json regression: the envelope returns the exact fixture document
    let json_out = fixture.run(&["read", "--format", "json"]);
    let value: serde_json::Value = serde_json::from_slice(&json_out.stdout).expect("json output");
    let notes = value["notes"].as_array().expect("notes array");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["document"], rich_doc());
    assert!(value["file_rev"].as_str().is_some());
}

#[test]
fn compact_determinism_same_input_same_output() {
    let fixture = Fixture::new();
    let first = text_out(&fixture.run(&["read", "--format", "compact"]));
    let second = text_out(&fixture.run(&["read", "--format", "compact"]));
    assert_eq!(first, second, "§3.11: identical input, identical output");
}

#[test]
fn compact_budget_paginates_with_visible_cursor_marker() {
    let fixture = Fixture::new();

    // a tiny budget stops the page after whole blocks with an explicit,
    // comment-free marker
    let page1 = text_out(&fixture.run(&["read", "--format", "compact", "--max-bytes", "220"]));
    assert!(!page1.contains("<!--"));
    let marker = "[um:incomplete next_cursor=\"";
    assert!(
        page1.contains(marker),
        "compact truncation is a visible text marker, got: {page1}"
    );
    let token = page1
        .split(marker)
        .nth(1)
        .and_then(|rest| rest.split("\"]").next())
        .expect("cursor token inside the marker")
        .to_owned();

    // the resumed page continues after the last emitted block and completes
    let page2 = text_out(&fixture.run(&[
        "read",
        "--format",
        "compact",
        "--max-bytes",
        "20000",
        "--cursor",
        &token,
    ]));
    assert!(!page2.contains("[um:incomplete"));
    assert!(page2.contains("```mermaid"));
    // no block is lost or duplicated across the page boundary
    let first_block_line = page1
        .lines()
        .find(|line| line.starts_with("## "))
        .expect("page 1 has the heading");
    assert!(page1.contains(first_block_line));
    assert!(!page2.contains(first_block_line), "pages do not overlap");
    assert!(page2.contains("| A\\|B | C |"));
}

#[test]
fn annotated_budget_keeps_comment_markers() {
    let fixture = Fixture::new();
    let page1 = text_out(&fixture.run(&["read", "--format", "annotated", "--max-bytes", "400"]));
    assert!(page1.contains("<!-- um:incomplete next_cursor=\""));
}

#[test]
fn cursor_rejects_changed_note_revision() {
    let fixture = Fixture::new();
    let page1 = text_out(&fixture.run(&["read", "--format", "compact", "--max-bytes", "220"]));
    let marker = "[um:incomplete next_cursor=\"";
    let token = page1
        .split(marker)
        .nth(1)
        .and_then(|rest| rest.split("\"]").next())
        .expect("cursor token")
        .to_owned();

    // rewrite the SAME container path — the note_rev changes under the cursor
    let same_path_doc = json!({ "type": "noteDoc", "content": [
        json!({"type": "heading", "attrs": {"level": 2}, "content": [json!({"type": "text", "text": "Изменённый раздел"})]}),
    ]});
    let note = fixtures::FixtureNote {
        id: "compactnote01".to_owned(),
        title: "Projection".to_owned(),
        schema: "plain",
        doc: same_path_doc,
    };
    fixtures::write_um(std::path::Path::new(&fixture.path), &[note]).expect("rewrite container");

    let output = fixture.run(&[
        "read",
        "--format",
        "compact",
        "--max-bytes",
        "20000",
        "--cursor",
        &token,
    ]);
    assert!(!output.status.success());
    let err = error_out(&output);
    assert_eq!(err["error"]["code"], "CURSOR_STALE");
}

#[test]
fn with_anchors_outside_compact_is_a_usage_error() {
    let fixture = Fixture::new();
    for format in ["json", "annotated"] {
        let output = fixture.run(&["read", "--format", format, "--with-anchors"]);
        assert!(!output.status.success());
        let err = error_out(&output);
        assert_eq!(err["error"]["code"], "USAGE");
        assert!(
            err["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("--with-anchors applies to --format compact only")
        );
    }
}

#[test]
fn unknown_format_is_rejected_with_the_full_choice() {
    let fixture = Fixture::new();
    let output = fixture.run(&["read", "--format", "yaml"]);
    assert!(!output.status.success());
    let err = error_out(&output);
    assert_eq!(err["error"]["code"], "USAGE");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("`json`, `annotated` or `compact`")
    );
}

#[test]
fn oversize_single_block_is_emitted_whole_and_marked() {
    // a single paragraph larger than the whole budget: the documented
    // exception keeps it whole and names it explicitly
    let long_text = "длинный абзац ".repeat(40);
    let doc = json!({ "type": "noteDoc", "content": [
        json!({"type": "paragraph", "content": [json!({"type": "text", "text": long_text})]}),
    ]});
    let fixture = Fixture::with_doc(doc);
    let out = text_out(&fixture.run(&["read", "--format", "compact", "--max-bytes", "50"]));
    assert!(out.contains(long_text.trim()), "the block is emitted whole");
    assert!(out.contains("[um:oversized-block pointer=\"/content/0\" budget=\"50\"]"));
    assert!(!out.contains("<!--"));

    let annotated = text_out(&fixture.run(&["read", "--format", "annotated", "--max-bytes", "50"]));
    assert!(annotated.contains("<!-- um:oversized-block pointer=\"/content/0\" budget=\"50\" -->"));
}

#[test]
fn compact_reads_a_bare_note_file() {
    let dir = TempDir::new().expect("tempdir");
    let doc = json!({ "type": "noteDoc", "content": [
        json!({"type": "noteTitle", "content": [json!({"type": "text", "text": "Одиночная"})]}),
        json!({"type": "paragraph", "content": [json!({"type": "text", "text": "тело заметки"})]}),
    ]});
    let path = dir.path().join("single.note");
    fixtures::write_envelope_note(&path, &doc).expect("envelope fixture");
    let output = insum()
        .args(["read", "--format", "compact"])
        .arg(&path)
        .output()
        .expect("insum runs");
    let out = String::from_utf8(output.stdout).expect("utf-8");
    assert!(output.status.success());
    assert!(
        out.starts_with("# Одиночная\n"),
        "no leading blank line: {out:?}"
    );
    assert!(out.contains("тело заметки"));
    assert!(!out.contains("<!--"));
}
