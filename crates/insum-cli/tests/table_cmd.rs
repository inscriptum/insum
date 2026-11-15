//! `insum table get` acceptance tests — addressing (--index/--id/
//! --after-heading), the `--where` filter, ragged/duplicate/span/nested
//! diagnostics, pipe escaping (roundtrip value-echo), output budget and the
//! read-only guarantee. Fixtures come from `insum-test-utils`, generated
//! into a temp directory per test; nothing outside it is touched.

use std::fs;
use std::process::Output as ProcessOutput;

use assert_cmd::Command;
use serde_json::{Value, json};
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

fn fail_json(args: &[&str]) -> (i32, Value) {
    let output = insum().args(args).output().expect("insum runs");
    (
        output.status.code().expect("exit code"),
        stderr_json(&output),
    )
}

fn stderr_json(output: &ProcessOutput) -> Value {
    assert!(output.stdout.is_empty(), "stdout stays empty on error");
    serde_json::from_slice(&output.stderr).expect("stderr is one JSON error object")
}

/// Document blocks of the table corpus. Layout (top-level positions):
/// 0 heading Board, 1 table BOARD, 2 heading Ragged, 3 table RAGGED,
/// 4 heading Dup, 5 table DUP, 6 heading Spans, 7 table SPANS,
/// 8 heading Nested, 9 table NESTED, 10 heading Dup (again), 11 table DUP2,
/// 12 heading Weird, 13 table WEIRD — seven tables in document order.
fn corpus_blocks() -> Vec<Value> {
    let board = json!({
        "type": "table",
        "attrs": { "id": "tblboard01" },
        "content": [
            fixtures::table_row(Some("rowhdr01"), &[
                fixtures::table_header(Some("colname1"), "Name"),
                fixtures::table_header(Some("colstat1"), "State"),
                fixtures::table_header(Some("coldate1"), "Date"),
            ]),
            fixtures::table_row(Some("rowtask1"), &[
                fixtures::table_cell_text(Some("cellt1a"), "task|one"),
                fixtures::table_cell_text(Some("cellt1b"), "Todo"),
                fixtures::table_cell_text(Some("cellt1c"), "a|b\nnext"),
            ]),
            fixtures::table_row(Some("rowtask2"), &[
                fixtures::table_cell_text(Some("cellt2a"), "task two"),
                fixtures::table_cell_text(Some("cellt2b"), "Done"),
                fixtures::table_cell_text(Some("cellt2c"), "2026-11-15"),
            ]),
        ]
    });
    let ragged = json!({
        "type": "table",
        "attrs": { "id": "tblraggd01" },
        "content": [
            fixtures::table_row(Some("rowhdrg0"), &[
                fixtures::table_header(Some("colrg1"), "A"),
                fixtures::table_header(Some("colrg2"), "B"),
            ]),
            fixtures::table_row(Some("rowrg1"), &[
                fixtures::table_cell_text(Some("cellrg1"), "one"),
            ]),
            fixtures::table_row(Some("rowrg2"), &[
                fixtures::table_cell_text(Some("cellrg2"), "x"),
                fixtures::table_cell_text(Some("cellrg3"), "y"),
                fixtures::table_cell_text(Some("cellrg4"), "extra"),
            ]),
        ]
    });
    let duplicated = json!({
        "type": "table",
        "attrs": { "id": "tbldupes01" },
        "content": [
            fixtures::table_row(Some("rowhdup0"), &[
                fixtures::table_header(Some("coldp1"), "Same"),
                fixtures::table_header(Some("coldp2"), "Other"),
                fixtures::table_header(Some("coldp3"), "Same"),
            ]),
            fixtures::table_row(Some("rowdp1"), &[
                fixtures::table_cell_text(Some("celldp1"), "first"),
                fixtures::table_cell_text(Some("celldp2"), "mid"),
                fixtures::table_cell_text(Some("celldp3"), "second"),
            ]),
        ]
    });
    let spans = json!({
        "type": "table",
        "attrs": { "id": "tblspans01" },
        "content": [
            fixtures::table_row(Some("rowhspn0"), &[
                fixtures::table_header(Some("colsp1"), "A"),
                fixtures::table_header(Some("colsp2"), "B"),
            ]),
            fixtures::table_row(Some("rowsp1"), &[
                fixtures::table_cell("tableCell", Some("cellsp1"), "wide", 2, 1),
                fixtures::table_cell("tableCell", Some("cellsp2"), "tall", 1, 2),
            ]),
        ]
    });
    let nested = json!({
        "type": "table",
        "attrs": { "id": "tblnestd01" },
        "content": [
            fixtures::table_row(Some("rowhnst0"), &[
                fixtures::table_header(Some("colnst1"), "A"),
            ]),
            fixtures::table_row(Some("rownst1"), &[
                fixtures::table_cell("tableCell", Some("cellnst1"), "outer", 1, 1),
            ]),
        ]
    });
    let dup_second = json!({
        "type": "table",
        "attrs": { "id": "tbldupes02" },
        "content": [
            fixtures::table_row(Some("rowhamb0"), &[
                fixtures::table_header(Some("colamb1"), "A"),
            ]),
            fixtures::table_row(Some("rowamb1"), &[
                fixtures::table_cell_text(Some("cellamb1"), "second dup table"),
            ]),
        ]
    });
    let weird = json!({
        "type": "table",
        "attrs": { "id": "tblweird01" },
        "content": [
            fixtures::table_row(Some("rowhwrd0"), &[
                fixtures::table_header(Some("colwrd1"), "A"),
            ]),
            fixtures::table_row(Some("rowwrd1"), &[json_cell_with_unknown(Some("cellwrd1"))]),
        ]
    });
    vec![
        fixtures::heading(2, "Board"),
        board,
        fixtures::heading(2, "Ragged"),
        ragged,
        fixtures::heading(2, "Dup"),
        duplicated,
        fixtures::heading(2, "Spans"),
        spans,
        fixtures::heading(2, "Nested"),
        nested,
        fixtures::heading(2, "Dup"),
        dup_second,
        fixtures::heading(2, "Weird"),
        weird,
    ]
}

