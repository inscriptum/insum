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
use crate::pointer;
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
    pub fn new(rules: IdRules) -> Result<Self, UmError> {
        // the generator must produce ids the artifact pattern accepts —
        // fail fast instead of debug_asserting at stamping time
        let generator = Self { rules };
        let sample = generator.generate_once();
        if !generator.rules.pattern.is_match(&sample) {
            return Err(UmError::new(
                ErrorCode::SchemaArtifactInvalid,
                format!(
                    "id_rules.pattern rejects the generator output ({sample:?}); migration cannot run"
                ),
            ));
        }
        Ok(generator)
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
        // rand 0.9 — cryptographically secure source (OsRng-backed)
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
use crate::schema::addressable_types;

/// Set the planned `attrs.id` values on a document. The pointers are the
/// same the plan was built with; unknown fields stay untouched (§12). A
/// node whose `attrs` exists but is not an object is corrupted input —
/// returned as an error, never silently skipped (the marker must not lie).
fn apply_ids(doc: &mut Value, plan: &[MigrateChange], attrs_key: &str) -> Result<(), UmError> {
    let by_pointer: HashMap<&str, &str> = plan
        .iter()
        .map(|c| (c.pointer.as_str(), c.new_id.as_str()))
        .collect();
    fn set_ids(
        node: &mut Value,
        pointer: &str,
        by_pointer: &HashMap<&str, &str>,
        attrs_key: &str,
    ) -> Result<(), UmError> {
        let object = match node.as_object_mut() {
            Some(object) => object,
            None => return Ok(()),
        };
        if let Some(new_id) = by_pointer.get(pointer) {
            match object.get_mut("attrs") {
                None => {
                    object.insert("attrs".to_owned(), json!({ attrs_key: new_id }));
                }
                Some(Value::Object(attrs_map)) => {
                    attrs_map.insert(attrs_key.to_owned(), Value::String((*new_id).to_owned()));
                }
                Some(other) => {
                    return Err(UmError::new(
                        ErrorCode::NoteDocInvalid,
                        format!(
                            "node at {pointer} has a non-object attrs ({other}); fix the data first"
                        ),
                    ));
                }
            }
        }
        if let Some(content) = object.get_mut("content").and_then(Value::as_array_mut) {
            for (index, child) in content.iter_mut().enumerate() {
                set_ids(
                    child,
                    &format!("{pointer}/content/{index}"),
                    by_pointer,
                    attrs_key,
                )?;
            }
        }
        Ok(())
    }
    // the plan points at top-level blocks as /content/<i>
    if let Some(content) = doc.get_mut("content").and_then(Value::as_array_mut) {
        for (index, child) in content.iter_mut().enumerate() {
            set_ids(child, &format!("/content/{index}"), &by_pointer, attrs_key)?;
        }
    }
    // the plan and the document must agree — every planned id is present,
    // none silently skipped (the marker must not lie, §4.6)
    for change in plan {
        let actual = pointer::resolve(doc, &change.pointer)
            .ok()
            .and_then(|node| node.get("attrs").and_then(|attrs| attrs.get(attrs_key)))
            .and_then(Value::as_str);
        if actual != Some(change.new_id.as_str()) {
            return Err(UmError::internal(format!(
                "post-apply check failed: block at {} does not carry the planned id",
                change.pointer
            )));
        }
    }
    Ok(())
}

/// Plan one note: walk blocks in document order, keep the first valid unique
/// ID, schedule new IDs for missing/duplicate/invalid ones. All valid ids
/// are collected in a pre-pass, so generated ids can never collide with a
/// valid id later in the document. A node whose `attrs` exists but is not
/// an object fails the note (§4.4: every addressable block MUST carry an
/// id; the marker must not lie).
pub fn plan_note(
    doc: &Value,
    note_id: Option<String>,
    manifest_title: Option<&str>,
    note_rev: String,
    addressable: &BTreeSet<String>,
    generator: &IdGenerator,
    attrs_key: &str,
) -> Result<MigrateNotePlan, UmError> {
    fn check_attrs(node: &Value, pointer: &str) -> Result<(), UmError> {
        if let Some(attrs) = node.get("attrs")
            && !attrs.is_object()
        {
            return Err(UmError::new(
                ErrorCode::NoteDocInvalid,
                format!("node at {pointer} has a non-object attrs; fix the data before migration"),
            ));
        }
        Ok(())
    }
    fn walk_shapes(node: &Value, pointer: &str) -> Result<(), UmError> {
        check_attrs(node, pointer)?;
        if let Some(children) = node.get("content").and_then(Value::as_array) {
            for (index, child) in children.iter().enumerate() {
                walk_shapes(child, &format!("{pointer}/content/{index}"))?;
            }
        }
        Ok(())
    }
    // the document must not carry non-object attrs anywhere the plan might
    // touch — refuse before planning instead of lying with the marker
    walk_shapes(doc, "")?;

    // pre-pass: every valid id in the note joins the used set, whether it
    // comes before or after the block being planned
    let mut used: BTreeSet<String> = BTreeSet::new();
    fn collect_valid(
        node: &Value,
        pattern: &regex::Regex,
        attrs_key: &str,
        used: &mut BTreeSet<String>,
    ) {
        if let Some(id) = node
            .get("attrs")
            .and_then(|attrs| attrs.get(attrs_key))
            .and_then(Value::as_str)
            .filter(|id| pattern.is_match(id))
        {
            used.insert(id.to_owned());
        }
        if let Some(children) = node.get("content").and_then(Value::as_array) {
            for child in children {
                collect_valid(child, pattern, attrs_key, used);
            }
        }
    }
    collect_valid(doc, &generator.rules.pattern, attrs_key, &mut used);

    let mut plan = MigrateNotePlan {
        note_id,
        note_title: note_title(manifest_title, doc),
        note_rev,
        missing_ids: 0,
        duplicate_ids: 0,
        invalid_ids: 0,
        changes: Vec::new(),
    };
    // ids already consumed by an earlier duplicate/invalid replacement join
    // the used set through generate(); the first valid duplicate keeps its id
    let mut kept: BTreeSet<String> = BTreeSet::new();
    #[allow(clippy::too_many_arguments)]
    fn walk(
        node: &Value,
        pointer: String,
        addressable: &BTreeSet<String>,
        generator: &IdGenerator,
        attrs_key: &str,
        used: &mut BTreeSet<String>,
        kept: &mut BTreeSet<String>,
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
                .and_then(|attrs| attrs.get(attrs_key))
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
                Some(old_id) if kept.contains(&old_id) => {
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
                    kept.insert(old_id);
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
                    attrs_key,
                    used,
                    kept,
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
                attrs_key,
                &mut used,
                &mut kept,
                &mut plan,
            );
        }
    }
    Ok(plan)
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
            ErrorCode::IoError,
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
    let generator = IdGenerator::new(IdRules::from_artifact(&resolved.artifact)?)?;

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
    let attrs_key = IdRules::from_artifact(&resolved.artifact)?.attrs_key;
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
            &attrs_key,
        )?;
        // plan == commit: the dry-run must show the same refusal the commit
        // would produce, so the planned document is validated here with the
        // same shared check the pre-commit validation runs (B-1)
        let mut planned_doc = parsed.note.doc.clone();
        apply_ids(&mut planned_doc, &plan.changes, &attrs_key)?;
        validate_migrated_doc(
            &descriptor.id,
            &planned_doc,
            &plan,
            &addressable,
            &attrs_key,
            limits,
        )?;
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

    // transactional commit: lock → recheck → rewrite → validate → release
    let lock = acquire_write_lock(path, "migrate-ids", &LockConfig::defaults())?;
    // released on success and on error (lock-protocol): a secondary release
    // failure must not mask the primary outcome
    let outcome = commit_um(
        path,
        &mut container,
        &selected,
        &plans,
        &meta,
        &attrs_key,
        &addressable,
        limits,
    );
    match outcome {
        Ok(value) => {
            lock.release()?;
            Ok(value)
        }
        Err(err) => {
            let _ = lock.release();
            Err(err)
        }
    }
}

