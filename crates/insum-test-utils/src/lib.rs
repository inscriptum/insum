//! insum-test-utils — generation of `.um` / `.note` fixtures for insum tests.
//!
//! Fixtures follow the UM Document Container Format 1.0 (spec §4.2/§4.6) and
//! the observed on-disk dictionary (`~/insum/.instruments/um-format.md`):
//! a `.um` file is a ZIP with `manifest.json` and `notes/<ULID>.json` entries;
//! a `.note` file is a raw ProseMirror-like `noteDoc` JSON (legacy) or an
//! `inscriptum-note` envelope.
//!
//! Generators are deterministic: entry order, contents and ZIP timestamps are
//! fixed, ULIDs derive from a counter — so two runs on the same input produce
//! byte-identical files (spec §3.11).

use std::fs::File;
use std::io::Write;
use std::path::Path;

use serde_json::{Value, json};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

/// Fixed ZIP entry metadata so fixture bytes are reproducible: the timestamp
/// is pinned explicitly — with the `time` feature enabled, `default()` would
/// stamp the current wall clock and break byte-determinism (spec §3.11).
fn fixture_zip_options() -> SimpleFileOptions {
    SimpleFileOptions::default().last_modified_time(zip::DateTime::default()) // 1980-01-01
}

// ---------------------------------------------------------------------------
// ULID
// ---------------------------------------------------------------------------

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Encode a 128-bit value as a 26-character Crockford-base32 ULID
/// (10 time characters + 16 random characters).
fn encode_ulid(time_ms: u64, random: u128) -> String {
    let mut chars = [0u8; 26];
    let mut n = time_ms & 0xFFFF_FFFF_FFFF; // 48-bit time
    for slot in (0..10).rev() {
        chars[slot] = CROCKFORD[(n % 32) as usize];
        n /= 32;
    }
    let mut r = random & ((1u128 << 80) - 1); // 80-bit randomness
    for slot in (10..26).rev() {
        chars[slot] = CROCKFORD[(r % 32) as usize];
        r /= 32;
    }
    String::from_utf8(chars.to_vec()).expect("Crockford alphabet is ASCII")
}

/// A real-time ULID (for runtime use; tests prefer [`ulid_seq`] for
/// reproducibility).
pub fn ulid_now() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let random =
        u128::from(u64::from(std::process::id())) << 32 | u128::from(nanoseconds_counter());
    encode_ulid(ms, random)
}

fn nanoseconds_counter() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

/// A deterministic ULID: `index` selects distinct values, the time part is
/// fixed. ULIDs do not encode position (spec §4.4) — this is only a stable
/// source of syntactically valid identifiers.
pub fn ulid_seq(index: u32) -> String {
    // Golden-ratio spread over the 80-bit random part: distinct indices give
    // distinct ULIDs without needing entropy.
    let random = (u128::from(index).wrapping_mul(0x9E37_79B9_7F4A_7C15_9E37_79B9_7F4A_7C15))
        & ((1u128 << 80) - 1);
    encode_ulid(1_793_635_200_000, random) // fixed epoch ms, deterministic
}

// ---------------------------------------------------------------------------
// noteDoc builders (dictionary: ~/insum/.instruments/um-format.md)
// ---------------------------------------------------------------------------

/// A `text` inline node.
pub fn text(s: &str) -> Value {
    json!({ "type": "text", "text": s })
}

/// A `paragraph` block with inline content.
pub fn paragraph(content: &[Value]) -> Value {
    json!({ "type": "paragraph", "content": content })
}

/// A `heading` block with a level (1–4 observed on disk).
pub fn heading(level: u8, s: &str) -> Value {
    json!({ "type": "heading", "attrs": { "level": level }, "content": [text(s)] })
}

/// A `bulletList` of `listItem` paragraphs.
pub fn bullet_list(items: &[&str]) -> Value {
    let list_items: Vec<Value> = items
        .iter()
        .map(|item| json!({ "type": "listItem", "content": [paragraph(&[text(item)])] }))
        .collect();
    json!({ "type": "bulletList", "content": list_items })
}

/// A `hljsCodeBlock` with language class and one row of code text.
pub fn code_block(language: &str, code: &str) -> Value {
    json!({
        "type": "hljsCodeBlock",
        "attrs": { "class": format!("language-{language}"), "autocomplete": "off", "spellcheck": "false" },
        "content": [
            {
                "type": "hljsCodeBlockRow",
                "attrs": { "class": "l" },
                "content": [text(code)]
            }
        ]
    })
}