/// A tableCell holding an unknown node type (withholds the cell text).
fn json_cell_with_unknown(id: Option<&str>) -> Value {
    let mut cell = json!({
        "type": "tableCell",
        "attrs": { "colspan": 1, "rowspan": 1 },
        "content": [
            { "type": "paragraph", "content": [{ "type": "text", "text": "before " }] },
            { "type": "customWidget", "content": [] }
        ]
    });
    if let Some(id) = id {
        cell["attrs"]["id"] = json!(id);
    }
    cell
}

/// Replace the NESTED table's first data cell with one that contains a real
/// nested table (the builder API keeps cells single-paragraph).
fn with_nested_cell(mut blocks: Vec<Value>) -> Vec<Value> {
    let inner = json!({
        "type": "table",
        "content": [
            { "type": "tableRow", "content": [
                { "type": "tableCell", "attrs": { "colspan": 1, "rowspan": 1 },
                  "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": "inner" }] }] }
            ]}
        ]
    });
    blocks[9]["content"][1]["content"][0]["content"]
        .as_array_mut()
        .expect("cell content")
        .push(inner);
    blocks
}

/// One container with every table shape the contract needs, plus `.note`
/// flavors.
struct Corpus {
    dir: TempDir,
}

impl Corpus {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path();
        let note_id = fixtures::ulid_seq(1);
        fixtures::write_um(
            &path.join("tables.um"),
            &[fixtures::FixtureNote::plain_page(
                &note_id,
                "Tables",
                &with_nested_cell(corpus_blocks()),
            )],
        )
        .expect("tables fixture");

        let legacy = fixtures::plain_doc(&[
            fixtures::heading(2, "Solo"),
            fixtures::table(&[
                fixtures::table_row(Some("lrow0"), &[fixtures::table_header(Some("lcol0"), "A")]),
                fixtures::table_row(
                    Some("lrow1"),
                    &[fixtures::table_cell_text(Some("lcell1"), "legacy")],
                ),
            ]),
        ]);
        fixtures::write_legacy_note(&path.join("legacy.note"), &legacy).expect("legacy fixture");