/// The post-migration document must satisfy the contract the marker vouches
/// for (§10.1): every planned id present at its pointer, ids unique per
/// note, grammar-valid, and required for artifact-addressable types only
/// (§4.7 — the id-required universe is the same one `plan_note` schedules
/// against). Dry-run runs this on the planned copy of the document; commit
/// runs it on the rewritten container — one shared implementation keeps
/// `dry-run == commit`.
fn validate_migrated_doc(
    note: &str,
    doc: &Value,
    plan: &MigrateNotePlan,
    addressable: &BTreeSet<String>,
    attrs_key: &str,
    limits: &Limits,
) -> Result<(), UmError> {
    for change in &plan.changes {
        let actual = pointer::resolve(doc, &change.pointer)
            .ok()
            .and_then(|node| {
                node.get("attrs")
                    .and_then(|attrs| attrs.get(attrs_key))
                    .and_then(Value::as_str)
            });
        if actual != Some(change.new_id.as_str()) {
            return Err(UmError::internal(format!(
                "note {note:?} does not carry the planned id at {}",
                change.pointer
            )));
        }
    }
    // ids unique within the note
    let mut ids: Vec<String> = Vec::new();
    fn collect(node: &Value, attrs_key: &str, ids: &mut Vec<String>) {
        if let Some(id) = node
            .get("attrs")
            .and_then(|attrs| attrs.get(attrs_key))
            .and_then(Value::as_str)
        {
            ids.push(id.to_owned());
        }
        if let Some(children) = node.get("content").and_then(Value::as_array) {
            for child in children {
                collect(child, attrs_key, ids);
            }
        }
    }
    collect(doc, attrs_key, &mut ids);
    let unique: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    if ids.len() != unique.len() {
        return Err(validation_refusal(
            note,
            "BLOCK_ID_DUPLICATE",
            format!("note {note:?} has duplicate ids"),
        ));
    }
    // structural validation with the artifact-addressable id universe
    let validation =
        crate::validate_note_doc_with_schema(doc, true, limits, None, Some(addressable))?;
    if !validation.valid() {
        let first = validation.errors.first();
        return Err(validation_refusal(
            note,
            first.map(|d| d.code).unwrap_or("NOTE_DOC_INVALID"),
            format!(
                "note {note:?} would not satisfy the block-id contract: {}",
                first.map(|d| d.message.clone()).unwrap_or_default()
            ),
        ));
    }
    Ok(())
}