/// A title-page document: mandatory `noteTitle` + `noteSummary` + content
/// blocks (`schema: "title"`, `schemaVersion: 2`).
pub fn title_doc(title: &str, summary: &str, blocks: &[Value]) -> Value {
    let mut content = vec![
        json!({ "type": "noteTitle", "content": [text(title)] }),
        json!({
            "type": "noteSummary",
            "attrs": { "data-placeholder": "Add a short summary", "data-label": "Summary", "role": "definition" },
            "content": [text(summary)]
        }),
    ];
    content.extend(blocks.iter().cloned());
    json!({ "type": "noteDoc", "content": content })
}

/// A plain-page document: content blocks without a mandatory title
/// (`schema: "plain"`); the title of the page is its first line.
pub fn plain_doc(blocks: &[Value]) -> Value {
    json!({ "type": "noteDoc", "content": blocks })
}

// ---------------------------------------------------------------------------
// .um container writer
// ---------------------------------------------------------------------------

/// One note page of a fixture container.
pub struct FixtureNote {
    /// ULID, also used for the `notes/<id>.json` entry name.
    pub id: String,
    /// Manifest `title` (mirror of the document title).
    pub title: String,
    /// `"title"` for the page with a mandatory noteTitle (order 0),
    /// `"plain"` otherwise.
    pub schema: &'static str,
    /// noteDoc JSON payload.
    pub doc: Value,
}

impl FixtureNote {
    pub fn title_page(id: &str, title: &str, summary: &str, blocks: &[Value]) -> Self {
        Self {
            id: id.to_owned(),
            title: title.to_owned(),
            schema: "title",
            doc: title_doc(title, summary, blocks),
        }
    }

    pub fn plain_page(id: &str, title: &str, blocks: &[Value]) -> Self {
        Self {
            id: id.to_owned(),
            title: title.to_owned(),
            schema: "plain",
            doc: plain_doc(blocks),
        }
    }
}

/// Manifest JSON for the given notes, in order (spec §4.6 / um-format.md).
pub fn manifest(notes: &[FixtureNote]) -> Value {
    let entries: Vec<Value> = notes
        .iter()
        .enumerate()
        .map(|(order, note)| {
            let mut entry = json!({
                "id": note.id,
                "path": format!("notes/{}.json", note.id),
                "order": order,
                "title": note.title,
                "schema": note.schema,
                "schemaVersion": 2,
            });
            if order == 0 {
                entry["expanded"] = json!(true);
            }
            entry
        })
        .collect();
    json!({
        "format": "um",
        "version": 1,
        "type": "notepad",
        "notes": entries,
        "assets": [],
    })
}