        let envelope = fixtures::plain_doc(&[
            fixtures::heading(2, "Solo"),
            fixtures::table(&[
                fixtures::table_row(Some("erow0"), &[fixtures::table_header(Some("ecol0"), "A")]),
                fixtures::table_row(
                    Some("erow1"),
                    &[fixtures::table_cell_text(Some("ecell1"), "envelope")],
                ),
            ]),
        ]);
        fixtures::write_envelope_note(&path.join("envelope.note"), &envelope)
            .expect("envelope fixture");

        fixtures::empty_container(&path.join("empty.um")).expect("empty fixture");
        Self { dir }
    }

    fn path(&self, name: &str) -> String {
        self.dir.path().join(name).to_string_lossy().into_owned()
    }

    fn note_id(&self) -> String {
        fixtures::ulid_seq(1)
    }
}

// ---------------------------------------------------------------------------
// addressing
// ---------------------------------------------------------------------------

#[test]
fn index_zero_returns_the_board_table() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let value = run_json(&["table", "get", &file, "--note", &note, "--index", "0"]);
    assert_eq!(value["api_version"], 1);
    // CONTRACT (§5.2): JSON paths use `/` regardless of the OS — the
    // expectation is the normalized form, never the OS-native argument
    // string (a Windows temp path carries `\` and would never match).
    assert_eq!(value["path"], file.replace('\\', "/"));
    assert_eq!(value["note_id"], note);
    assert!(value["note_rev"].as_str().unwrap().starts_with("sha256:"));
    let table = &value["table"];
    assert_eq!(table["pointer"], "/content/1");
    assert_eq!(table["id"], "tblboard01");
    assert_eq!(table["index"], 0);
    assert_eq!(table["table_count"], 7);
    assert_eq!(table["heading"], "Board");
    assert_eq!(
        table["columns"],
        json!([
            {"name": "Name", "id": "colname1", "pointer": "/content/1/content/0/content/0"},
            {"name": "State", "id": "colstat1", "pointer": "/content/1/content/0/content/1"},
            {"name": "Date", "id": "coldate1", "pointer": "/content/1/content/0/content/2"},
        ])
    );
    assert_eq!(table["rows_source"], 2);
    assert_eq!(table["rows_matched"], 2);
    let rows = table["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], "rowtask1");
    assert_eq!(rows[0]["pointer"], "/content/1/content/1");
    assert_eq!(rows[0]["cells"][0]["id"], "cellt1a");
    assert_eq!(
        rows[0]["cells"][0]["pointer"],
        "/content/1/content/1/content/0"
    );
    assert_eq!(rows[0]["cells"][0]["text"], "task|one");
    assert_eq!(rows[0]["cells"][2]["text"], "a|b\nnext");
    // values by column name, names unique
    assert_eq!(rows[0]["values"]["Name"], "task|one");
    assert_eq!(rows[0]["values"]["State"], "Todo");
    assert_eq!(rows[0]["values"]["Date"], "a|b\nnext");
    assert!(value["complete"].as_bool().unwrap());
    assert!(!value["truncated"].as_bool().unwrap());
    assert!(value["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn index_addresses_each_table_in_document_order() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    for (index, pointer) in [
        (0, "/content/1"),
        (1, "/content/3"),
        (2, "/content/5"),
        (3, "/content/7"),
        (4, "/content/9"),
        (5, "/content/11"),
        (6, "/content/13"),
    ] {
        let index_text = index.to_string();
        let value = run_json(&[
            "table",
            "get",
            &file,
            "--note",
            &note,
            "--index",
            &index_text,
        ]);
        assert_eq!(value["table"]["pointer"], pointer, "index {index}");
    }
}

#[test]
fn index_out_of_range_is_block_not_found() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let (code, err) = fail_json(&["table", "get", &file, "--note", &note, "--index", "7"]);
    assert_eq!(code, 3);
    assert_eq!(err["error"]["code"], "BLOCK_NOT_FOUND");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap()
            .contains("holds 7 tables")
    );
}