/// Classify a validation refusal with an existing registry code (§11): the
/// marker must not lie, and a data problem is exit 4 — never INTERNAL.
fn validation_refusal(note: &str, code: &str, message: String) -> UmError {
    let code = match code {
        "BLOCK_ID_MISSING" => ErrorCode::BlockIdMissing,
        "BLOCK_ID_DUPLICATE" => ErrorCode::BlockIdDuplicate,
        "BLOCK_ID_INVALID" => ErrorCode::BlockIdInvalid,
        _ => ErrorCode::NoteDocInvalid,
    };
    UmError::new(code, message).with_hint(format!(
        "Fix the data before migrating note {note:?}; `insum validate` shows the full diagnostic list. The file was not modified."
    ))
}

/// Lock-held phase of the `.um` commit: revision recheck, rewrite,
/// validation of the result (§12), atomic replace.
#[allow(clippy::too_many_arguments)]
fn commit_um(
    path: &Path,
    container: &mut UmContainer,
    selected: &[crate::model::NoteDescriptor],
    plans: &[MigrateNotePlan],
    meta: &SchemaMeta,
    attrs_key: &str,
    addressable: &BTreeSet<String>,
    limits: &Limits,
) -> Result<Value, UmError> {
    let file_rev_before = container.file_rev()?;

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

    let new_bytes = rewrite_um(
        path,
        &mut reopened,
        selected,
        plans,
        meta,
        attrs_key,
        limits,
    )?;
    validate_result_um(&new_bytes, selected, plans, attrs_key, addressable, limits)?;
    atomic_replace(path, &new_bytes)?;

    Ok(json!({
        "api_version": 1,
        "dry_run": false,
        "path": path.display().to_string(),
        "notes": plans.iter().map(|p| p.to_json()).collect::<Vec<_>>(),
        "file_rev": note_rev_for_file_bytes(&new_bytes),
    }))
}

