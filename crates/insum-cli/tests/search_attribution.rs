//! Search attribution invariant — cli-feedback «search-note-attribution-flake».
//!
//! Invariant: every match resolves `insum block get` by its own `note_id`
//! and `pointer` on the same file, and the attributed note is the manifest
//! note whose payload produced the text. The audit found the attribution
//! path structurally sound (per-view pairing in read_notes, view-local
//! traverse, no shared maps, single-threaded); these tests keep it pinned:
//! repeated runs over a multi-note corpus with heavy text sharing, plus the
//! one deterministic data class that CAN break the invariant (a manifest
//! whose descriptor path points at foreign payload) is pinned as
//! manifest-faithful attribution.

use assert_cmd::Command;
use serde_json::Value;
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

/// A container where many notes share heavily overlapping texts (the shape
/// the reported flake involved: same heading text in several notes) plus a
/// unique marker per note.
fn shared_text_corpus(dir: &std::path::Path, notes: usize, blocks: usize) -> String {
    let mut fixture_notes = Vec::new();
    for note_index in 0..notes {
        let mut nodes = Vec::new();
        // every note carries the SAME shared heading — attribution pressure
        nodes.push(fixtures::heading(
            2,
            "4.6 Метаданные схемы и feature block_ids",
        ));
        for block_index in 0..blocks {
            nodes.push(fixtures::paragraph(&[fixtures::text(&format!(
                "note-{note_index} block-{block_index} shared-body-text"
            ))]));
        }
        // and one unique marker per note
        nodes.push(fixtures::paragraph(&[fixtures::text(&format!(
            "unique-marker-{note_index}"
        ))]));
        fixture_notes.push(fixtures::FixtureNote::title_page(
            &fixtures::ulid_seq(note_index as u32 + 1),
            &format!("Note {note_index}"),
            "stress corpus page",
            &nodes,
        ));
    }
    let path = dir.join("shared.um");
    fixtures::write_um(&path, &fixture_notes).expect("stress fixture");
    path.to_string_lossy().into_owned()
}

/// For every match: block get by its own note_id + pointer must resolve the
/// same block (id/type/text) on the same file. block get's response carries
/// no note_title — the id↔title pairing is asserted by the marker test and
/// the foreign-path test below.
fn assert_invariant(file: &str, matches: &[Value]) {
    assert!(!matches.is_empty(), "corpus must produce matches");
    for hit in matches {
        let args = [
            "block",
            "get",
            file,
            "--note",
            hit["note_id"].as_str().unwrap(),
            "--pointer",
            hit["pointer"].as_str().unwrap(),
        ];
        let output = insum().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "invariant broken for {hit}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            value["note_id"], hit["note_id"],
            "block get resolved a different note: {hit}"
        );
        let found = &value["block"];
        assert_eq!(found["id"], hit["block_id"], "block id swap: {hit}");
        assert_eq!(found["node"]["type"], hit["type"], "node type swap: {hit}");
        let text = node_text(&found["node"]);
        assert!(
            text.contains(hit["match"].as_str().unwrap()),
            "resolved text does not contain the match: {hit} vs {text}"
        );
    }
}

/// All inline text of a block node (concatenated `text` children).
fn node_text(node: &Value) -> String {
    let mut out = String::new();
    fn walk(node: &Value, out: &mut String) {
        if let Some(text) = node["text"].as_str() {
            out.push_str(text);
        }
        if let Some(children) = node["content"].as_array() {
            for child in children {
                walk(child, out);
            }
        }
    }
    walk(node, &mut out);
    out
}

/// M-12: bind the per-note marker inside the hit text (`note-<i> ...`) to
/// the attributed note id. The stress corpus puts a unique `note-<i>`
/// marker in every shared body block, so a cross-note substitution of two
/// textually identical hits (the invariant above cannot see it: both notes
/// resolve the same id/type/text) shows up as a marker belonging to a
/// different note than the one the hit claims. Hits without the marker
/// (headings, `.note` windows) are skipped; the corpus shape makes the
/// binding deterministic.
fn assert_marker_binding(matches: &[Value]) {
    for hit in matches {
        let text = hit["text"].as_str().unwrap_or("");
        let Some(rest) = text.strip_prefix("note-") else {
            continue;
        };
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        let index: u32 = digits.parse().expect("marker index parses");
        assert_eq!(
            hit["note_id"],
            Value::String(fixtures::ulid_seq(index + 1)),
            "hit attributed across notes: its text carries the note-{index} \
             marker but it claims another note: {hit}"
        );
    }
}