#[test]
fn id_addresses_the_table_and_warns_without_marker() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let value = run_json(&["table", "get", &file, "--note", &note, "--id", "tblboard01"]);
    // the .um fixture carries no features.block_ids marker: reading by ID
    // works on presence, the answer says the IDs are not guaranteed
    let codes: Vec<&str> = value["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(
        codes.contains(&"BLOCK_IDS_NOT_GUARANTEED"),
        "codes: {codes:?}"
    );
    assert_eq!(value["table"]["pointer"], "/content/1");
}

#[test]
fn id_miss_is_block_not_found() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let (code, err) = fail_json(&["table", "get", &file, "--note", &note, "--id", "nope12345"]);
    assert_eq!(code, 3);
    assert_eq!(err["error"]["code"], "BLOCK_NOT_FOUND");
}

#[test]
fn plain_read_of_envelope_note_has_no_marker_warning() {
    let corpus = Corpus::new();
    let file = corpus.path("envelope.note");
    let value = run_json(&["table", "get", &file, "--index", "0"]);
    let codes: Vec<&str> = value["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(
        !codes.contains(&"BLOCK_IDS_NOT_GUARANTEED"),
        "codes: {codes:?}"
    );
    assert_eq!(value["note_id"], Value::Null);
    assert_eq!(value["table"]["heading"], "Solo");
}

#[test]
fn after_heading_resolves_the_nearest_preceding_heading() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let value = run_json(&[
        "table",
        "get",
        &file,
        "--note",
        &note,
        "--after-heading",
        "Ragged",
    ]);
    assert_eq!(value["table"]["pointer"], "/content/3");
    assert_eq!(value["table"]["heading"], "Ragged");
}

#[test]
fn after_heading_miss_is_block_not_found() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let (code, err) = fail_json(&[
        "table",
        "get",
        &file,
        "--note",
        &note,
        "--after-heading",
        "Ragged ",
    ]);
    assert_eq!(code, 3, "exact match, no trimming");
    assert_eq!(err["error"]["code"], "BLOCK_NOT_FOUND");
}

#[test]
fn after_heading_ambiguity_lists_pointers() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    // two "Dup" headings, each followed by its own table
    let (code, err) = fail_json(&[
        "table",
        "get",
        &file,
        "--note",
        &note,
        "--after-heading",
        "Dup",
    ]);
    assert_eq!(code, 3);
    assert_eq!(err["error"]["code"], "BLOCK_NOT_FOUND");
    assert_eq!(err["error"]["details"]["ambiguous"], true);
    let pointers = err["error"]["details"]["pointers"].as_array().unwrap();
    assert_eq!(
        pointers,
        &["/content/5", "/content/11"]
            .iter()
            .map(|s| json!(s))
            .collect::<Vec<_>>()
    );
}

#[test]
fn after_heading_unique_result_succeeds() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let value = run_json(&[
        "table",
        "get",
        &file,
        "--note",
        &note,
        "--after-heading",
        "Nested",
    ]);
    assert_eq!(value["table"]["pointer"], "/content/9");
}

#[test]
fn addressing_options_are_mutually_exclusive_and_required() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    for args in [
        vec!["table", "get", &file, "--note", &note],
        vec![
            "table",
            "get",
            &file,
            "--note",
            &note,
            "--index",
            "0",
            "--id",
            "tblboard01",
        ],
        vec![
            "table",
            "get",
            &file,
            "--note",
            &note,
            "--index",
            "0",
            "--after-heading",
            "Board",
        ],
    ] {
        let (code, err) = fail_json(&args);
        assert_eq!(code, 2, "args {args:?}");
        assert_eq!(err["error"]["code"], "USAGE");
    }
}