/// Pre-commit validation (§12: the result passes validation before commit):
/// every selected note of the rewritten container — a note that only
/// receives the marker included — passes the shared migration validation.
fn validate_result_um(
    new_bytes: &[u8],
    selected: &[crate::model::NoteDescriptor],
    plans: &[MigrateNotePlan],
    attrs_key: &str,
    addressable: &BTreeSet<String>,
    limits: &Limits,
) -> Result<(), UmError> {
    let tmp =
        std::env::temp_dir().join(format!("insum-migrate-validate-{}.um", std::process::id()));
    std::fs::write(&tmp, new_bytes).map_err(|err| {
        UmError::new(
            ErrorCode::IoError,
            format!("cannot stage the result: {err}"),
        )
    })?;
    let result = (|| {
        let mut container = UmContainer::open(&tmp, limits)?;
        for (descriptor, plan) in selected.iter().zip(plans.iter()) {
            let parsed = container.read_note_with_rev(descriptor, limits)?;
            validate_migrated_doc(
                &descriptor.id,
                &parsed.note.doc,
                plan,
                addressable,
                attrs_key,
                limits,
            )?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&tmp);
    result
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
    let rules = IdRules::from_artifact(&resolved.artifact)?;
    let attrs_key = rules.attrs_key.clone();
    let generator = IdGenerator::new(rules)?;

    let note_file = NoteFile::open(path, limits, true)?;
    let already_envelope = note_file.note.envelope;
    // the marker state of an envelope: features.block_ids == 1 (§4.6) —
    // an envelope without the marker is selected even with complete ids
    let marker_set = note_file
        .note
        .features
        .as_ref()
        .and_then(|f| f.get("block_ids"))
        .and_then(Value::as_u64)
        == Some(1);
    let plan = plan_note(
        &note_file.note.doc,
        None,
        None,
        note_file.note_rev.clone(),
        &addressable,
        &generator,
        &attrs_key,
    )?;
    // plan == commit: the same shared validation the commit runs, on the
    // planned copy of the document (B-1)
    let mut planned_doc = note_file.note.doc.clone();
    apply_ids(&mut planned_doc, &plan.changes, &attrs_key)?;
    validate_migrated_doc(
        "<note>",
        &planned_doc,
        &plan,
        &addressable,
        &attrs_key,
        limits,
    )?;

    if dry_run {
        return Ok(json!({
            "api_version": 1,
            "dry_run": true,
            "path": path.display().to_string(),
            "notes": [plan.to_json()],
            "would_write": plan.change_count() > 0 || !marker_set,
        }));
    }

    if plan.change_count() == 0 && marker_set {
        return Ok(json!({
            "api_version": 1,
            "dry_run": false,
            "path": path.display().to_string(),
            "notes": [plan.to_json()],
            "file_rev": note_file.file_rev.clone(),
        }));
    }

    let lock = acquire_write_lock(path, "migrate-ids", &LockConfig::defaults())?;
    let outcome = commit_note_file(
        path,
        &note_file.note_rev,
        &plan,
        &resolved,
        already_envelope,
        &attrs_key,
        &addressable,
        limits,
    );
    match outcome {
        Ok(value) => {
            lock.release()?;
            Ok(value)
        }
        Err(err) => {
            let _ = lock.release();
            Err(err)
        }
    }
}

/// Lock-held phase of the `.note` commit: revision recheck, envelope build
/// with unknown fields preserved (§12), validation, atomic replace.
#[allow(clippy::too_many_arguments)]
fn commit_note_file(
    path: &Path,
    expected_rev: &str,
    plan: &MigrateNotePlan,
    resolved: &crate::schema::ResolvedSchema,
    already_envelope: bool,
    attrs_key: &str,
    addressable: &BTreeSet<String>,
    limits: &Limits,
) -> Result<Value, UmError> {
    let reopened = NoteFile::open(path, limits, true)?;
    if reopened.note_rev != expected_rev {
        return Err(revision_mismatch(
            expected_rev,
            &reopened.note_rev,
            "the .note file",
        ));
    }

    let mut doc = reopened.note.doc;
    apply_ids(&mut doc, &plan.changes, attrs_key)?;

    // the envelope is built ON the raw payload when one exists: unknown
    // top-level fields are preserved as values (§12 — anything that cannot
    // be preserved must abort the write, not be dropped silently)
    let mut envelope: Value = if already_envelope {
        serde_json::from_slice(&std::fs::read(path).map_err(|err| {
            UmError::new(
                ErrorCode::IoError,
                format!("cannot read {}: {err}", path.display()),
            )
        })?)
        .map_err(|err| UmError::internal(format!("envelope does not parse: {err}")))?
    } else {
        json!({})
    };
    {
        let map = envelope
            .as_object_mut()
            .ok_or_else(|| UmError::internal("envelope root must be an object"))?;
        map.insert("format".to_owned(), json!("inscriptum-note"));
        map.insert("version".to_owned(), json!(1));
        let features = map
            .entry("features".to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        match features {
            Value::Object(map) => {
                map.insert("block_ids".to_owned(), json!(1));
            }
            _ => {
                return Err(UmError::new(
                    ErrorCode::NoteDocInvalid,
                    "the envelope features field is not an object; fix the data before migration",
                ));
            }
        }
        // the envelope's own schema metadata is kept when present; a legacy
        // note gets the metadata of the artifact the migration ran against
        let schema = map.entry("schema".to_owned()).or_insert_with(|| {
            json!({
                "id": resolved.artifact.schema_id,
                "schema_version": resolved.artifact.schema_version,
                "schema_hash": resolved.artifact.schema_hash,
            })
        });
        if !schema.is_object() {
            return Err(UmError::new(
                ErrorCode::NoteDocInvalid,
                "the envelope schema field is not an object; fix the data before migration",
            ));
        }
        map.insert("document".to_owned(), doc.clone());
    }

    // §12: the result passes validation before commit — the shared
    // migration validation (dry-run runs exactly the same check)
    validate_migrated_doc("<note>", &doc, plan, addressable, attrs_key, limits)?;

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
#[allow(clippy::too_many_arguments)]
fn rewrite_um(
    path: &Path,
    container: &mut UmContainer,
    selected: &[crate::model::NoteDescriptor],
    plans: &[MigrateNotePlan],
    meta: &SchemaMeta,
    attrs_key: &str,
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
        apply_ids(&mut doc, &plan.changes, attrs_key)?;
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
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if !selected_ids.contains(id.as_str()) {
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
        match features {
            Value::Object(map) => {
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
            _ => {
                // refuse-don't-skip: a selected note must not report success
                // without its marker (same rule as the payload attrs path)
                return Err(UmError::new(
                    ErrorCode::ManifestInvalid,
                    format!(
                        "descriptor {id:?} has a non-object features field; fix the manifest before migration"
                    ),
                ));
            }
        }
    }
    Ok(manifest)
}