#[test]
fn search_attribution_survives_repeated_runs_and_text_sharing() {
    let dir = TempDir::new().unwrap();
    let file = shared_text_corpus(dir.path(), 8, 6);

    // shared query: every note matches → the widest attribution surface
    let shared = run_json(&["search", "-F", "shared-body-text", &file]);
    assert_eq!(shared["complete"], true);
    assert_invariant(&file, shared["matches"].as_array().unwrap());
    // each hit's text marker must belong to the note the hit claims
    assert_marker_binding(shared["matches"].as_array().unwrap());

    // the heading text all notes share — the exact shape of the report.
    // All eight hits carry IDENTICAL text, so the invariant above (id/
    // type/text equality) cannot detect a cross-note swap: pin the
    // attributed note_id set to the whole corpus — one hit per note.
    let heading = run_json(&["search", "-F", "Метаданные схемы", &file]);
    let heading_matches = heading["matches"].as_array().unwrap();
    assert_eq!(heading_matches.len(), 8);
    assert_invariant(&file, heading_matches);
    let mut hit_ids: Vec<String> = heading_matches
        .iter()
        .map(|m| {
            m["note_id"]
                .as_str()
                .expect(".um hits carry note_id")
                .to_owned()
        })
        .collect();
    hit_ids.sort();
    hit_ids.dedup();
    let mut corpus_ids: Vec<String> = (1..=8).map(fixtures::ulid_seq).collect();
    corpus_ids.sort();
    assert_eq!(
        hit_ids, corpus_ids,
        "the identical-text heading hits must cover every note exactly once"
    );

    // unique marker: exactly one note must be attributed
    for note_index in 0..8 {
        let marker = format!("unique-marker-{note_index}");
        let run = run_json(&["search", "-F", &marker, &file]);
        let matches = run["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1, "marker {marker} matched once");
        assert_eq!(
            matches[0]["note_title"],
            format!("Note {note_index}"),
            "marker {marker} attributed to its own note"
        );
    }

    // repeated runs on the same file: attribution is stable across runs
    for _ in 0..5 {
        let run = run_json(&["search", "-F", "Метаданные схемы", &file]);
        assert_invariant(&file, run["matches"].as_array().unwrap());
    }
}

#[test]
fn search_attribution_with_note_filter_resolves_on_selected_note() {
    let dir = TempDir::new().unwrap();
    let file = shared_text_corpus(dir.path(), 6, 4);
    let note_id = fixtures::ulid_seq(3);
    let run = run_json(&[
        "search",
        "-F",
        "shared-body-text",
        "--note",
        &note_id,
        &file,
    ]);
    let matches = run["matches"].as_array().unwrap();
    assert!(!matches.is_empty());
    for hit in matches {
        assert_eq!(hit["note_id"], Value::String(note_id.clone()));
    }
    assert_invariant(&file, matches);
    assert_marker_binding(matches);
}

#[test]
fn search_attribution_spans_um_and_note_files() {
    let dir = TempDir::new().unwrap();
    let file = shared_text_corpus(dir.path(), 4, 2);
    let doc = fixtures::plain_doc(&[
        fixtures::heading(1, "4.6 Метаданные схемы и feature block_ids"),
        fixtures::paragraph(&[fixtures::text("note-file shared-body-text")]),
    ]);
    let note_path = dir.path().join("single.note");
    fixtures::write_legacy_note(&note_path, &doc).unwrap();

    let run = run_json(&[
        "search",
        "-F",
        "shared-body-text",
        dir.path().to_string_lossy().as_ref(),
    ]);
    let matches = run["matches"].as_array().unwrap();
    // .note matches carry no note_id (single-document files) but must still
    // resolve structurally; .um matches keep the full invariant
    let um_matches: Vec<&Value> = matches
        .iter()
        .filter(|m| m["note_id"].is_string())
        .collect();
    assert_invariant(
        &file,
        &um_matches.iter().map(|v| (*v).clone()).collect::<Vec<_>>(),
    );
    assert!(
        matches
            .iter()
            .any(|m| m["path"].as_str().unwrap().ends_with("single.note")),
        ".note file participates in search"
    );
    // .note matches resolve structurally too: block get without --note on
    // the same file returns the same block
    for hit in matches
        .iter()
        .filter(|m| m["path"].as_str().unwrap().ends_with("single.note"))
    {
        let output = insum()
            .args([
                "block",
                "get",
                &note_path.to_string_lossy(),
                "--pointer",
                hit["pointer"].as_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            ".note match does not resolve: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["block"]["node"]["type"], hit["type"]);
    }
}

/// Manifest data whose descriptor path points at a foreign payload is the
/// deterministic data-level oddity worth pinning: search attributes per the
/// manifest (the only authority for note ids), so the task invariant —
/// block get by the hit's own note_id + pointer — still holds (search and
/// block get resolve the same descriptor path). What such data breaks is
/// the payload-file-name-owns-the-note expectation, nothing else. Pinned so
/// a future swap in search logic cannot hide behind it.
#[test]
fn foreign_descriptor_path_is_attributed_per_manifest() {
    let dir = TempDir::new().unwrap();
    // ULIDs are time-based — generate each id exactly once and reuse it
    let ids: Vec<String> = (1..=2).map(fixtures::ulid_seq).collect();
    let fixture_notes: Vec<fixtures::FixtureNote> = (0..2)
        .map(|note_index| {
            fixtures::FixtureNote::title_page(
                &ids[note_index],
                &format!("Note {note_index}"),
                "desc",
                &[fixtures::paragraph(&[fixtures::text(&format!(
                    "payload-of-note-{note_index}"
                ))])],
            )
        })
        .collect();
    // descriptor of note 0 points at note 1's payload path (foreign data)
    let mut manifest = fixtures::manifest(&fixture_notes);
    manifest["notes"][0]["path"] = Value::String(format!("notes/{}.json", ids[1]));

    use std::io::Write as _;
    let zip_path = dir.path().join("foreign-rebuilt.um");
    let file = fs::File::create(&zip_path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().last_modified_time(zip::DateTime::default());
    writer.start_file("manifest.json", options).unwrap();
    writer.write_all(manifest.to_string().as_bytes()).unwrap();
    for (note_index, id) in ids.iter().enumerate() {
        writer
            .start_file(format!("notes/{id}.json"), options)
            .unwrap();
        let doc = fixtures::title_doc(
            &format!("Note {note_index}"),
            "desc",
            &[fixtures::paragraph(&[fixtures::text(&format!(
                "payload-of-note-{note_index}"
            ))])],
        );
        writer.write_all(doc.to_string().as_bytes()).unwrap();
    }
    writer.finish().unwrap();

    let file = zip_path.to_string_lossy().into_owned();
    let run = run_json(&["search", "-F", "payload-of-note-1", &file]);
    let matches = run["matches"].as_array().unwrap();
    assert!(!matches.is_empty());
    // BOTH descriptors point at that payload, so both must surface — each
    // hit carries its OWN descriptor's id/title pair (no cross-mixing
    // inside a hit, no wholesale swap):
    let expected: &[(String, &str)] = &[(ids[0].clone(), "Note 0"), (ids[1].clone(), "Note 1")];
    for hit in matches {
        let pair = (
            hit["note_id"].as_str().unwrap().to_owned(),
            hit["note_title"].as_str().unwrap(),
        );
        assert!(
            expected
                .iter()
                .any(|(id, title)| *id == pair.0 && *title == pair.1),
            "hit {pair:?} is not a manifest (id, title) pair: {hit}"
        );
    }
    for (id, title) in expected {
        assert!(
            matches
                .iter()
                .any(|hit| hit["note_id"] == Value::String(id.clone())
                    && hit["note_title"] == Value::String((*title).into())),
            "descriptor {id}/{title} missing from matches: {matches:?}"
        );
    }
    // the payload the manifest no longer references is honestly invisible
    let orphan = run_json(&["search", "-F", "payload-of-note-0", &file]);
    assert_eq!(orphan["matches"].as_array().unwrap().len(), 0);
}

use std::fs;