#[test]
fn note_flag_follows_the_block_get_conventions() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    // .um without --note
    let (code, err) = fail_json(&["table", "get", &file, "--index", "0"]);
    assert_eq!(code, 2);
    assert_eq!(err["error"]["code"], "USAGE");
    // .note with --note
    let (code, err) = fail_json(&[
        "table",
        "get",
        &corpus.path("legacy.note"),
        "--note",
        "x",
        "--index",
        "0",
    ]);
    assert_eq!(code, 2);
    assert_eq!(err["error"]["code"], "USAGE");
    // note title resolution works like everywhere else
    let value = run_json(&["table", "get", &file, "--note", "Tables", "--index", "0"]);
    assert_eq!(value["note_id"], corpus.note_id());
    // missing note
    let (code, err) = fail_json(&[
        "table",
        "get",
        &file,
        "--note",
        "01UNKNOWNNOTE0000000000000",
        "--index",
        "0",
    ]);
    assert_eq!(code, 3);
    assert_eq!(err["error"]["code"], "NOTE_NOT_FOUND");
    // empty container
    let (code, err) = fail_json(&[
        "table",
        "get",
        &corpus.path("empty.um"),
        "--note",
        "x",
        "--index",
        "0",
    ]);
    assert_eq!(code, 3);
    assert_eq!(err["error"]["code"], "NOTE_NOT_FOUND");
}

#[test]
fn unknown_format_is_usage() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let (code, err) = fail_json(&[
        "table", "get", &file, "--note", &note, "--index", "0", "--format", "yaml",
    ]);
    assert_eq!(code, 2);
    assert_eq!(err["error"]["code"], "USAGE");
}

// ---------------------------------------------------------------------------
// --where
// ---------------------------------------------------------------------------

#[test]
fn where_filter_keeps_exact_matches_only() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let value = run_json(&[
        "table",
        "get",
        &file,
        "--note",
        &note,
        "--index",
        "0",
        "--where",
        "State=Todo",
    ]);
    let rows = value["table"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "rowtask1");
    assert_eq!(value["table"]["rows_source"], 2);
    assert_eq!(value["table"]["rows_matched"], 1);
    assert!(value["complete"].as_bool().unwrap());
}

#[test]
fn where_match_is_byte_exact_not_a_substring() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    // "on" is a substring of "Done" but matches nothing byte-for-byte
    let value = run_json(&[
        "table", "get", &file, "--note", &note, "--index", "0", "--where", "State=on",
    ]);
    assert_eq!(value["table"]["rows"].as_array().unwrap().len(), 0);
    assert!(value["complete"].as_bool().unwrap());
    // the value may contain '='
    let value = run_json(&[
        "table", "get", &file, "--note", &note, "--index", "0", "--where", "Name=a=b",
    ]);
    assert_eq!(value["table"]["rows"].as_array().unwrap().len(), 0);
}

#[test]
fn where_miss_returns_zero_rows_and_stays_complete() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let value = run_json(&[
        "table",
        "get",
        &file,
        "--note",
        &note,
        "--index",
        "0",
        "--where",
        "State=Nope",
    ]);
    assert_eq!(value["table"]["rows"].as_array().unwrap().len(), 0);
    assert_eq!(value["table"]["rows_matched"], 0);
    assert!(value["complete"].as_bool().unwrap());
}

#[test]
fn where_unknown_column_is_an_error_with_column_list() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let (code, err) = fail_json(&[
        "table", "get", &file, "--note", &note, "--index", "0", "--where", "Nope=x",
    ]);
    assert_eq!(code, 3);
    assert_eq!(err["error"]["code"], "BLOCK_NOT_FOUND");
    assert!(
        err["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("Name, State, Date")
    );
}

#[test]
fn where_on_duplicated_columns_refuses() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    // the DUP table: Same | Other | Same — refused even when the queried
    // column name is unique ("Other")
    let (code, err) = fail_json(&[
        "table",
        "get",
        &file,
        "--note",
        &note,
        "--index",
        "2",
        "--where",
        "Other=mid",
    ]);
    assert_eq!(code, 3);
    assert_eq!(err["error"]["code"], "BLOCK_NOT_FOUND");
    assert_eq!(err["error"]["details"]["ambiguous"], true);
    assert_eq!(err["error"]["details"]["duplicates"], json!(["Same"]));
}

#[test]
fn where_without_separator_or_column_is_usage() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let (code, err) = fail_json(&[
        "table",
        "get",
        &file,
        "--note",
        &note,
        "--index",
        "0",
        "--where",
        "NoSeparator",
    ]);
    assert_eq!(code, 2);
    assert_eq!(err["error"]["code"], "USAGE");
    let (code, err) = fail_json(&[
        "table", "get", &file, "--note", &note, "--index", "0", "--where", "=x",
    ]);
    assert_eq!(code, 2);
    assert_eq!(err["error"]["code"], "USAGE");
}

