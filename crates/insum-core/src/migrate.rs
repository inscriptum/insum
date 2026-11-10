//! `migrate-ids` engine (§7.2, stage C): assign guaranteed block IDs to a
//! note and become the only writer of the `features.block_ids` marker
//! (§4.6). Business logic lives here; the CLI only shapes input/output.
//!
//! The write path is transactional: plan → lock → revision recheck →
//! atomic container rewrite (untouched ZIP entries are copied raw,
//! byte-identical) → release. Dry-run never touches the file and its
//! preview IDs may differ from the commit IDs (§7.2).

use std::collections::{BTreeSet, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use zip::write::SimpleFileOptions;

use crate::container::{NoteFile, UmContainer, note_rev_for_file_bytes};
use crate::error::{ErrorCode, UmError};
use crate::limits::Limits;
use crate::lock::{LockConfig, acquire_write_lock};
use crate::model::note_title;
use crate::schema::SchemaSource;

/// ID grammar from the artifact's `id_rules` (§4.4, §4.7 — never hardcoded
/// in Rust).
pub struct IdRules {
    pattern: regex::Regex,
    recommended_length: usize,
    attrs_key: String,
}

impl IdRules {
    /// Parse `id_rules` out of the resolved artifact. A runtime artifact
    /// without usable rules is damaged for migration purposes.
    pub fn from_artifact(artifact: &crate::schema::SchemaArtifact) -> Result<Self, UmError> {
        let rules = artifact
            .raw
            .get("id_rules")
            .filter(|rules| rules.is_object())
            .ok_or_else(|| {
                UmError::new(
                    ErrorCode::SchemaArtifactInvalid,
                    "the schema artifact carries no id_rules; block ID migration cannot run",
                )
                .with_hint("Refresh the plugin runtime artifact (`insum schema` shows its source).")
            })?;
        let pattern_src = rules
            .get("pattern")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                UmError::new(
                    ErrorCode::SchemaArtifactInvalid,
                    "id_rules.pattern is missing or empty",
                )
            })?;
        let pattern = regex::Regex::new(pattern_src).map_err(|err| {
            UmError::new(
                ErrorCode::SchemaArtifactInvalid,
                format!("id_rules.pattern {pattern_src:?} does not compile: {err}"),
            )
        })?;
        let recommended_length = rules
            .get("recommended_length")
            .and_then(Value::as_u64)
            .unwrap_or(10) as usize;
        if !(8..=12).contains(&recommended_length) {
            return Err(UmError::new(
                ErrorCode::SchemaArtifactInvalid,
                format!(
                    "id_rules.recommended_length {recommended_length} is outside the 8..12 grammar"
                ),
            ));
        }
        let attrs_key = rules
            .get("attrs_key")
            .and_then(Value::as_str)
            .filter(|k| !k.is_empty())
            .unwrap_or("id")
            .to_owned();
        Ok(Self {
            pattern,
            recommended_length,
            attrs_key,
        })
    }
}

/// Random ID generation per §4.4: cryptographically random, grammar-checked,
/// collision-checked within the note.
pub struct IdGenerator {
    rules: IdRules,
}

impl IdGenerator {
    pub fn new(rules: IdRules) -> Self {
        Self { rules }
    }

    /// Generate an ID that satisfies the artifact pattern and is not in
    /// `used`. Retries on collision (§4.4).
    pub fn generate(&self, used: &mut BTreeSet<String>) -> String {
        loop {
            let id = self.generate_once();
            if used.insert(id.clone()) {
                return id;
            }
        }
    }

    fn generate_once(&self) -> String {
        // ring 0.17 rand — cryptographically secure source (OsRng-backed)
        use rand::Rng;
        let alphabet: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let mut id = String::with_capacity(self.rules.recommended_length);
        while id.len() < self.rules.recommended_length {
            let byte = rand::rng().random_range(0..alphabet.len());
            id.push(alphabet[byte] as char);
        }
        debug_assert!(self.rules.pattern.is_match(&id));
        id
    }
}

/// One addressable block scheduled for an ID change.
pub struct MigrateChange {
    pub pointer: String,
    pub old_id: Option<String>,
    pub new_id: String,
    pub node_type: String,
}