/// Write a `.um` container: `manifest.json` + `notes/<ULID>.json` entries.
/// Entry names and ZIP metadata are deterministic.
pub fn write_um(path: &Path, notes: &[FixtureNote]) -> std::io::Result<()> {
    let file = File::create(path)?;
    let mut writer = ZipWriter::new(file);
    writer.start_file("manifest.json", fixture_zip_options())?;
    writer.write_all(manifest(notes).to_string().as_bytes())?;
    for note in notes {
        writer.start_file(format!("notes/{}.json", note.id), fixture_zip_options())?;
        writer.write_all(note.doc.to_string().as_bytes())?;
    }
    writer.finish()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// .note writers
// ---------------------------------------------------------------------------

/// Write a legacy `.note`: the file equals the raw `noteDoc` JSON
/// (no envelope, no block IDs — read API supports it as-is, spec §6.1).
pub fn write_legacy_note(path: &Path, doc: &Value) -> std::io::Result<()> {
    std::fs::write(path, doc.to_string())
}

/// Write a versioned `.note` envelope (spec §4.6). The write of envelopes is
/// an этап C concern; read API must already read them.
pub fn write_envelope_note(path: &Path, doc: &Value) -> std::io::Result<()> {
    let envelope = json!({
        "format": "inscriptum-note",
        "version": 1,
        "features": { "block_ids": 1 },
        "schema": {
            "id": "inscriptum",
            "schema_version": 1,
            "schema_hash": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        },
        "document": doc,
    });
    std::fs::write(path, envelope.to_string())
}

// ---------------------------------------------------------------------------
// Ready-made fixture sets (acceptance §15.1 №1–2)
// ---------------------------------------------------------------------------

/// The minimal container: manifest plus one small title page.
pub fn minimal_container(path: &Path) -> std::io::Result<()> {
    let notes = [FixtureNote::title_page(
        &ulid_seq(1),
        "Minimal",
        "Smallest valid container",
        &[paragraph(&[text("One paragraph.")])],
    )];
    write_um(path, &notes)
}

/// The empty container: manifest with zero notes.
pub fn empty_container(path: &Path) -> std::io::Result<()> {
    write_um(path, &[])
}

/// A regular container: a title page plus plain pages with the usual block
/// vocabulary (headings, lists, code, quotes).
pub fn regular_container(path: &Path) -> std::io::Result<()> {
    let notes = [
        FixtureNote::title_page(
            &ulid_seq(1),
            "Развёртывание",
            "Как развернуть сервис",
            &[
                heading(2, "Развёртывание"),
                bullet_list(&["Установить зависимости", "Запустить проверку"]),
            ],
        ),
        FixtureNote::plain_page(
            &ulid_seq(2),
            "Инструкция по deployment приложения",
            &[
                paragraph(&[text("Инструкция по deployment приложения")]),
                code_block("bash", "cargo build --release"),
                paragraph(&[text("Deployment завершён.")]),
            ],
        ),
    ];
    write_um(path, &notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn read_zip_entry(path: &Path, name: &str) -> String {
        let file = File::open(path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut entry = archive.by_name(name).unwrap();
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut entry, &mut buf).unwrap();
        buf
    }

    #[test]
    fn ulids_are_valid_crockford() {
        for index in 0..1000u32 {
            let ulid = ulid_seq(index);
            assert_eq!(ulid.len(), 26);
            assert!(
                ulid.bytes().all(|b| CROCKFORD.contains(&b)),
                "bad char in {ulid}"
            );
        }
        // distinct indices → distinct ULIDs
        assert_ne!(ulid_seq(1), ulid_seq(2));
    }

    #[test]
    fn minimal_container_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("minimal.um");
        minimal_container(&path).unwrap();
        let manifest_json = read_zip_entry(&path, "manifest.json");
        let manifest: Value = serde_json::from_str(&manifest_json).unwrap();
        assert_eq!(manifest["format"], "um");
        assert_eq!(manifest["version"], 1);
        assert_eq!(manifest["type"], "notepad");
        assert_eq!(manifest["notes"].as_array().unwrap().len(), 1);
        assert_eq!(manifest["notes"][0]["schema"], "title");
        // payload entry exists and parses
        let note_id = manifest["notes"][0]["id"].as_str().unwrap().to_owned();
        let doc: Value =
            serde_json::from_str(&read_zip_entry(&path, &format!("notes/{note_id}.json"))).unwrap();
        assert_eq!(doc["type"], "noteDoc");
    }

    #[test]
    fn containers_are_deterministic() {
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("a.um");
        let b = dir.path().join("b.um");
        regular_container(&a).unwrap();
        regular_container(&b).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    }

    /// The DOS timestamp of every entry is the pinned 1980-01-01 default —
    /// this is what keeps fixtures byte-identical across processes and days
    /// (same-process comparison alone would hide a wall-clock timestamp).
    #[test]
    fn zip_entry_timestamps_are_pinned() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pinned.um");
        minimal_container(&path).unwrap();
        let mut archive = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        for name in ["manifest.json", &format!("notes/{}.json", ulid_seq(1))] {
            let entry = archive.by_name(name).unwrap();
            assert_eq!(
                entry.last_modified(),
                Some(zip::DateTime::default()),
                "entry {name}"
            );
        }
    }

    #[test]
    fn note_writers_produce_legacy_and_envelope() {
        let dir = TempDir::new().unwrap();
        let doc = plain_doc(&[paragraph(&[text("hello")])]);
        let legacy = dir.path().join("legacy.note");
        write_legacy_note(&legacy, &doc).unwrap();
        let legacy_json: Value =
            serde_json::from_str(&std::fs::read_to_string(&legacy).unwrap()).unwrap();
        assert_eq!(legacy_json["type"], "noteDoc");
        assert!(legacy_json.get("format").is_none());

        let envelope = dir.path().join("envelope.note");
        write_envelope_note(&envelope, &doc).unwrap();
        let envelope_json: Value =
            serde_json::from_str(&std::fs::read_to_string(&envelope).unwrap()).unwrap();
        assert_eq!(envelope_json["format"], "inscriptum-note");
        assert_eq!(envelope_json["features"]["block_ids"], 1);
        assert_eq!(envelope_json["document"]["type"], "noteDoc");
    }
}