// ---------------------------------------------------------------------------
// diagnostics: ragged, duplicates, spans, nested, unknown
// ---------------------------------------------------------------------------

#[test]
fn ragged_rows_are_diagnosed_and_kept_positional() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let value = run_json(&[
        "table",
        "get",
        &file,
        "--note",
        &note,
        "--after-heading",
        "Ragged",
    ]);
    let codes: Vec<&str> = value["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert_eq!(
        codes.iter().filter(|c| **c == "TABLE_ROW_RAGGED").count(),
        2,
        "both deviating rows are named: {codes:?}"
    );
    // no cell is dropped: row 1 has one cell, row 2 has three
    let rows = value["table"]["rows"].as_array().unwrap();
    assert_eq!(rows[0]["cells"].as_array().unwrap().len(), 1);
    assert_eq!(rows[1]["cells"].as_array().unwrap().len(), 3);
    assert!(value["complete"].as_bool().unwrap());
}

#[test]
fn duplicate_headers_warn_and_values_collapse_to_null() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let value = run_json(&["table", "get", &file, "--note", &note, "--index", "2"]);
    let codes: Vec<&str> = value["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(
        codes.contains(&"TABLE_HEADER_DUPLICATE"),
        "codes: {codes:?}"
    );
    let rows = value["table"]["rows"].as_array().unwrap();
    assert_eq!(rows[0]["values"], Value::Null, "ambiguous map is null");
    // positional cells stay complete
    assert_eq!(rows[0]["cells"].as_array().unwrap().len(), 3);
    assert_eq!(rows[0]["cells"][2]["text"], "second");
}

#[test]
fn duplicate_headers_warn_without_refusing_plain_read() {
    // Without --where the duplicate is a warning, not an error (exit 0).
    let corpus = Corpus::new();
    let output = insum()
        .args([
            "table",
            "get",
            &corpus.path("tables.um"),
            "--note",
            &corpus.note_id(),
            "--index",
            "2",
        ])
        .output()
        .expect("insum runs");
    assert!(output.status.success());
}

#[test]
fn spans_are_diagnosed_with_cell_pointers() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let value = run_json(&[
        "table",
        "get",
        &file,
        "--note",
        &note,
        "--after-heading",
        "Spans",
    ]);
    let warnings = value["warnings"].as_array().unwrap();
    let span = warnings
        .iter()
        .find(|w| w["code"] == "TABLE_SPAN")
        .expect("TABLE_SPAN warning");
    let cells = span["details"]["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 2);
    assert_eq!(cells[0]["colspan"], 2);
    assert_eq!(cells[1]["rowspan"], 2);
}

#[test]
fn nested_table_is_diagnosed_and_excluded_from_cell_text() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let value = run_json(&[
        "table",
        "get",
        &file,
        "--note",
        &note,
        "--after-heading",
        "Nested",
    ]);
    let warnings = value["warnings"].as_array().unwrap();
    let nested = warnings
        .iter()
        .find(|w| w["code"] == "TABLE_NESTED")
        .expect("TABLE_NESTED warning");
    assert_eq!(nested["details"]["pointers"].as_array().unwrap().len(), 1);
    let rows = value["table"]["rows"].as_array().unwrap();
    assert_eq!(rows[0]["cells"][0]["text"], "outer");
    // the nested table is not a separate entry of the note
    assert_eq!(value["table"]["table_count"], 7);
}

#[test]
fn unknown_node_type_withholds_text_and_warns() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let value = run_json(&[
        "table",
        "get",
        &file,
        "--note",
        &note,
        "--after-heading",
        "Weird",
    ]);
    let codes: Vec<&str> = value["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"UNKNOWN_NODE_TYPE"), "codes: {codes:?}");
    let rows = value["table"]["rows"].as_array().unwrap();
    assert_eq!(rows[0]["cells"][0]["text"], Value::Null);
    assert_eq!(rows[0]["values"]["A"], Value::Null);
}