/// Per-note migration outcome.
pub struct MigrateNotePlan {
    pub note_id: Option<String>,
    pub note_title: Option<String>,
    pub note_rev: String,
    pub missing_ids: u32,
    pub duplicate_ids: u32,
    pub invalid_ids: u32,
    pub changes: Vec<MigrateChange>,
}

impl MigrateNotePlan {
    pub fn change_count(&self) -> u32 {
        self.missing_ids + self.duplicate_ids + self.invalid_ids
    }

    pub fn to_json(&self) -> Value {
        json!({
            "note_id": self.note_id.clone(),
            "note_title": self.note_title.clone(),
            "note_rev": self.note_rev,
            "missing_ids": self.missing_ids,
            "duplicate_ids": self.duplicate_ids,
            "invalid_ids": self.invalid_ids,
            "changes": self.changes.iter().map(|c| json!({
                "pointer": c.pointer,
                "old_id": c.old_id.clone(),
                "new_id": c.new_id,
                "type": c.node_type,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Addressable node types come from the artifact (`addressable: true`) —
/// never from a hardcoded Rust list (§4.7).
fn addressable_types(artifact: &crate::schema::SchemaArtifact) -> BTreeSet<String> {
    artifact
        .nodes
        .iter()
        .filter(|(_, spec)| spec.addressable)
        .map(|(name, _)| name.clone())
        .collect()
}

/// Set the planned `attrs.id` values on a document. The pointers are the
/// same the plan was built with; unknown fields stay untouched (§12).
fn apply_ids(doc: &mut Value, plan: &[MigrateChange], attrs_key: &str) {
    let by_pointer: HashMap<&str, &str> = plan
        .iter()
        .map(|c| (c.pointer.as_str(), c.new_id.as_str()))
        .collect();
    fn set_ids(node: &mut Value, pointer: &str, by_pointer: &HashMap<&str, &str>, attrs_key: &str) {
        let object = match node.as_object_mut() {
            Some(object) => object,
            None => return,
        };
        if let Some(new_id) = by_pointer.get(pointer) {
            let attrs = object
                .entry("attrs".to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(attrs_map) = attrs.as_object_mut() {
                attrs_map.insert(attrs_key.to_owned(), Value::String((*new_id).to_owned()));
            }
        }
        if let Some(content) = object.get_mut("content").and_then(Value::as_array_mut) {
            for (index, child) in content.iter_mut().enumerate() {
                set_ids(
                    child,
                    &format!("{pointer}/content/{index}"),
                    by_pointer,
                    attrs_key,
                );
            }
        }
    }
    let _ = &doc;
    if doc.as_object_mut().is_some() {
        // the plan points at top-level blocks as /content/<i> — walk the
        // children of the document root with that base
        if let Some(content) = doc.get_mut("content").and_then(Value::as_array_mut) {
            for (index, child) in content.iter_mut().enumerate() {
                set_ids(child, &format!("/content/{index}"), &by_pointer, attrs_key);
            }
        }
    }
}

/// Plan one note: walk blocks in document order, keep the first valid unique
/// ID, schedule new IDs for missing/duplicate/invalid ones.
pub fn plan_note(
    doc: &Value,
    note_id: Option<String>,
    manifest_title: Option<&str>,
    note_rev: String,
    addressable: &BTreeSet<String>,
    generator: &IdGenerator,
) -> MigrateNotePlan {
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut plan = MigrateNotePlan {
        note_id,
        note_title: note_title(manifest_title, doc),
        note_rev,
        missing_ids: 0,
        duplicate_ids: 0,
        invalid_ids: 0,
        changes: Vec::new(),
    };
    fn walk(
        node: &Value,
        pointer: String,
        addressable: &BTreeSet<String>,
        generator: &IdGenerator,
        used: &mut BTreeSet<String>,
        plan: &mut MigrateNotePlan,
    ) {
        let object = match node.as_object() {
            Some(object) => object,
            None => return,
        };
        let node_type = object.get("type").and_then(Value::as_str).unwrap_or("");
        if addressable.contains(node_type) {
            let existing = object
                .get("attrs")
                .and_then(|attrs| attrs.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            match existing {
                None => {
                    plan.missing_ids += 1;
                    let new_id = generator.generate(used);
                    plan.changes.push(MigrateChange {
                        pointer: pointer.clone(),
                        old_id: None,
                        new_id,
                        node_type: node_type.to_owned(),
                    });
                }
                Some(old_id) if !generator.rules.pattern.is_match(&old_id) => {
                    plan.invalid_ids += 1;
                    let new_id = generator.generate(used);
                    plan.changes.push(MigrateChange {
                        pointer: pointer.clone(),
                        old_id: Some(old_id),
                        new_id,
                        node_type: node_type.to_owned(),
                    });
                }
                Some(old_id) if used.contains(&old_id) => {
                    plan.duplicate_ids += 1;
                    let new_id = generator.generate(used);
                    plan.changes.push(MigrateChange {
                        pointer: pointer.clone(),
                        old_id: Some(old_id),
                        new_id,
                        node_type: node_type.to_owned(),
                    });
                }
                Some(old_id) => {
                    used.insert(old_id);
                }
            }
        }
        if let Some(children) = object.get("content").and_then(Value::as_array) {
            for (index, child) in children.iter().enumerate() {
                walk(
                    child,
                    format!("{pointer}/content/{index}"),
                    addressable,
                    generator,
                    used,
                    plan,
                );
            }
        }
    }
    if let Some(content) = doc.get("content").and_then(Value::as_array) {
        for (index, node) in content.iter().enumerate() {
            walk(
                node,
                format!("/content/{index}"),
                addressable,
                generator,
                &mut used,
                &mut plan,
            );
        }
    }
    plan
}

/// The marker may only be stamped from a plugin-runtime or explicit
/// artifact that passes the hash check (§4.6: bundled fallback is never
/// evidence that writing is allowed).
fn marker_rejection(resolved_source: SchemaSource, hash_verified: bool) -> Result<(), UmError> {
    if resolved_source == SchemaSource::BundledFallback {
        return Err(UmError::new(
            ErrorCode::SchemaUnavailable,
            "only the bundled fallback schema is applicable here; migration refuses to write without the plugin runtime artifact",
        )
        .with_hint("Run inside the vault (the plugin runtime artifact is found next to the plugin), or pass the artifact explicitly with --schema."));
    }
    if !hash_verified {
        return Err(UmError::new(
            ErrorCode::SchemaMismatch,
            "the schema artifact failed the hash check; migration refuses to stamp the marker",
        )
        .with_hint(
            "Re-open the document in Obsidian so the plugin regenerates the runtime artifact.",
        ));
    }
    Ok(())
}

/// Resolve a schema artifact for a write (explicit → vault rule → refused
/// bundled fallback).
fn resolve_for_write(
    schema: Option<&Path>,
    limits: &Limits,
) -> Result<crate::schema::ResolvedSchema, UmError> {
    let start = std::env::current_dir()
        .map_err(|err| UmError::new(ErrorCode::IoError, format!("cannot read cwd: {err}")))?;
    let resolved = crate::schema::resolve(schema, &start, limits)?;
    marker_rejection(resolved.source, resolved.hash_verified)?;
    Ok(resolved)
}

fn revision_mismatch(expected: &str, actual: &str, what: &str) -> UmError {
    UmError::new(
        ErrorCode::NoteRevisionMismatch,
        format!("{what} changed after the migration plan was built"),
    )
    .with_hint("Re-run `insum migrate-ids` — the plan is rebuilt from the current revision.")
    .with_details(json!({ "expected": expected, "actual": actual }))
}

/// Hidden temp file next to the target (§12: temp artifacts are hidden and
/// live beside the original).
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| format!(".{}.migrate-tmp", n.to_string_lossy()))
        .unwrap_or_else(|| ".insum-migrate-tmp".to_owned());
    path.parent().unwrap_or(Path::new(".")).join(name)
}

/// Write bytes to the temp file and atomically replace the target.
fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), UmError> {
    let tmp = temp_path(path);
    {
        let mut file = std::fs::File::create(&tmp).map_err(|err| {
            UmError::new(
                ErrorCode::IoError,
                format!("cannot create temp file {}: {err}", tmp.display()),
            )
        })?;
        file.write_all(bytes).map_err(|err| {
            UmError::new(ErrorCode::IoError, format!("cannot write temp file: {err}"))
        })?;
        file.sync_all().map_err(|err| {
            UmError::new(ErrorCode::IoError, format!("cannot sync temp file: {err}"))
        })?;
    }
    std::fs::rename(&tmp, path).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        UmError::new(
            ErrorCode::WriteConflict,
            format!("cannot replace the file atomically: {err}"),
        )
    })
}

/// Response of `migrate-ids` on a `.um` container (§7.2).
pub fn migrate_um(
    path: &Path,
    filters: Option<&[String]>,
    schema: Option<&Path>,
    dry_run: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    let resolved = resolve_for_write(schema, limits)?;
    let meta = SchemaMeta {
        id: resolved.artifact.schema_id.clone(),
        schema_version: resolved.artifact.schema_version,
        schema_hash: resolved.artifact.schema_hash.clone(),
    };
    let addressable = addressable_types(&resolved.artifact);
    let generator = IdGenerator::new(IdRules::from_artifact(&resolved.artifact)?);

    let mut container = UmContainer::open(path, limits)?;
    let file_rev_before = container.file_rev()?;

    // selection: note ids (--note repeats) or every note needing migration
    let selected: Vec<crate::model::NoteDescriptor> = match filters {
        Some(filters) => {
            let mut out = Vec::new();
            for filter in filters {
                out.push(container.find_note(filter)?.clone());
            }
            out
        }
        None => container.notes_in_order().into_iter().cloned().collect(),
    };

    // plan every selected note; a marked note with nothing to fix is a no-op
    let mut plans: Vec<MigrateNotePlan> = Vec::new();
    for descriptor in &selected {
        let parsed = container.read_note_with_rev(descriptor, limits)?;
        let plan = plan_note(
            &parsed.note.doc,
            Some(descriptor.id.clone()),
            descriptor.title.as_deref(),
            parsed.note_rev,
            &addressable,
            &generator,
        );
        plans.push(plan);
    }
    let notes_json: Vec<Value> = plans.iter().map(|p| p.to_json()).collect();
    // a note without the marker is written even when its ids are complete:
    // the marker records the verification itself (§4.6)
    let would_write = plans
        .iter()
        .zip(selected.iter())
        .any(|(plan, descriptor)| plan.change_count() > 0 || !descriptor.block_ids_enabled());

    if dry_run {
        return Ok(json!({
            "api_version": 1,
            "dry_run": true,
            "path": path.display().to_string(),
            "notes": notes_json,
            "would_write": would_write,
        }));
    }

    if !would_write {
        return Ok(json!({
            "api_version": 1,
            "dry_run": false,
            "path": path.display().to_string(),
            "notes": notes_json,
            "file_rev": file_rev_before,
        }));
    }

    // transactional commit: lock → recheck → rewrite → release
    let _lock = acquire_write_lock(path, "migrate-ids", &LockConfig::defaults())?;

    // recheck revisions inside the lock (§7.2): the plan was built from the
    // generation captured above
    let mut reopened = UmContainer::open(path, limits)?;
    let file_rev_now = reopened.file_rev()?;
    if file_rev_now != file_rev_before {
        return Err(revision_mismatch(
            &file_rev_before,
            &file_rev_now,
            "the file",
        ));
    }
    for (plan, descriptor) in plans.iter().zip(selected.iter()) {
        if plan.changes.is_empty() {
            continue;
        }
        let parsed = reopened.read_note_with_rev(descriptor, limits)?;
        if parsed.note_rev != plan.note_rev {
            return Err(revision_mismatch(
                &plan.note_rev,
                &parsed.note_rev,
                &format!("note {:?}", descriptor.id),
            ));
        }
    }

    let new_bytes = rewrite_um(path, &mut reopened, &selected, &plans, &meta, limits)?;
    atomic_replace(path, &new_bytes)?;

    Ok(json!({
        "api_version": 1,
        "dry_run": false,
        "path": path.display().to_string(),
        "notes": plans.iter().map(|p| p.to_json()).collect::<Vec<_>>(),
        "file_rev": note_rev_for_file_bytes(&new_bytes),
    }))
}

/// Legacy `.note` → envelope with IDs (§4.6 envelope shape). An envelope
/// that already carries the marker keeps its schema metadata.
pub fn migrate_note_file(
    path: &Path,
    schema: Option<&Path>,
    dry_run: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    let resolved = resolve_for_write(schema, limits)?;
    let addressable = addressable_types(&resolved.artifact);
    let generator = IdGenerator::new(IdRules::from_artifact(&resolved.artifact)?);

    let note_file = NoteFile::open(path, limits, true)?;
    let already_envelope = note_file.note.envelope;
    let plan = plan_note(
        &note_file.note.doc,
        None,
        None,
        note_file.note_rev.clone(),
        &addressable,
        &generator,
    );

    if dry_run {
        return Ok(json!({
            "api_version": 1,
            "dry_run": true,
            "path": path.display().to_string(),
            "notes": [plan.to_json()],
            "would_write": plan.change_count() > 0 || !already_envelope,
        }));
    }

    if plan.change_count() == 0 && already_envelope {
        return Ok(json!({
            "api_version": 1,
            "dry_run": false,
            "path": path.display().to_string(),
            "notes": [plan.to_json()],
            "file_rev": note_file.file_rev.clone(),
        }));
    }

    let _lock = acquire_write_lock(path, "migrate-ids", &LockConfig::defaults())?;
    let reopened = NoteFile::open(path, limits, true)?;
    if reopened.note_rev != note_file.note_rev {
        return Err(revision_mismatch(
            &note_file.note_rev,
            &reopened.note_rev,
            "the .note file",
        ));
    }

    let mut doc = reopened.note.doc;
    let attrs_key = IdRules::from_artifact(&resolved.artifact)?.attrs_key;
    apply_ids(&mut doc, &plan.changes, &attrs_key);

    // the envelope's own schema metadata is preserved when present (the read
    // path drops it, so the raw payload is the source); a legacy note gets
    // the metadata of the artifact the migration ran against
    let artifact_schema = json!({
        "id": resolved.artifact.schema_id,
        "schema_version": resolved.artifact.schema_version,
        "schema_hash": resolved.artifact.schema_hash,
    });
    let existing_schema = if already_envelope {
        let raw: Value = serde_json::from_slice(&std::fs::read(path).map_err(|err| {
            UmError::new(
                ErrorCode::IoError,
                format!("cannot read {}: {err}", path.display()),
            )
        })?)
        .map_err(|err| UmError::internal(format!("envelope does not parse: {err}")))?;
        raw.get("schema")
            .filter(|schema| schema.is_object())
            .cloned()
            .unwrap_or(artifact_schema)
    } else {
        artifact_schema
    };
    let mut envelope = json!({
        "format": "inscriptum-note",
        "version": 1,
        "features": reopened.note.features.clone().unwrap_or(json!({ "block_ids": 1 })),
        "schema": existing_schema,
        "document": doc,
    });
    let map = envelope
        .as_object_mut()
        .ok_or_else(|| UmError::internal("envelope root must be an object"))?;
    let features = map
        .entry("features".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(map) = features.as_object_mut() {
        map.insert("block_ids".to_owned(), json!(1));
    }

    let new_bytes = serde_json::to_vec(&envelope)
        .map_err(|err| UmError::internal(format!("envelope does not serialize: {err}")))?;
    atomic_replace(path, &new_bytes)?;

    Ok(json!({
        "api_version": 1,
        "dry_run": false,
        "path": path.display().to_string(),
        "notes": [plan.to_json()],
        "file_rev": note_rev_for_file_bytes(&new_bytes),
    }))
}

/// Schema metadata the marker carries (§4.6).
struct SchemaMeta {
    id: String,
    schema_version: u32,
    schema_hash: String,
}

/// Rebuild the `.um` container: modified manifest + modified payloads, every
/// other entry copied raw (byte-identical, order and timestamps preserved).
fn rewrite_um(
    path: &Path,
    container: &mut UmContainer,
    selected: &[crate::model::NoteDescriptor],
    plans: &[MigrateNotePlan],
    meta: &SchemaMeta,
    limits: &Limits,
) -> Result<Vec<u8>, UmError> {
    // payloads to replace, by archive entry name
    let mut replacements: HashMap<String, Vec<u8>> = HashMap::new();
    for (descriptor, plan) in selected.iter().zip(plans.iter()) {
        if plan.changes.is_empty() {
            continue;
        }
        let payload = container.read_note(descriptor, limits)?;
        let mut doc = payload.doc;
        let attrs_key = container
            .manifest
            .raw
            .get("id_rules")
            .and_then(|rules| rules.get("attrs_key"))
            .and_then(Value::as_str)
            .unwrap_or("id")
            .to_owned();
        apply_ids(&mut doc, &plan.changes, &attrs_key);
        let bytes = serde_json::to_vec(&doc)
            .map_err(|err| UmError::internal(format!("note payload does not serialize: {err}")))?;
        replacements.insert(descriptor.path.clone(), bytes);
    }

    // manifest: stamp the marker on selected descriptors, keep unknown fields
    let manifest_value = stamp_manifest(container.manifest.raw.clone(), selected, meta)?;
    let manifest_bytes = serde_json::to_vec_pretty(&manifest_value)
        .map_err(|err| UmError::internal(format!("manifest does not serialize: {err}")))?;
    replacements.insert("manifest.json".to_owned(), manifest_bytes);

    // rebuild: walk the source archive in order
    let source = std::fs::File::open(path).map_err(|err| {
        UmError::new(
            ErrorCode::IoError,
            format!("cannot read {}: {err}", path.display()),
        )
    })?;
    let mut archive = zip::ZipArchive::new(source).map_err(|err| {
        crate::container::torn_error(
            "truncated",
            path,
            format!("ZIP container is incomplete or corrupt: {err}"),
        )
    })?;
    let buffer = std::io::Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(buffer);
    for index in 0..archive.len() {
        let (name, last_modified) = {
            let raw = archive.by_index_raw(index).map_err(|err| {
                UmError::new(
                    ErrorCode::ArchiveEntryNameInvalid,
                    format!("cannot read entry {index}: {err}"),
                )
            })?;
            let name = raw
                .name()
                .map_err(|err| {
                    UmError::new(
                        ErrorCode::ArchiveEntryNameInvalid,
                        format!("cannot decode entry {index} name: {err}"),
                    )
                })?
                .into_owned();
            (name, raw.last_modified().unwrap_or_default())
        };
        let name_str: &str = name.as_ref();
        if let Some(bytes) = replacements.get(name_str) {
            // replace the entry: same name, same declared mtime, fresh deflate
            let options = SimpleFileOptions::default().last_modified_time(last_modified);
            writer
                .start_file(name_str, options)
                .map_err(|err| UmError::internal(format!("cannot start entry {name:?}: {err}")))?;
            writer
                .write_all(bytes)
                .map_err(|err| UmError::internal(format!("cannot write entry {name:?}: {err}")))?;
        } else {
            // untouched entry: raw copy keeps the compressed bytes verbatim
            let raw = archive.by_index_raw(index).map_err(|err| {
                UmError::new(
                    ErrorCode::ArchiveEntryNameInvalid,
                    format!("cannot read entry {index}: {err}"),
                )
            })?;
            writer
                .raw_copy_file(raw)
                .map_err(|err| UmError::internal(format!("cannot copy entry {name:?}: {err}")))?;
        }
    }
    let buffer = writer
        .finish()
        .map_err(|err| UmError::internal(format!("cannot finish the new container: {err}")))?;
    Ok(buffer.into_inner())
}

/// Set `features.block_ids = 1` and schema metadata on the selected
/// descriptors; unknown manifest fields are preserved as values (§12).
fn stamp_manifest(
    mut manifest: Value,
    selected: &[crate::model::NoteDescriptor],
    meta: &SchemaMeta,
) -> Result<Value, UmError> {
    let notes = manifest
        .get_mut("notes")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| UmError::new(ErrorCode::ManifestInvalid, "manifest has no notes array"))?;
    let selected_ids: BTreeSet<&str> = selected.iter().map(|d| d.id.as_str()).collect();
    for entry in notes.iter_mut() {
        let id = entry.get("id").and_then(Value::as_str).unwrap_or("");
        if !selected_ids.contains(id) {
            continue;
        }
        let object = entry.as_object_mut().ok_or_else(|| {
            UmError::new(
                ErrorCode::ManifestInvalid,
                "manifest note entry is not an object",
            )
        })?;
        // the marker and the fingerprint it was stamped under live in the
        // descriptor's `features` object: the `schema` key of a descriptor
        // is the editor profile ("title"/"plain") and must not be touched
        let features = object
            .entry("features".to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(map) = features.as_object_mut() {
            map.insert("block_ids".to_owned(), json!(1));
            map.insert(
                "schema".to_owned(),
                json!({
                    "id": meta.id,
                    "schema_version": meta.schema_version,
                    "schema_hash": meta.schema_hash,
                }),
            );
        }
    }
    Ok(manifest)
}