#[test]
fn withheld_heading_context_warns_instead_of_silence() {
    // A heading whose inline text is withheld (unknown node) leaves the
    // following table with `heading: null` — the response must say WHY,
    // the silence must not read as "no heading above".
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("broken-heading.note");
    let doc = fixtures::plain_doc(&[
        json!({
            "type": "heading",
            "attrs": {"level": 2},
            "content": [
                { "type": "text", "text": "Broken " },
                { "type": "customWidget", "content": [] }
            ]
        }),
        fixtures::table(&[
            fixtures::table_row(Some("hrow0"), &[fixtures::table_header(Some("hcol0"), "A")]),
            fixtures::table_row(
                Some("hrow1"),
                &[fixtures::table_cell_text(Some("hcell1"), "v")],
            ),
        ]),
    ]);
    fixtures::write_legacy_note(&path, &doc).expect("fixture");
    let file = path.to_string_lossy().into_owned();
    let value = run_json(&["table", "get", &file, "--index", "0"]);
    assert_eq!(value["table"]["heading"], Value::Null);
    let warnings = value["warnings"].as_array().unwrap();
    let heading_warning = warnings.first().expect("the heading warning is first");
    assert_eq!(heading_warning["code"], "UNKNOWN_NODE_TYPE");
    assert!(
        heading_warning["message"]
            .as_str()
            .unwrap()
            .contains("heading text is withheld"),
        "message: {}",
        heading_warning["message"]
    );
    // the same note read after an addressing miss still fails BLOCK_NOT_FOUND
    let (code, err) = fail_json(&["table", "get", &file, "--after-heading", "Broken"]);
    assert_eq!(code, 3);
    assert_eq!(err["error"]["code"], "BLOCK_NOT_FOUND");
}

// ---------------------------------------------------------------------------
// markdown projection
// ---------------------------------------------------------------------------

#[test]
fn markdown_is_gfm_with_escaped_pipes_and_no_anchors() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let output = insum()
        .args([
            "table", "get", &file, "--note", &note, "--index", "0", "--format", "markdown",
        ])
        .output()
        .expect("insum runs");
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.starts_with("| Name | State | Date |\n| --- | --- | --- |\n"),
        "GFM header and separator first: {text:?}"
    );
    assert!(text.contains("| task\\|one | Todo | a\\|b<br>next |"));
    assert!(text.ends_with('\n'));
    assert!(!text.contains("um:pointer"), "no anchors: {text:?}");
    assert!(
        !text.contains("um:warning"),
        "clean table carries no diagnostics: {text:?}"
    );
}

#[test]
fn markdown_pipe_roundtrip_value_echo() {
    // Escaping must not distort the value: unescaping the GFM cell text
    // (\\| -> |, <br> -> \n) reproduces the JSON cell text byte-for-byte.
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let json = run_json(&["table", "get", &file, "--note", &note, "--index", "0"]);
    let rows = json["table"]["rows"].as_array().unwrap();
    let expected_first = rows[0]["cells"][0]["text"].as_str().unwrap();
    let expected_third = rows[0]["cells"][2]["text"].as_str().unwrap();
    let output = insum()
        .args([
            "table", "get", &file, "--note", &note, "--index", "0", "--format", "markdown",
        ])
        .output()
        .expect("insum runs");
    let text = String::from_utf8(output.stdout).unwrap();
    let data_line = text
        .lines()
        .find(|line| line.starts_with("| task"))
        .expect("data row line");
    let body = data_line
        .strip_prefix("| ")
        .and_then(|line| line.strip_suffix(" |"))
        .expect("GFM row shape");
    let cells: Vec<String> = body.split(" | ").map(str::to_owned).collect();
    assert_eq!(cells.len(), 3);
    assert_eq!(cells[0].replace("\\|", "|"), expected_first);
    assert_eq!(
        cells[2].replace("\\|", "|").replace("<br>", "\n"),
        expected_third
    );
}

#[test]
fn markdown_pads_ragged_rows_and_warns() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let output = insum()
        .args([
            "table",
            "get",
            &file,
            "--note",
            &note,
            "--after-heading",
            "Ragged",
            "--format",
            "markdown",
        ])
        .output()
        .expect("insum runs");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("| one |  |\n"), "short row padded: {text:?}");
    assert!(
        text.contains("| x | y | extra |\n"),
        "extra cells kept: {text:?}"
    );
    assert!(text.contains("um:warning code=\"TABLE_ROW_RAGGED\""));
}

#[test]
fn markdown_empty_projection_for_column_less_table() {
    // A table whose first row has no cells has no columns: markdown cannot
    // represent it, the diagnostics are emitted instead.
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("headerless.note");
    let doc = fixtures::plain_doc(&[fixtures::table(&[
        fixtures::table_row(Some("r0"), &[]),
        fixtures::table_row(Some("r1"), &[fixtures::table_cell_text(Some("c1"), "v")]),
    ])]);
    fixtures::write_legacy_note(&path, &doc).expect("fixture");
    let output = insum()
        .args([
            "table",
            "get",
            &path.to_string_lossy(),
            "--index",
            "0",
            "--format",
            "markdown",
        ])
        .output()
        .expect("insum runs");
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains('|'), "no GFM pipe rows: {text:?}");
    assert!(text.contains("um:warning"));
}

// ---------------------------------------------------------------------------
// budget and determinism
// ---------------------------------------------------------------------------

#[test]
fn json_budget_truncates_whole_rows_explicitly() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let value = run_json(&[
        "table",
        "get",
        &file,
        "--note",
        &note,
        "--index",
        "0",
        "--max-bytes",
        "700",
    ]);
    assert!(!value["complete"].as_bool().unwrap());
    assert!(value["truncated"].as_bool().unwrap());
    let rows = value["table"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "the first data row survives");
    assert_eq!(rows[0]["id"], "rowtask1", "the prefix is in order");
    let codes: Vec<&str> = value["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"OUTPUT_BUDGET"), "codes: {codes:?}");
}

#[test]
fn json_budget_never_breaks_with_tiny_budget() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    // even a budget smaller than the skeleton answers honestly
    let value = run_json(&[
        "table",
        "get",
        &file,
        "--note",
        &note,
        "--index",
        "0",
        "--max-bytes",
        "80",
    ]);
    assert!(!value["complete"].as_bool().unwrap());
    assert_eq!(value["table"]["rows"].as_array().unwrap().len(), 1);
}

#[test]
fn markdown_budget_truncates_with_marker() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let output = insum()
        .args([
            "table",
            "get",
            &file,
            "--note",
            &note,
            "--index",
            "0",
            "--format",
            "markdown",
            "--max-bytes",
            "160",
        ])
        .output()
        .expect("insum runs");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("| task\\|one"), "the first row survives");
    assert!(
        text.contains("um:warning code=\"OUTPUT_BUDGET\""),
        "truncation marker present: {text:?}"
    );
    assert!(text.contains("1/2 rows"));
}

#[test]
fn repeated_runs_are_byte_identical_and_read_only() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let before = fs::read(&file).expect("read fixture");
    let args = [
        "table",
        "get",
        &file,
        "--note",
        &corpus.note_id(),
        "--index",
        "0",
    ];
    let first = insum().args(args).output().expect("insum runs");
    let second = insum().args(args).output().expect("insum runs");
    assert_eq!(first.stdout, second.stdout, "determinism");
    let after = fs::read(&file).expect("read fixture");
    assert_eq!(before, after, "the file is never modified");
}

#[test]
fn pretty_flag_indents_json() {
    let corpus = Corpus::new();
    let file = corpus.path("tables.um");
    let note = corpus.note_id();
    let compact = insum()
        .args(["table", "get", &file, "--note", &note, "--index", "0"])
        .output()
        .expect("insum runs");
    let pretty = insum()
        .args([
            "table", "get", &file, "--note", &note, "--index", "0", "--pretty",
        ])
        .output()
        .expect("insum runs");
    let a: Value = serde_json::from_slice(&compact.stdout).unwrap();
    let b: Value = serde_json::from_slice(&pretty.stdout).unwrap();
    assert_eq!(a, b);
    assert!(String::from_utf8_lossy(&pretty.stdout).contains('\n'));
}
