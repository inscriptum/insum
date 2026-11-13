//! `apply` engine (§8, stage D): addressed block writes by ID with a
//! revision CAS. One package mutates one note; the commit inherits the
//! transactional pipeline (`commit.rs`, `lock.rs`): lock → revision recheck
//! with bounded re-apply → raw-copy container rebuild → staged validation →
//! atomic replace. Dry-run shares the plan phase and never touches the
//! file (plan == commit).
//!
//! Wire format (§8.2): the CLI owns IDs. Input nodes must not carry
//! `attrs.<id_key>`; an ID that belongs to another block is `ID_CONFLICT`,
//! any other input ID is `BLOCK_ID_IMMUTABLE`. `replace` keeps the
//! target's ID; descendants of `replace`/`replace_content` may preserve
//! their IDs only when they belonged to the replaced subtree. New
//! addressable blocks receive generator IDs after the whole package has
//! been applied (document order).

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::commit::{
    StagedFile, ensure_writable_source, payload_bytes, pre_replace_verification, revision_mismatch,
    rewrite_um_entries,
};
use crate::container::{NoteFile, UmContainer, note_rev_for_file_bytes};
use crate::error::{ErrorCode, UmError};
use crate::limits::Limits;
use crate::lock::{LockConfig, acquire_write_lock, check_sidecar_name_fits};
use crate::migrate::{IdGenerator, IdRules};
use crate::pmschema::PmSchema;
use crate::schema::{SchemaSource, addressable_types, resolve};
use crate::validate::{Diag, validate_note_doc_with_schema};

// ---------------------------------------------------------------------------
// Package model (§8.1)
// ---------------------------------------------------------------------------

/// One operation of the package. Shapes are validated at parse time; ID
/// rules are checked against the document state during application (§8.3).
#[derive(Debug)]
enum Operation {
    Replace {
        id: String,
        node: Value,
    },
    Delete {
        id: String,
    },
    InsertBefore {
        id: String,
        nodes: Vec<Value>,
    },
    InsertAfter {
        id: String,
        nodes: Vec<Value>,
    },
    AppendChild {
        parent_id: Option<String>,
        nodes: Vec<Value>,
    },
    SetAttrs {
        id: String,
        set: Map<String, Value>,
        unset: Vec<String>,
    },
    ReplaceContent {
        id: String,
        content: Vec<Value>,
    },
}

/// The parsed operations package: one note, one base revision, N operations.
#[derive(Debug)]
struct Package {
    note_id: Option<String>,
    base_note_rev: String,
    if_match: Option<String>,
    operations: Vec<Operation>,
}

/// The canonical UM_MAX_OPS_BYTES verdict for a freshly read package (§5.5).
/// The CLI reader and the engine re-check share this one construction so the
/// limit cannot be reported with two different texts or codes.
pub fn check_ops_package_size(bytes: &[u8], limits: &Limits) -> Result<(), UmError> {
    if bytes.len() > limits.max_ops_bytes {
        return Err(limit_exceeded(format!(
            "operations package exceeds UM_MAX_OPS_BYTES ({} bytes)",
            limits.max_ops_bytes
        )));
    }
    Ok(())
}

/// Parse and shape-check the package (§8.1). Package-envelope violations
/// (non-UTF-8 bytes, api_version, base_note_rev, note_id presence) are
/// `USAGE`; malformed operation entries are `INVALID_OPERATION`; malformed
/// node JSON inside an operation is `INVALID_NODE`. Limits bound the package
/// size and the operation count (§5.5).
fn parse_package(
    bytes: &[u8],
    limits: &Limits,
    require_note_id: bool,
    if_match: Option<&str>,
) -> Result<Package, UmError> {
    check_ops_package_size(bytes, limits)?;
    let text =
        std::str::from_utf8(bytes).map_err(|_| usage("the operations package is not UTF-8"))?;
    let value =
        crate::container::parse_json_limited(text, limits.max_depth, ErrorCode::InvalidOperation)
            .map_err(|err| {
            // WHY: parse_json_limited owns the depth verdict — a depth over
            // UM_MAX_DEPTH must stay LIMIT_EXCEEDED (exit 5), only a genuine
            // parse failure is package-envelope USAGE
            if err.code == ErrorCode::LimitExceeded {
                err
            } else {
                usage(format!(
                    "the operations package does not parse: {}",
                    err.message
                ))
            }
        })?;
    let object = value
        .as_object()
        .ok_or_else(|| usage("the operations package is not a JSON object"))?;
    let api_version = object
        .get("api_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| usage("package is missing integer `api_version`"))?;
    if api_version != 1 {
        return Err(usage(format!(
            "unsupported package api_version {api_version} (this CLI speaks 1)"
        )));
    }
    let note_id = match object.get("note_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if !id.is_empty() => Some(id.clone()),
        Some(_) => {
            return Err(usage(
                "package `note_id` must be a non-empty string or null",
            ));
        }
    };
    if require_note_id && note_id.is_none() {
        return Err(usage(
            "package is missing `note_id` (required for a .um container)",
        ));
    }
    if !require_note_id && note_id.is_some() {
        return Err(usage(
            "package `note_id` must be absent or null for a .note file (it holds a single document)",
        ));
    }
    let base_note_rev = object
        .get("base_note_rev")
        .and_then(Value::as_str)
        .ok_or_else(|| usage("package is missing string `base_note_rev`"))?;
    if !is_full_rev(base_note_rev) {
        return Err(usage(
            "package `base_note_rev` must be a full revision (\"sha256:\" + 64 hex characters) — take it from `insum tree`, `insum read`, `insum block get` or `insum search`",
        ));
    }
    let if_match = match if_match {
        None => None,
        Some(prefix) => {
            let prefix = normalized_prefix(prefix)?;
            if !base_note_rev.to_ascii_lowercase().starts_with(&prefix) {
                return Err(usage(format!(
                    "--if-match {prefix:?} is not a prefix of the package `base_note_rev`; the two must agree"
                )));
            }
            Some(prefix)
        }
    };
    let ops_value = object
        .get("operations")
        .and_then(Value::as_array)
        .ok_or_else(|| usage("package is missing array `operations`"))?;
    if ops_value.len() > limits.max_apply_ops {
        return Err(limit_exceeded(format!(
            "package holds {} operations, UM_MAX_APPLY_OPS is {}",
            ops_value.len(),
            limits.max_apply_ops
        )));
    }
    let mut operations = Vec::with_capacity(ops_value.len());
    for op_value in ops_value {
        operations.push(parse_operation(op_value)?);
    }
    Ok(Package {
        note_id,
        base_note_rev: base_note_rev.to_owned(),
        if_match,
        operations,
    })
}

fn parse_operation(value: &Value) -> Result<Operation, UmError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_operation("an operation entry is not a JSON object"))?;
    let op = object
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_operation("an operation entry is missing string `op`"))?;
    let id = |field: &str| -> Result<String, UmError> {
        match object.get(field) {
            Some(Value::String(id)) if !id.is_empty() => Ok(id.clone()),
            Some(_) => Err(invalid_operation(format!(
                "operation {op:?} needs a non-empty string `{field}`"
            ))),
            None => Err(invalid_operation(format!(
                "operation {op:?} is missing `{field}`"
            ))),
        }
    };
    let nodes = |field: &str| -> Result<Vec<Value>, UmError> {
        let array = object
            .get(field)
            .and_then(Value::as_array)
            .filter(|array| !array.is_empty())
            .ok_or_else(|| {
                invalid_operation(format!(
                    "operation {op:?} needs a non-empty array `{field}`"
                ))
            })?;
        for node in array {
            if !node.is_object() {
                return Err(invalid_node(format!(
                    "operation {op:?} carries a node that is not a JSON object"
                )));
            }
        }
        Ok(array.clone())
    };
    match op {
        "replace" => {
            let node = object
                .get("node")
                .ok_or_else(|| invalid_operation("operation \"replace\" is missing `node`"))?;
            if !node.is_object() {
                return Err(invalid_node(
                    "operation \"replace\" carries a node that is not a JSON object",
                ));
            }
            Ok(Operation::Replace {
                id: id("id")?,
                node: node.clone(),
            })
        }
        "delete" => Ok(Operation::Delete { id: id("id")? }),
        "insert_before" => Ok(Operation::InsertBefore {
            id: id("id")?,
            nodes: nodes("nodes")?,
        }),
        "insert_after" => Ok(Operation::InsertAfter {
            id: id("id")?,
            nodes: nodes("nodes")?,
        }),
        "append_child" => {
            let parent_id = match object.get("parent_id") {
                None | Some(Value::Null) => None,
                Some(Value::String(id)) if !id.is_empty() => Some(id.clone()),
                Some(_) => {
                    return Err(invalid_operation(
                        "operation \"append_child\" needs string `parent_id` or null (the document root)",
                    ));
                }
            };
            Ok(Operation::AppendChild {
                parent_id,
                nodes: nodes("nodes")?,
            })
        }
        "set_attrs" => {
            let set = match object.get("set") {
                None => Map::new(),
                Some(Value::Object(map)) => map.clone(),
                Some(_) => {
                    return Err(invalid_operation(
                        "operation \"set_attrs\" needs object `set`",
                    ));
                }
            };
            let unset = match object.get("unset") {
                None => Vec::new(),
                Some(Value::Array(names)) => {
                    let mut out = Vec::with_capacity(names.len());
                    for name in names {
                        match name.as_str() {
                            Some(name) if !name.is_empty() => out.push(name.to_owned()),
                            _ => {
                                return Err(invalid_operation(
                                    "operation \"set_attrs\" carries a non-string entry in `unset`",
                                ));
                            }
                        }
                    }
                    out
                }
                Some(_) => {
                    return Err(invalid_operation(
                        "operation \"set_attrs\" needs array `unset`",
                    ));
                }
            };
            if set.is_empty() && unset.is_empty() {
                return Err(invalid_operation(
                    "operation \"set_attrs\" must carry `set`, `unset` or both",
                ));
            }
            Ok(Operation::SetAttrs {
                id: id("id")?,
                set,
                unset,
            })
        }
        "replace_content" => {
            let content = object
                .get("content")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    invalid_operation("operation \"replace_content\" needs array `content`")
                })?;
            for node in content {
                if !node.is_object() {
                    return Err(invalid_node(
                        "operation \"replace_content\" carries a content node that is not a JSON object",
                    ));
                }
            }
            Ok(Operation::ReplaceContent {
                id: id("id")?,
                content: content.clone(),
            })
        }
        other => Err(invalid_operation(format!(
            "unknown operation {other:?} (supported: replace, delete, insert_before, insert_after, append_child, set_attrs, replace_content; set_text and move are not in v1)"
        ))),
    }
}

// ---------------------------------------------------------------------------
// ID helpers (§4.4, §8.2)
// ---------------------------------------------------------------------------

/// Every `attrs.<id_key>` of the subtree, in document order.
fn collect_ids(node: &Value, attrs_key: &str, out: &mut Vec<String>) {
    if let Some(id) = node
        .get("attrs")
        .and_then(|attrs| attrs.get(attrs_key))
        .and_then(Value::as_str)
    {
        out.push(id.to_owned());
    }
    if let Some(children) = node.get("content").and_then(Value::as_array) {
        for child in children {
            collect_ids(child, attrs_key, out);
        }
    }
}

/// The JSON content-index path of the node carrying `id`, if any.
fn find_path(doc: &Value, attrs_key: &str, id: &str) -> Option<Vec<usize>> {
    fn walk(node: &Value, attrs_key: &str, id: &str, path: &mut Vec<usize>) -> Option<()> {
        let children = node.get("content").and_then(Value::as_array)?;
        for (index, child) in children.iter().enumerate() {
            path.push(index);
            if child
                .get("attrs")
                .and_then(|attrs| attrs.get(attrs_key))
                .and_then(Value::as_str)
                == Some(id)
            {
                return Some(());
            }
            if walk(child, attrs_key, id, path).is_some() {
                return Some(());
            }
            path.pop();
        }
        None
    }
    let mut path = Vec::new();
    walk(doc, attrs_key, id, &mut path)?;
    Some(path)
}

/// The node at a content-index path (paths come from `find_path` on the
/// same document, so the descent is always well-formed).
fn node_at<'a>(doc: &'a Value, path: &[usize]) -> &'a Value {
    let mut current = doc;
    for &index in path {
        current = &current["content"][index];
    }
    current
}

fn node_at_mut<'a>(doc: &'a mut Value, path: &[usize]) -> &'a mut Value {
    let mut current = doc;
    for &index in path {
        current = &mut current["content"][index];
    }
    current
}

/// The content array holding the node at `path` (`path` points at a block
/// inside it).
fn content_array_mut<'a>(doc: &'a mut Value, parent_path: &[usize]) -> &'a mut Vec<Value> {
    node_at_mut(doc, parent_path)
        .get_mut("content")
        .and_then(Value::as_array_mut)
        .expect("the resolved target lives in a content array")
}

/// Occurrence counts of every `attrs.<id_key>` in the document: the
/// incremental form of the live-ID set (SEC-4/PRF-1). A count map, not a
/// set, so that a source carrying a duplicate ID loses exactly one
/// occurrence per deleted/replaced block — verdicts stay identical to a
/// fresh full scan even for hand-written documents.
fn counts_of(ids: impl IntoIterator<Item = String>) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for id in ids {
        register_id(&mut counts, id);
    }
    counts
}

/// Drop one occurrence of `id` from the counts (subtree removed from the
/// working copy).
fn release_id(live_ids: &mut HashMap<String, usize>, id: &str) {
    if let Some(count) = live_ids.get_mut(id) {
        *count -= 1;
        if *count == 0 {
            live_ids.remove(id);
        }
    }
}

/// Register one occurrence of `id` in the counts (subtree written into the
/// working copy).
fn register_id(live_ids: &mut HashMap<String, usize>, id: String) {
    *live_ids.entry(id).or_insert(0) += 1;
}

/// Every input ID of the node is checked against the state (§8.2): an ID
/// that belongs to a block outside the allowed set is `ID_CONFLICT`, any
/// other input ID is `BLOCK_ID_IMMUTABLE` (the CLI assigns IDs).
fn check_input_ids(
    node: &Value,
    attrs_key: &str,
    allowed: &BTreeSet<String>,
    live_ids: &HashMap<String, usize>,
) -> Result<(), UmError> {
    let mut passed = Vec::new();
    collect_ids(node, attrs_key, &mut passed);
    for id in passed {
        if allowed.contains(&id) {
            continue;
        }
        if live_ids.contains_key(&id) {
            return Err(UmError::new(
                ErrorCode::IdConflict,
                format!(
                    "input id {id:?} belongs to another block of the note; IDs are managed by the CLI"
                ),
            )
            .with_hint(
                "Pass nodes without attrs.id — a replaced subtree may restate its own descendant IDs, everything new is assigned by the CLI.",
            ));
        }
        return Err(UmError::new(
            ErrorCode::BlockIdImmutable,
            format!(
                "input nodes must not carry attrs.{attrs_key} ({id:?}); IDs are managed by the CLI"
            ),
        )
        .with_hint(
            "Drop attrs.id from the input node — new addressable blocks receive an ID at commit.",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Engine (§8.3, §8.4): sequential application to the working copy
// ---------------------------------------------------------------------------

struct Engine<'a> {
    attrs_key: &'a str,
    addressable: &'a BTreeSet<String>,
    /// Live `attrs.<id_key>` occurrence counts of the working copy,
    /// maintained incrementally (SEC-4/PRF-1): one full walk at init, then
    /// every operation adjusts only its own subtree — no per-operation
    /// full scan of the document. Counts, not a set, keep the verdicts
    /// identical to a fresh scan even for a source that carries a
    /// duplicate ID.
    live_ids: HashMap<String, usize>,
    /// insert_after cursors per anchor id: how many nodes earlier
    /// operations of this package have already inserted directly after
    /// the anchor (§8.4 — groups must not reverse).
    after_cursor: HashMap<String, usize>,
}

impl<'a> Engine<'a> {
    fn new(
        attrs_key: &'a str,
        addressable: &'a BTreeSet<String>,
        live_ids: HashMap<String, usize>,
    ) -> Self {
        Self {
            attrs_key,
            addressable,
            live_ids,
            after_cursor: HashMap::new(),
        }
    }

    /// The live ID counts after the last applied operation — the same set
    /// a fresh full walk would produce over the document at that point
    /// (before `assign_missing_ids` adds the generated IDs).
    fn into_live_ids(self) -> HashMap<String, usize> {
        self.live_ids
    }

    fn resolve(&self, doc: &Value, id: &str) -> Result<Vec<usize>, UmError> {
        find_path(doc, self.attrs_key, id).ok_or_else(|| {
            UmError::new(
                ErrorCode::BlockNotFound,
                format!("block {id:?} is not in the note at the time of this operation"),
            )
            .with_hint(
                "IDs resolve against the current state of the working copy — re-run `insum block get` and rebuild the package. A deleted block fails every later operation on its ID.",
            )
            .with_details(json!({ "id": id }))
        })
    }

    fn apply(&mut self, doc: &mut Value, ops: &[Operation]) -> Result<(), UmError> {
        for (index, op) in ops.iter().enumerate() {
            self.apply_one(doc, op)
                .map_err(|err| annotate_failed_operation(err, index))?;
        }
        Ok(())
    }

    fn apply_one(&mut self, doc: &mut Value, op: &Operation) -> Result<(), UmError> {
        match op {
            Operation::Replace { id, node } => {
                let path = self.resolve(doc, id)?;
                // the old target subtree defines which descendant IDs may be
                // passed (§8.3 replace: IDs from the same subtree only)
                let mut old_ids = Vec::new();
                collect_ids(node_at(doc, &path), self.attrs_key, &mut old_ids);
                let allowed: BTreeSet<String> = old_ids.iter().cloned().collect();
                // the input root may restate the target's ID, never change it
                if let Some(passed) = node
                    .get("attrs")
                    .and_then(|attrs| attrs.get(self.attrs_key))
                    .and_then(Value::as_str)
                    && passed != id
                {
                    return Err(UmError::new(
                        ErrorCode::BlockIdImmutable,
                        format!(
                            "replace must keep the target block ID {id:?}; the input node carries {passed:?}"
                        ),
                    )
                    .with_hint(
                        "Drop attrs.id from the input node — replace preserves the target's ID itself.",
                    ));
                }
                // checks run BEFORE any mutation, so live_ids still counts
                // the old subtree — exactly the set a fresh scan would see
                check_input_ids(node, self.attrs_key, &allowed, &self.live_ids)?;
                // rebuild the root: input attrs minus any passed ID, plus the
                // target's ID when the new type stays addressable (§8.3)
                let new_type = node
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let mut new_node = node.clone();
                {
                    let object = new_node.as_object_mut().expect("replace node is an object");
                    let mut attrs = match object.get("attrs") {
                        Some(Value::Object(map)) => map.clone(),
                        Some(_) => {
                            return Err(invalid_node(
                                "the replace input node carries a non-object attrs",
                            ));
                        }
                        None => Map::new(),
                    };
                    attrs.remove(self.attrs_key);
                    if self.addressable.contains(&new_type) {
                        attrs.insert(self.attrs_key.to_owned(), Value::String(id.clone()));
                    }
                    object.insert("attrs".to_owned(), Value::Object(attrs));
                }
                // live_ids: old subtree out, rebuilt subtree in (a non-
                // addressable new root drops the target's ID)
                let mut new_ids = Vec::new();
                collect_ids(&new_node, self.attrs_key, &mut new_ids);
                for old in &old_ids {
                    release_id(&mut self.live_ids, old);
                }
                for new in new_ids {
                    register_id(&mut self.live_ids, new);
                }
                let index = *path.last().expect("path is non-empty");
                let array = content_array_mut(doc, &path[..path.len() - 1]);
                array[index] = new_node;
                Ok(())
            }
            Operation::Delete { id } => {
                let path = self.resolve(doc, id)?;
                let mut subtree_ids = Vec::new();
                collect_ids(node_at(doc, &path), self.attrs_key, &mut subtree_ids);
                let index = *path.last().expect("path is non-empty");
                let array = content_array_mut(doc, &path[..path.len() - 1]);
                array.remove(index);
                self.after_cursor.remove(id);
                for removed in subtree_ids {
                    release_id(&mut self.live_ids, &removed);
                }
                Ok(())
            }
            Operation::InsertBefore { id, nodes } => {
                let path = self.resolve(doc, id)?;
                let empty = BTreeSet::new();
                for node in nodes {
                    check_input_ids(node, self.attrs_key, &empty, &self.live_ids)?;
                }
                let index = *path.last().expect("path is non-empty");
                let array = content_array_mut(doc, &path[..path.len() - 1]);
                for (offset, node) in nodes.iter().enumerate() {
                    array.insert(index + offset, node.clone());
                }
                Ok(())
            }
            Operation::InsertAfter { id, nodes } => {
                let path = self.resolve(doc, id)?;
                let empty = BTreeSet::new();
                for node in nodes {
                    check_input_ids(node, self.attrs_key, &empty, &self.live_ids)?;
                }
                let index = *path.last().expect("path is non-empty");
                let offset = self.after_cursor.entry(id.clone()).or_insert(0);
                let array = content_array_mut(doc, &path[..path.len() - 1]);
                let position = (index + 1 + *offset).min(array.len());
                for (offset, node) in nodes.iter().enumerate() {
                    array.insert(position + offset, node.clone());
                }
                *offset += nodes.len();
                Ok(())
            }
            Operation::AppendChild { parent_id, nodes } => {
                let empty = BTreeSet::new();
                for node in nodes {
                    check_input_ids(node, self.attrs_key, &empty, &self.live_ids)?;
                }
                match parent_id {
                    None => {
                        let array = doc
                            .get_mut("content")
                            .and_then(Value::as_array_mut)
                            .expect("the document root carries a content array");
                        array.extend(nodes.iter().cloned());
                    }
                    Some(parent) => {
                        let path = self.resolve(doc, parent)?;
                        let node = node_at_mut(doc, &path);
                        // a target without a content slot gets an empty one;
                        // the schema check refuses atom types (§8.3)
                        if node.get("content").and_then(Value::as_array).is_none() {
                            node["content"] = Value::Array(Vec::new());
                        }
                        let array = node
                            .get_mut("content")
                            .and_then(Value::as_array_mut)
                            .expect("content slot just created");
                        array.extend(nodes.iter().cloned());
                    }
                }
                Ok(())
            }
            Operation::SetAttrs { id, set, unset } => {
                if set.contains_key(self.attrs_key) || unset.iter().any(|k| k == self.attrs_key) {
                    return Err(UmError::new(
                        ErrorCode::BlockIdImmutable,
                        format!(
                            "set_attrs must not touch attrs.{} — IDs are managed by the CLI",
                            self.attrs_key
                        ),
                    )
                    .with_hint(
                        "Change other attributes; the block ID is preserved automatically.",
                    ));
                }
                for key in set.keys() {
                    if unset.contains(key) {
                        return Err(invalid_operation(format!(
                            "attribute {key:?} cannot be set and unset by one operation"
                        )));
                    }
                }
                let path = self.resolve(doc, id)?;
                let node = node_at_mut(doc, &path);
                let object = node
                    .as_object_mut()
                    .ok_or_else(|| invalid_node("the set_attrs target is not a JSON object"))?;
                let attrs = match object.get_mut("attrs") {
                    Some(Value::Object(map)) => map,
                    Some(_) => {
                        return Err(invalid_node(
                            "the set_attrs target carries a non-object attrs",
                        ));
                    }
                    None => {
                        object.insert("attrs".to_owned(), Value::Object(Map::new()));
                        match object.get_mut("attrs") {
                            Some(Value::Object(map)) => map,
                            _ => unreachable!("just inserted an object"),
                        }
                    }
                };
                for (key, value) in set {
                    attrs.insert(key.clone(), value.clone());
                }
                for key in unset {
                    attrs.remove(key);
                }
                Ok(())
            }
            Operation::ReplaceContent { id, content } => {
                let path = self.resolve(doc, id)?;
                // the target's CURRENT content subtree defines the
                // preservable IDs (§8.3 replace_content); collected over a
                // borrow — the deep clone bought nothing but a redundant
                // walk (PRF-2), and the borrow ends before the mutation
                let mut subtree_ids = Vec::new();
                {
                    let empty: Vec<Value> = Vec::new();
                    let current = node_at(doc, &path)
                        .get("content")
                        .and_then(Value::as_array)
                        .unwrap_or(&empty);
                    for element in current {
                        collect_ids(element, self.attrs_key, &mut subtree_ids);
                    }
                }
                let allowed: BTreeSet<String> = subtree_ids.iter().cloned().collect();
                for element in content {
                    check_input_ids(element, self.attrs_key, &allowed, &self.live_ids)?;
                }
                // live_ids: current content out, new content in (the new
                // elements may restate IDs from the replaced content)
                let mut new_ids = Vec::new();
                for element in content {
                    collect_ids(element, self.attrs_key, &mut new_ids);
                }
                for removed in &subtree_ids {
                    release_id(&mut self.live_ids, removed);
                }
                for new in new_ids {
                    register_id(&mut self.live_ids, new);
                }
                let node = node_at_mut(doc, &path);
                node["content"] = Value::Array(content.clone());
                Ok(())
            }
        }
    }
}

/// Point a failed operation back at its position in the package: the message
/// names the index and details carry it, so the agent can locate the failing
/// entry without counting JSON members by hand.
fn annotate_failed_operation(mut err: UmError, index: usize) -> UmError {
    err.message = format!("operation {index}: {}", err.message);
    let mut details = err.details.take().unwrap_or_else(|| json!({}));
    details["operation_index"] = json!(index);
    err.details = Some(details);
    err
}

/// One addressable block created by the package: ID, pointer in the final
/// document, node type (§8.5).
#[derive(Debug, Clone)]
struct CreatedBlock {
    id: String,
    pointer: String,
    node_type: String,
}

/// The result of applying a package to one document generation: the final
/// document, the created/deleted ID bookkeeping and the validation verdict.
#[derive(Debug)]
struct Generation {
    doc: Value,
    operations_applied: usize,
    created: Vec<CreatedBlock>,
    deleted_ids: Vec<String>,
    warnings: Vec<Value>,
}

/// Apply the package to a copy of `doc_source` and validate the result:
/// assign IDs to the new addressable blocks (document order, §4.4), diff
/// the ID sets into created/deleted, then run the full applicable check
/// (structural + IDs + ProseMirror parity — §8.6 step 3, §10).
#[allow(clippy::too_many_arguments)]
fn build_generation(
    doc_source: &Value,
    package: &Package,
    rules: &IdRules,
    generator: &IdGenerator,
    addressable: &BTreeSet<String>,
    pm: &PmSchema,
    limits: &Limits,
) -> Result<Generation, UmError> {
    let attrs_key = rules.attrs_key();
    let mut doc = doc_source.clone();
    // one full walk for the initial ID snapshot: the before-set of the
    // created/deleted diff and the engine's incremental live counts
    // (SEC-4/PRF-1 — no per-operation full scans follow)
    let mut initial_ids = Vec::new();
    collect_ids(&doc, attrs_key, &mut initial_ids);
    let ids_before: BTreeSet<String> = initial_ids.iter().cloned().collect();

    let mut engine = Engine::new(attrs_key, addressable, counts_of(initial_ids));
    engine.apply(&mut doc, &package.operations)?;
    let operations_applied = package.operations.len();
    refuse_title_family_loss(doc_source, &doc)?;

    // deleted = IDs present before the package and gone after it
    // (delete ops, replaced subtrees, dropped replace_content content);
    // the after-set comes from the engine's incremental counts
    let ids_after_ops: BTreeSet<String> = engine.into_live_ids().into_keys().collect();
    let deleted_ids: Vec<String> = ids_before.difference(&ids_after_ops).cloned().collect();

    // assign IDs to the new addressable blocks, document order; a deleted
    // ID never comes back (§4.4: no intentional reuse)
    let mut used = ids_after_ops;
    used.extend(deleted_ids.iter().cloned());
    let mut created = Vec::new();
    assign_missing_ids(
        &mut doc,
        "",
        attrs_key,
        addressable,
        generator,
        &mut used,
        &mut created,
    )?;

    // full applicable validation of the RESULT (§8.6 step 3, §10): parity
    // errors map onto the operation-level registry codes
    let validation =
        validate_note_doc_with_schema(&doc, true, limits, Some(pm), Some(addressable))?;
    if !validation.valid() {
        return Err(validation_refusal(&validation.errors));
    }
    let warnings = validation
        .warnings
        .iter()
        .map(diag_to_warning)
        .collect::<Vec<_>>();

    Ok(Generation {
        doc,
        operations_applied,
        created,
        deleted_ids,
        warnings,
    })
}

/// Walk in document order; every addressable block without an ID receives
/// a generated one and joins the created list with its final pointer.
fn assign_missing_ids(
    node: &mut Value,
    pointer: &str,
    attrs_key: &str,
    addressable: &BTreeSet<String>,
    generator: &IdGenerator,
    used: &mut BTreeSet<String>,
    created: &mut Vec<CreatedBlock>,
) -> Result<(), UmError> {
    if let Some(object) = node.as_object_mut() {
        let node_type = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let has_id = object
            .get("attrs")
            .and_then(|attrs| attrs.get(attrs_key))
            .and_then(Value::as_str)
            .is_some();
        if addressable.contains(&node_type) && !has_id {
            let id = generator.generate(used)?;
            match object.get_mut("attrs") {
                Some(Value::Object(_)) => {}
                Some(_) => {
                    return Err(UmError::new(
                        ErrorCode::NoteDocInvalid,
                        format!(
                            "node at {pointer} has a non-object attrs; fix the data before writing"
                        ),
                    ));
                }
                None => {
                    object.insert("attrs".to_owned(), Value::Object(Map::new()));
                }
            }
            if let Some(Value::Object(attrs)) = object.get_mut("attrs") {
                attrs.insert(attrs_key.to_owned(), Value::String(id.clone()));
            }
            created.push(CreatedBlock {
                id,
                pointer: pointer.to_owned(),
                node_type,
            });
        }
    }
    if let Some(children) = node.get_mut("content").and_then(Value::as_array_mut) {
        for (index, child) in children.iter_mut().enumerate() {
            let child_pointer = format!("{pointer}/content/{index}");
            assign_missing_ids(
                child,
                &child_pointer,
                attrs_key,
                addressable,
                generator,
                used,
                created,
            )?;
        }
    }
    Ok(())
}

/// The source document was title-family (first root child `noteTitle`) and
/// the result is not: the artifact's `noteTitle block*` root rule rejects
/// such a document, but pmschema only WARNS on it
/// (`root_outside_artifact_scope` → SCHEMA_FAMILY_UNCOVERED), so without
/// this gate a delete/replace of the title (or an insert_before ahead of
/// it) would commit a document the artifact refuses. Sources that are not
/// title-family keep the legacy plain-page verdict (warning only).
/// The type of the first root child, or None when the content is empty.
fn first_root_child_type(doc: &Value) -> Option<&str> {
    doc.get("content")
        .and_then(Value::as_array)
        .and_then(|children| children.first())
        .and_then(|child| child.get("type"))
        .and_then(Value::as_str)
}

fn refuse_title_family_loss(doc_source: &Value, doc: &Value) -> Result<(), UmError> {
    if first_root_child_type(doc_source) != Some("noteTitle")
        || first_root_child_type(doc) == Some("noteTitle")
    {
        return Ok(());
    }
    Err(validation_refusal(&[Diag {
        code: "CONTENT_MISMATCH",
        message:
            "the package removes the leading noteTitle: the noteDoc content rule of this artifact requires the title block first"
                .to_owned(),
    }]))
}

/// Map a parity-validation refusal onto the operation-level registry codes
/// (§10.2, §11): content shape → `CONTENT_NOT_ALLOWED`, unknown types →
/// `UNSUPPORTED_NODE_TYPE`, node shapes → `INVALID_NODE`, the rest → the
/// structural code itself. The full diagnostic list travels in `details`.
fn validation_refusal(diagnostics: &[Diag]) -> UmError {
    let first = diagnostics.first();
    let code = match first.map(|diag| diag.code) {
        Some("CONTENT_MISMATCH") => ErrorCode::ContentNotAllowed,
        Some("UNKNOWN_NODE_TYPE") | Some("UNKNOWN_MARK_TYPE") => ErrorCode::UnsupportedNodeType,
        Some("ATTR_REQUIRED") | Some("MARK_NOT_ALLOWED") | Some("MARK_DUPLICATE") => {
            ErrorCode::InvalidNode
        }
        Some("BLOCK_ID_MISSING") => ErrorCode::BlockIdMissing,
        Some("BLOCK_ID_DUPLICATE") => ErrorCode::BlockIdDuplicate,
        Some("BLOCK_ID_INVALID") => ErrorCode::BlockIdInvalid,
        _ => ErrorCode::NoteDocInvalid,
    };
    let diagnostics_json: Vec<Value> = diagnostics.iter().map(diag_to_warning).collect();
    UmError::new(
        code,
        format!(
            "the resulting document does not satisfy the applicable schema: {}",
            first.map(|diag| diag.message.as_str()).unwrap_or_default()
        ),
    )
    .with_hint(
        "Fix the operation package so the final document matches the schema; the source file was not modified.",
    )
    .with_details(json!({ "diagnostics": diagnostics_json }))
}

fn diag_to_warning(diag: &Diag) -> Value {
    json!({ "code": diag.code, "message": diag.message })
}

// ---------------------------------------------------------------------------
// Schema applicability for writes (§4.7 — the schema-cli policy)
// ---------------------------------------------------------------------------

/// Resolve the artifact a write runs against: explicit `--schema` or the
/// plugin runtime artifact of the target's vault. The bundled fallback is
/// never evidence that writing is allowed; a failed hash check refuses too.
fn resolve_schema_for_write(
    path: &Path,
    schema: Option<&Path>,
    limits: &Limits,
) -> Result<crate::schema::ResolvedSchema, UmError> {
    let start = path.parent().unwrap_or_else(|| Path::new("."));
    let resolved = resolve(schema, start, limits)?;
    if resolved.source == SchemaSource::BundledFallback {
        return Err(UmError::new(
            ErrorCode::SchemaUnavailable,
            "only the bundled fallback schema is applicable here; apply refuses to write without the plugin runtime artifact",
        )
        .with_hint(
            "Run inside the vault (the plugin runtime artifact is found next to the plugin), or pass the artifact explicitly with --schema.",
        ));
    }
    if !resolved.hash_verified {
        return Err(UmError::new(
            ErrorCode::SchemaMismatch,
            "the schema artifact failed the hash check; apply refuses to write",
        )
        .with_hint(
            "Re-open the document in Obsidian so the plugin regenerates the runtime artifact, or pass a trusted artifact explicitly with --schema.",
        ));
    }
    Ok(resolved)
}

/// Write by ID requires the guarantee only the `features.block_ids` marker
/// carries (§4.6) — `migrate-ids` is its only writer.
fn block_ids_not_enabled(where_: &str) -> UmError {
    UmError::new(
        ErrorCode::BlockIdsNotEnabled,
        format!(
            "{where_} does not carry the features.block_ids marker; IDs are not guaranteed there"
        ),
    )
    .with_hint(
        "Run `insum migrate-ids` on the file first — it verifies and assigns IDs, then stamps the marker.",
    )
}

/// Revision guards (§8.1): the package base must equal the current
/// revision, and an `--if-match` prefix must still match it.
fn ensure_revision_match(package: &Package, current: &str, what: &str) -> Result<(), UmError> {
    if current != package.base_note_rev {
        return Err(revision_mismatch(&package.base_note_rev, current, what));
    }
    if let Some(prefix) = &package.if_match
        && !current.starts_with(prefix.as_str())
    {
        return Err(revision_mismatch(prefix, current, what));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// .um pipeline (§8.6)
// ---------------------------------------------------------------------------

/// `insum apply` on a `.um` container (§8.1, §8.6): plan (CAS + apply +
/// validate) shared by dry-run and commit; the commit takes the lock and
/// re-applies onto a changed generation while the target note itself is
/// unchanged (bounded by UM_MAX_COMMIT_RETRIES, then `WRITE_CONFLICT`).
pub fn apply_um(
    path: &Path,
    package_bytes: &[u8],
    if_match: Option<&str>,
    schema: Option<&Path>,
    dry_run: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    // §9.3: a symlink or non-regular source is refused before any work —
    // dry-run and commit refuse alike (plan == commit)
    ensure_writable_source(path)?;
    let resolved = resolve_schema_for_write(path, schema, limits)?;
    let pm = PmSchema::build(&resolved.artifact)?;
    let addressable = addressable_types(&resolved.artifact);
    let rules = IdRules::from_artifact(&resolved.artifact)?;
    let generator = IdGenerator::new(rules.clone());
    let package = parse_package(package_bytes, limits, true, if_match)?;
    let note_id = package
        .note_id
        .clone()
        .expect("parse_package required it for .um");

    // plan phase (pre-lock): open, CAS, apply, validate
    let mut container = UmContainer::open(path, limits)?;
    let file_rev_plan = container.file_rev()?;
    let descriptor = container.find_note(&note_id)?.clone();
    if !descriptor.block_ids_enabled() {
        return Err(block_ids_not_enabled(&format!("note {note_id:?}")));
    }
    let parsed = container.read_note_with_rev(&descriptor, limits)?;
    ensure_revision_match(&package, &parsed.note_rev, &format!("note {note_id:?}"))?;
    let plan = build_generation(
        &parsed.note.doc,
        &package,
        &rules,
        &generator,
        &addressable,
        &pm,
        limits,
    )?;

    // plan == commit (R3COR-1): the lock sidecar is named after the target,
    // so a run that would take the lock refuses here with the error the
    // lock phase would produce
    check_sidecar_name_fits(path)?;

    if dry_run {
        return Ok(dry_run_response(
            path,
            Some(note_id.as_str()),
            &plan,
            limits,
        ));
    }

    let lock = acquire_write_lock(path, "apply", &LockConfig::defaults())?;
    let outcome = commit_um_locked(
        path,
        &descriptor,
        &package,
        &plan,
        file_rev_plan,
        &rules,
        &generator,
        &addressable,
        &pm,
        limits,
    );
    match outcome {
        Ok(value) => {
            // best-effort: a release failure must not turn the already
            // successful write into an IO_ERROR; stale recovery covers a
            // leftover sidecar
            let _ = lock.release();
            Ok(value)
        }
        Err(err) => {
            let _ = lock.release();
            Err(err)
        }
    }
}

/// Lock-held commit of a `.um` apply: revision recheck with bounded
/// re-application (§8.6 steps 5–8), raw-copy rebuild, staged validation,
/// pre-replace verification, atomic rename (§9.3).
#[allow(clippy::too_many_arguments)]
fn commit_um_locked(
    path: &Path,
    descriptor: &crate::model::NoteDescriptor,
    package: &Package,
    plan: &Generation,
    file_rev_plan: String,
    rules: &IdRules,
    generator: &IdGenerator,
    addressable: &BTreeSet<String>,
    pm: &PmSchema,
    limits: &Limits,
) -> Result<Value, UmError> {
    let mut attempts: usize = 0;
    let mut file_rev_snapshot = file_rev_plan;
    let mut owned: Option<Generation> = None;
    loop {
        // fresh generation under the lock (M-1 discipline: everything below
        // reads from THIS open)
        let mut container = UmContainer::open(path, limits)?;
        let file_rev_now = container.file_rev()?;
        if file_rev_now != file_rev_snapshot {
            // §8.6 steps 7–8: the file moved on — re-apply onto the fresh
            // generation only while the TARGET note is unchanged; otherwise
            // the package base is stale and the CAS must fail
            attempts += 1;
            if attempts > limits.max_commit_retries {
                // WHY: `attempts` counts observed changes; the last one never
                // produced a re-apply, so the report must not overstate by one
                let re_applications = attempts.saturating_sub(1);
                return Err(UmError::new(
                    ErrorCode::WriteConflict,
                    format!(
                        "the file kept changing under the commit ({} re-applications, UM_MAX_COMMIT_RETRIES={})",
                        re_applications, limits.max_commit_retries
                    ),
                )
                .with_hint(
                    "Retry once the concurrent writer has finished; the package base_note_rev is still valid if the note itself did not change.",
                )
                .with_details(json!({
                    "expected_file_rev": file_rev_snapshot,
                    "actual_file_rev": file_rev_now,
                    "attempts": re_applications,
                })));
            }
            let fresh_descriptor = match container.find_note_by_path(&descriptor.path) {
                Ok(fresh) => fresh.clone(),
                // WHY: a note missing from the fresh manifest is a competitor's
                // move/delete of the TARGET — a stale package base (§8.6 step 7,
                // exit 7), not a lookup failure; genuine IO/torn errors from the
                // open/read calls above stay transparent
                Err(err) if err.code == ErrorCode::Internal => {
                    return Err(revision_mismatch(
                        &package.base_note_rev,
                        "<absent>",
                        "the target note",
                    ));
                }
                Err(err) => return Err(err),
            };
            let fresh = container.read_note_with_rev(&fresh_descriptor, limits)?;
            if fresh.note_rev != package.base_note_rev {
                return Err(revision_mismatch(
                    &package.base_note_rev,
                    &fresh.note_rev,
                    &format!("note {:?}", descriptor.id),
                ));
            }
            let rebuilt = build_generation(
                &fresh.note.doc,
                package,
                rules,
                generator,
                addressable,
                pm,
                limits,
            )?;
            owned = Some(rebuilt);
            file_rev_snapshot = file_rev_now;
            continue;
        }
        let current: &Generation = owned.as_ref().unwrap_or(plan);

        // the generation matches the plan: rebuild + stage + validate + replace
        let mut replacements = HashMap::new();
        replacements.insert(descriptor.path.clone(), payload_bytes(&current.doc)?);
        let (staged, file) = StagedFile::create(path)?;
        // the whole-file revision arrives from the tee-hash of the streaming
        // write (PRF-3) — no second full pass over the staged file
        let new_file_rev = rewrite_um_entries(&mut container, &replacements, file)?;

        // §12: the result passes validation BEFORE the rename — over the
        // exact staged bytes; they also yield the post-write note revision
        let new_note_rev = validate_staged_um(
            staged.path(),
            descriptor.path.as_str(),
            rules.attrs_key(),
            current,
            addressable,
            pm,
            limits,
        )?;
        let mode = match pre_replace_verification(path, &file_rev_snapshot, "the file") {
            // §8.6 steps 7–8 in the commit window: the file moved on AFTER the
            // loop-top recheck, but the TARGET note may be unchanged — re-enter
            // the CAS loop (the attempt counter bounds it; exhaustion reports
            // WRITE_CONFLICT). A .note never takes this path: any file change
            // there IS a target change (commit_note_locked keeps the mismatch).
            Err(err) if err.code == ErrorCode::NoteRevisionMismatch => continue,
            result => result?,
        };
        staged.commit_to(path, mode)?;

        return Ok(commit_response(
            path,
            Some(descriptor.id.as_str()),
            current,
            &package.base_note_rev,
            &new_note_rev,
            &file_rev_snapshot,
            &new_file_rev,
            limits,
        ));
    }
}

/// Validate the staged container (§12): the target payload passes the full
/// applicable check, and every created ID resolves at its planned pointer
/// with its planned type (the working copy and the staged bytes are the
/// same document). Returns the post-write `note_rev` of the staged note.
fn validate_staged_um(
    staged_path: &Path,
    payload_path: &str,
    attrs_key: &str,
    plan: &Generation,
    addressable: &BTreeSet<String>,
    pm: &PmSchema,
    limits: &Limits,
) -> Result<String, UmError> {
    let mut container = UmContainer::open(staged_path, limits)?;
    let staged_descriptor = container.find_note_by_path(payload_path)?.clone();
    let parsed = container.read_note_with_rev(&staged_descriptor, limits)?;
    let validation =
        validate_note_doc_with_schema(&parsed.note.doc, true, limits, Some(pm), Some(addressable))?;
    if !validation.valid() {
        return Err(validation_refusal(&validation.errors));
    }
    for created in &plan.created {
        let node = crate::pointer::resolve(&parsed.note.doc, &created.pointer)
            .ok()
            .cloned();
        let Some(node) = node else {
            return Err(UmError::internal(format!(
                "created block {} is missing at {} in the staged document",
                created.id, created.pointer
            )));
        };
        let staged_id = node
            .get("attrs")
            .and_then(|attrs| attrs.get(attrs_key))
            .and_then(Value::as_str);
        let staged_type = node.get("type").and_then(Value::as_str).unwrap_or("");
        if staged_id != Some(created.id.as_str()) || staged_type != created.node_type {
            return Err(UmError::internal(format!(
                "created block mismatch at {}: staged {:?}/{staged_type} != planned {}/{}",
                created.pointer,
                staged_id.unwrap_or("<none>"),
                created.id,
                created.node_type
            )));
        }
    }
    Ok(parsed.note_rev)
}

// ---------------------------------------------------------------------------
// .note pipeline
// ---------------------------------------------------------------------------

/// `insum apply` on a `.note` document. For `.note` the note revision and
/// the file revision are the same digest (the whole file), so the CAS and
/// the commit recheck collapse into one guard: any change of the file after
/// the plan is a target change (`NOTE_REVISION_MISMATCH`).
pub fn apply_note_file(
    path: &Path,
    package_bytes: &[u8],
    if_match: Option<&str>,
    schema: Option<&Path>,
    dry_run: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    ensure_writable_source(path)?;
    let resolved = resolve_schema_for_write(path, schema, limits)?;
    let pm = PmSchema::build(&resolved.artifact)?;
    let addressable = addressable_types(&resolved.artifact);
    let rules = IdRules::from_artifact(&resolved.artifact)?;
    let generator = IdGenerator::new(rules.clone());
    let package = parse_package(package_bytes, limits, false, if_match)?;

    // plan phase (pre-lock): open, CAS, apply, validate
    let (note_file, _raw) = NoteFile::open_with_raw(path, limits, true)?;
    let marked = note_file
        .note
        .features
        .as_ref()
        .and_then(|f| f.get("block_ids"))
        .and_then(Value::as_u64)
        == Some(1);
    if !marked {
        return Err(block_ids_not_enabled("the .note file"));
    }
    ensure_revision_match(&package, &note_file.note_rev, "the .note file")?;
    let plan = build_generation(
        &note_file.note.doc,
        &package,
        &rules,
        &generator,
        &addressable,
        &pm,
        limits,
    )?;
    // plan == commit (D-4 discipline of migrate): the commit rebuilds the
    // envelope from the fresh raw payload and refuses an envelope whose
    // `features` / `schema` fields are absent or not objects — the plan
    // refuses with the same code first (a green dry-run promises a green
    // commit)
    if let Some(features) = &note_file.features
        && !features.is_object()
    {
        return Err(envelope_field_shape_error("features"));
    }
    if marked {
        match &note_file.schema {
            Some(schema_field) if schema_field.is_object() => {}
            _ => return Err(envelope_field_shape_error("schema")),
        }
    }

    check_sidecar_name_fits(path)?;

    if dry_run {
        return Ok(dry_run_response(path, None, &plan, limits));
    }

    let lock = acquire_write_lock(path, "apply", &LockConfig::defaults())?;
    let outcome = commit_note_locked(
        path,
        &note_file.note_rev,
        note_file.note.envelope,
        &plan,
        limits,
    );
    match outcome {
        Ok(value) => {
            let _ = lock.release();
            Ok(value)
        }
        Err(err) => {
            let _ = lock.release();
            Err(err)
        }
    }
}

/// Lock-held commit of a `.note` apply: the fresh read must match the plan
/// generation (same digest — for `.note` any file change is a target
/// change), the envelope is rebuilt from the fresh raw bytes with unknown
/// fields preserved (§12), staged, atomically replaced.
fn commit_note_locked(
    path: &Path,
    expected_rev: &str,
    already_envelope: bool,
    plan: &Generation,
    limits: &Limits,
) -> Result<Value, UmError> {
    // PRF-4: the fresh bytes are read and hashed without a NoteFile parse.
    // The CAS gate below makes any byte change a refusal, so bytes that
    // pass it are exactly the planned generation the plan phase already
    // validated through NoteFile — one bounded parse of them (below) is
    // enough, the second full parse of the same bytes is gone.
    let (fresh_raw, fresh_rev) = NoteFile::read_raw_with_rev(path, limits)?;
    if fresh_rev != expected_rev {
        return Err(revision_mismatch(
            expected_rev,
            &fresh_rev,
            "the .note file",
        ));
    }
    // the envelope is built ON the fresh raw payload when one exists:
    // unknown top-level fields are preserved as values (§12 — anything that
    // cannot be preserved must abort the write, not be dropped silently)
    let mut envelope: Value = if already_envelope {
        let text = String::from_utf8(fresh_raw)
            .map_err(|err| UmError::internal(format!("envelope is not UTF-8: {err}")))?;
        crate::container::parse_json_limited(&text, limits.max_depth, ErrorCode::NoteJsonInvalid)?
    } else {
        json!({})
    };
    {
        let map = envelope
            .as_object_mut()
            .ok_or_else(|| UmError::internal("envelope root must be an object"))?;
        map.insert("format".to_owned(), json!("inscriptum-note"));
        map.insert("version".to_owned(), json!(1));
        match map.get_mut("features") {
            Some(Value::Object(features)) => {
                features.insert("block_ids".to_owned(), json!(1));
            }
            _ => return Err(envelope_field_shape_error("features")),
        }
        match map.get_mut("schema") {
            Some(Value::Object(_)) => {}
            _ => return Err(envelope_field_shape_error("schema")),
        }
        map.insert("document".to_owned(), plan.doc.clone());
    }
    let new_bytes = serde_json::to_vec(&envelope)
        .map_err(|err| UmError::internal(format!("envelope does not serialize: {err}")))?;

    let (staged, mut file) = StagedFile::create(path)?;
    std::io::Write::write_all(&mut file, &new_bytes).map_err(|err| {
        UmError::new(
            ErrorCode::IoError,
            format!("cannot write the staged file: {err}"),
        )
    })?;
    file.sync_all().map_err(|err| {
        UmError::new(
            ErrorCode::IoError,
            format!("cannot sync the staged file: {err}"),
        )
    })?;
    drop(file);

    // CONTRACT (§12): the result passes validation BEFORE the replace — the
    // staged bytes are re-read through the bounded reader so an oversized or
    // unparseable serialized result refuses before it can become the target
    // (parity with validate_staged_um on the .um path)
    NoteFile::open(staged.path(), limits, false)?;
    let mode = pre_replace_verification(path, expected_rev, "the .note file")?;
    staged.commit_to(path, mode)?;

    // for a .note the whole file IS the note: the post-write note revision
    // is the hash of the final bytes
    let new_rev = note_rev_for_file_bytes(&new_bytes);
    Ok(commit_response(
        path,
        None,
        plan,
        expected_rev,
        &new_rev,
        expected_rev,
        &new_rev,
        limits,
    ))
}

fn envelope_field_shape_error(field: &str) -> UmError {
    UmError::new(
        ErrorCode::NoteDocInvalid,
        format!("the envelope {field} field is not an object; fix the data before writing"),
    )
}

// ---------------------------------------------------------------------------
// Response shapes (§8.5) with the output budget (§5.5, §6.5)
// ---------------------------------------------------------------------------

/// Dry-run response: the package validated against the current state, no
/// file effect, no promised final IDs (§8.5).
fn dry_run_response(
    path: &Path,
    note_id: Option<&str>,
    plan: &Generation,
    limits: &Limits,
) -> Value {
    let response = json!({
        "api_version": 1,
        "applied": false,
        "dry_run": true,
        "path": path.display().to_string(),
        "note_id": note_id.map(|id| Value::String(id.to_owned())).unwrap_or(Value::Null),
        "operations_validated": plan.operations_applied,
        "would_create_count": plan.created.len(),
        "would_delete_ids": plan
            .deleted_ids
            .iter()
            .map(|id| Value::String(id.clone()))
            .collect::<Vec<_>>(),
        "warnings": plan.warnings,
    });
    budget_response(response, plan, limits, true)
}

/// Commit response (§8.5): revisions before/after, applied count, the full
/// created/deleted bookkeeping and the parity warnings.
#[allow(clippy::too_many_arguments)]
fn commit_response(
    path: &Path,
    note_id: Option<&str>,
    plan: &Generation,
    old_note_rev: &str,
    new_note_rev: &str,
    old_file_rev: &str,
    new_file_rev: &str,
    limits: &Limits,
) -> Value {
    let response = json!({
        "api_version": 1,
        "applied": true,
        "dry_run": false,
        "path": path.display().to_string(),
        "note_id": note_id.map(|id| Value::String(id.to_owned())).unwrap_or(Value::Null),
        "old_note_rev": old_note_rev,
        "new_note_rev": new_note_rev,
        "old_file_rev": old_file_rev,
        "new_file_rev": new_file_rev,
        "operations_applied": plan.operations_applied,
        "created_ids": plan
            .created
            .iter()
            .map(|block| {
                json!({
                    "id": block.id,
                    "pointer": block.pointer,
                    "type": block.node_type,
                })
            })
            .collect::<Vec<_>>(),
        "deleted_ids": plan
            .deleted_ids
            .iter()
            .map(|id| Value::String(id.clone()))
            .collect::<Vec<_>>(),
        "warnings": plan.warnings,
    });
    budget_response(response, plan, limits, false)
}

/// Fit the response into the output budget (§5.5, §6.5): the ID lists are
/// truncated with explicit totals and an OUTPUT_BUDGET warning — never
/// silently. The write itself always covered every operation.
fn budget_response(
    mut response: Value,
    plan: &Generation,
    limits: &Limits,
    dry_run: bool,
) -> Value {
    let budget = limits.output_bytes(None);
    let mut used = envelope_overhead(
        Path::new(response["path"].as_str().unwrap_or(".")),
        response.get("note_id").and_then(Value::as_str),
    );

    let fit = |entries: &[Value], used: &mut usize| -> (Vec<Value>, usize) {
        let mut kept = Vec::with_capacity(entries.len());
        for entry in entries {
            let size = serde_json::to_string(entry).map_or(128, |s| s.len() + 1);
            if !kept.is_empty() && *used + size > budget {
                break;
            }
            *used += size;
            kept.push(entry.clone());
        }
        let dropped = entries.len() - kept.len();
        (kept, dropped)
    };

    let created_json: Vec<Value> = plan
        .created
        .iter()
        .map(|block| {
            json!({
                "id": block.id,
                "pointer": block.pointer,
                "type": block.node_type,
            })
        })
        .collect();
    let deleted_json: Vec<Value> = plan
        .deleted_ids
        .iter()
        .map(|id| Value::String(id.clone()))
        .collect();

    let created_dropped: usize = if dry_run {
        // a dry-run carries only the count (§8.5: final IDs are not promised)
        0
    } else {
        let (kept, dropped) = fit(&created_json, &mut used);
        response["created_ids"] = Value::Array(kept);
        dropped
    };
    let deleted_dropped = {
        let list_name = if dry_run {
            "would_delete_ids"
        } else {
            "deleted_ids"
        };
        let (kept, dropped) = fit(&deleted_json, &mut used);
        response[list_name] = Value::Array(kept);
        dropped
    };

    let truncated = created_dropped > 0 || deleted_dropped > 0;
    if truncated {
        response["truncated"] = json!(true);
        if created_dropped > 0 {
            response["created_ids_total"] = json!(plan.created.len());
        }
        if deleted_dropped > 0 {
            response["deleted_ids_total"] = json!(plan.deleted_ids.len());
        }
        let mut warnings = response
            .get("warnings")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        warnings.push(json!({
            "code": "OUTPUT_BUDGET",
            "message": format!(
                "the id lists hold {}/{} created and {}/{} deleted entries to fit the output budget; the operation itself covered every operation — re-read the note with `insum tree` to enumerate the final IDs",
                created_json.len() - created_dropped,
                created_json.len(),
                deleted_json.len() - deleted_dropped,
                deleted_json.len(),
            ),
        }));
        response["warnings"] = Value::Array(warnings);
    }
    response
}

/// Response envelope overhead measured over the largest possible skeleton
/// (committed, truncated, both totals) — the budget must hold for whichever
/// shape is finally emitted (§6.5, same discipline as migrate's D-1).
fn envelope_overhead(path: &Path, note_id: Option<&str>) -> usize {
    let skeleton = json!({
        "api_version": 1,
        "applied": true,
        "dry_run": false,
        "path": path.display().to_string(),
        "note_id": note_id.map(|id| Value::String(id.to_owned())).unwrap_or(Value::Null),
        "old_note_rev": format!("sha256:{}", "0".repeat(64)),
        "new_note_rev": format!("sha256:{}", "0".repeat(64)),
        "old_file_rev": format!("sha256:{}", "0".repeat(64)),
        "new_file_rev": format!("sha256:{}", "0".repeat(64)),
        "operations_applied": 0,
        "created_ids": [],
        "created_ids_total": 0,
        "deleted_ids": [],
        "deleted_ids_total": 0,
        "operations_validated": 0,
        "would_create_count": 0,
        "would_delete_ids": [],
        "truncated": true,
        "warnings": [{
            "code": "OUTPUT_BUDGET",
            "message": "the id lists hold 0 of 0 entries to fit the output budget; the operation itself covered every operation — re-read the note with `insum tree` to enumerate the final IDs",
        }],
    });
    serde_json::to_string(&skeleton).map_or(1024, |s| s.len() + 16)
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn usage(message: impl Into<String>) -> UmError {
    UmError::new(ErrorCode::Usage, message)
        .with_hint("Run `insum apply --help` for the package format.")
}

fn invalid_operation(message: impl Into<String>) -> UmError {
    UmError::new(ErrorCode::InvalidOperation, message).with_hint(
        "Check the operation against the package format (`insum apply --help`): replace, delete, insert_before, insert_after, append_child, set_attrs, replace_content.",
    )
}

fn invalid_node(message: impl Into<String>) -> UmError {
    UmError::new(ErrorCode::InvalidNode, message).with_hint(
        "Copy the node shape from `insum schema --type <node>` or a neighboring block of the same type; the source file was not modified.",
    )
}

fn limit_exceeded(message: String) -> UmError {
    UmError::new(ErrorCode::LimitExceeded, message)
}

fn is_full_rev(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Normalize `--if-match`: "sha256:" + at least 16 hex characters,
/// lowercased (revisions are emitted lowercase; the prefix match is
/// case-insensitive for the agent's convenience and deterministic).
fn normalized_prefix(value: &str) -> Result<String, UmError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(usage(
            "--if-match must look like sha256:<hex> with at least 16 hex characters",
        ));
    };
    if hex.len() < 16 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(usage(
            "--if-match must carry at least 16 hex characters after sha256:",
        ));
    }
    Ok(format!("sha256:{}", hex.to_ascii_lowercase()))
}

// ---------------------------------------------------------------------------
// Race-style criteria (38–40) as in-process tests: the black-box CLI cannot
// mutate the file between the plan and the commit.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::LazyLock;

    /// Live-ID occurrence counts for [`Engine::new`], from one walk over
    /// the working copy.
    fn id_counts(doc: &Value, attrs_key: &str) -> HashMap<String, usize> {
        let mut ids = Vec::new();
        collect_ids(doc, attrs_key, &mut ids);
        counts_of(ids)
    }

    /// Self-hashed test artifact (same vocabulary as the CLI contract tests).
    static TEST_ARTIFACT: LazyLock<Value> = LazyLock::new(|| {
        let mut raw = serde_json::json!({
            "schema_id": "inscriptum",
            "schema_version": 1,
            "schema_hash": "",
            "top_node": "noteDoc",
            "validation_capability": "partial",
            "id_rules": {
                "attrs_key": "id",
                "pattern": "^[a-z0-9]{8,12}$",
                "recommended_length": 10
            },
            "nodes": {
                "noteDoc": {"content": "noteTitle block*", "groups": [], "attrs": {},
                            "marks": [], "addressable": false, "inline_content": false,
                            "searchable": false},
                "noteTitle": {"content": "inline*", "groups": ["block"],
                              "attrs": {"id": {"default": null}}, "marks": [],
                              "addressable": true, "inline_content": true, "searchable": true},
                "paragraph": {"content": "inline*", "groups": ["block"],
                              "attrs": {"id": {"default": null}}, "marks": [],
                              "addressable": true, "inline_content": true, "searchable": true},
                "text": {"content": null, "groups": ["inline"], "attrs": {},
                         "marks": [], "addressable": false, "inline_content": true,
                         "searchable": false}
            },
            "marks": {}
        });
        raw["schema_hash"] = json!(crate::schema::compute_hash(&raw));
        raw
    });

    struct Harness {
        /// holds the temp directory alive for `path`
        _dir: tempfile::TempDir,
        path: PathBuf,
        note1: String,
        note2: String,
        pm: PmSchema,
        addressable: BTreeSet<String>,
        rules: IdRules,
        generator: IdGenerator,
    }

    impl Harness {
        fn new() -> Self {
            let artifact = crate::schema::SchemaArtifact::parse(TEST_ARTIFACT.clone()).unwrap();
            let pm = PmSchema::build(&artifact).unwrap();
            let addressable = addressable_types(&artifact);
            let rules = IdRules::from_artifact(&artifact).unwrap();
            let generator = IdGenerator::new(rules.clone());
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("race.um");
            let doc1 = serde_json::json!({
                "type": "noteDoc",
                "content": [
                    {"type": "noteTitle", "attrs": {"id": "t1tl2e3h4e"}, "content": [{"type": "text", "text": "Alpha"}]},
                    {"type": "paragraph", "attrs": {"id": "para1a2b3c"}, "content": [{"type": "text", "text": "one"}]},
                    {"type": "paragraph", "attrs": {"id": "para4d5e6f"}, "content": [{"type": "text", "text": "two"}]}
                ]
            });
            let doc2 = serde_json::json!({
                "type": "noteDoc",
                "content": [
                    {"type": "paragraph", "attrs": {"id": "other9para"}, "content": [{"type": "text", "text": "other"}]}
                ]
            });
            insum_test_utils::write_um(
                &path,
                &[
                    insum_test_utils::FixtureNote::title_page(
                        "01RACENOTE000000000000000A",
                        "Alpha",
                        "first",
                        &doc1["content"].as_array().unwrap()[1..],
                    ),
                    insum_test_utils::FixtureNote::plain_page(
                        "01RACENOTE000000000000000B",
                        "Beta",
                        &[doc2["content"][0].clone()],
                    ),
                ],
            )
            .unwrap();
            Self {
                _dir: dir,
                path,
                note1: "01RACENOTE000000000000000A".to_owned(),
                note2: "01RACENOTE000000000000000B".to_owned(),
                pm,
                addressable,
                rules,
                generator,
            }
        }

        fn limits(&self) -> Limits {
            Limits::defaults()
        }

        /// Plan an append onto note1 from the CURRENT file state; returns the
        /// package, the plan and the pre-lock file revision.
        fn plan_append(&self, limits: &Limits) -> (Package, Generation, String) {
            let mut container = UmContainer::open(&self.path, limits).unwrap();
            let file_rev = container.file_rev().unwrap();
            let descriptor = container.find_note(&self.note1).unwrap().clone();
            let parsed = container.read_note_with_rev(&descriptor, limits).unwrap();
            let package = Package {
                note_id: Some(self.note1.clone()),
                base_note_rev: parsed.note_rev.clone(),
                if_match: None,
                operations: vec![Operation::AppendChild {
                    parent_id: None,
                    nodes: vec![serde_json::json!({
                        "type": "paragraph",
                        "attrs": {},
                        "content": [{"type": "text", "text": "raced in"}]
                    })],
                }],
            };
            let plan = build_generation(
                &parsed.note.doc,
                &package,
                &self.rules,
                &self.generator,
                &self.addressable,
                &self.pm,
                limits,
            )
            .unwrap();
            (package, plan, file_rev)
        }

        /// Overwrite note2's payload in the archive (a non-cooperative
        /// concurrent writer touching ANOTHER note).
        fn mutate_other_note(&self) {
            let limits = self.limits();
            let mut container = UmContainer::open(&self.path, &limits).unwrap();
            let descriptor = container.find_note(&self.note2).unwrap().clone();
            let parsed = container.read_note_with_rev(&descriptor, &limits).unwrap();
            let mut doc = parsed.note.doc;
            doc["content"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "type": "paragraph",
                    "attrs": {"id": "racedinote"},
                    "content": [{"type": "text", "text": "concurrent"}]
                }));
            let bytes = serde_json::to_vec(&doc).unwrap();
            self.patch_entry(&descriptor.path, &bytes);
        }

        fn mutate_target_note(&self) {
            let limits = self.limits();
            let mut container = UmContainer::open(&self.path, &limits).unwrap();
            let descriptor = container.find_note(&self.note1).unwrap().clone();
            let parsed = container.read_note_with_rev(&descriptor, &limits).unwrap();
            let mut doc = parsed.note.doc;
            doc["content"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "type": "paragraph",
                    "attrs": {"id": "racedinp01"},
                    "content": [{"type": "text", "text": "target raced"}]
                }));
            let bytes = serde_json::to_vec(&doc).unwrap();
            self.patch_entry(&descriptor.path, &bytes);
        }

        fn patch_entry(&self, name: &str, bytes: &[u8]) {
            let mut archive = zip::ZipArchive::new(fs::File::open(&self.path).unwrap()).unwrap();
            let buffer = std::io::Cursor::new(Vec::new());
            let mut writer = zip::ZipWriter::new(buffer);
            for index in 0..archive.len() {
                let raw = archive.by_index_raw(index).unwrap();
                let entry_name = raw.name().unwrap().into_owned();
                if entry_name == name {
                    let last_modified = raw.last_modified().unwrap_or_default();
                    drop(raw);
                    writer
                        .start_file(
                            entry_name.as_str(),
                            zip::write::SimpleFileOptions::default()
                                .last_modified_time(last_modified),
                        )
                        .unwrap();
                    std::io::Write::write_all(&mut writer, bytes).unwrap();
                } else {
                    writer.raw_copy_file(raw).unwrap();
                }
            }
            let buffer = writer.finish().unwrap();
            fs::write(&self.path, buffer.into_inner()).unwrap();
        }

        /// Remove a note from the container entirely (a non-cooperative
        /// concurrent writer DELETING the target): rewrite the archive
        /// without the payload entry and without the manifest entry.
        fn remove_note(&self, note: &str) {
            let manifest = {
                let mut archive =
                    zip::ZipArchive::new(fs::File::open(&self.path).unwrap()).unwrap();
                let mut entry = archive.by_name("manifest.json").unwrap();
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
                let mut manifest: Value = serde_json::from_slice(&bytes).unwrap();
                manifest["notes"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|entry| entry["id"].as_str() != Some(note));
                serde_json::to_vec(&manifest).unwrap()
            };
            let skip = format!("notes/{note}.json");
            let mut archive = zip::ZipArchive::new(fs::File::open(&self.path).unwrap()).unwrap();
            let buffer = std::io::Cursor::new(Vec::new());
            let mut writer = zip::ZipWriter::new(buffer);
            for index in 0..archive.len() {
                let raw = archive.by_index_raw(index).unwrap();
                let entry_name = raw.name().unwrap().into_owned();
                if entry_name == skip {
                    drop(raw);
                    continue;
                }
                if entry_name == "manifest.json" {
                    let last_modified = raw.last_modified().unwrap_or_default();
                    drop(raw);
                    writer
                        .start_file(
                            entry_name.as_str(),
                            zip::write::SimpleFileOptions::default()
                                .last_modified_time(last_modified),
                        )
                        .unwrap();
                    std::io::Write::write_all(&mut writer, &manifest).unwrap();
                } else {
                    writer.raw_copy_file(raw).unwrap();
                }
            }
            let buffer = writer.finish().unwrap();
            fs::write(&self.path, buffer.into_inner()).unwrap();
        }

        fn read_note_payload(&self, note: &str) -> Value {
            let limits = self.limits();
            let mut container = UmContainer::open(&self.path, &limits).unwrap();
            let descriptor = container.find_note(note).unwrap().clone();
            let parsed = container.read_note_with_rev(&descriptor, &limits).unwrap();
            parsed.note.doc
        }
    }

    use std::path::PathBuf;

    /// Criterion 38: a change of ANOTHER note rebuilds the container under
    /// the lock without a false NOTE_REVISION_MISMATCH; the other note's
    /// concurrent change survives the rewrite.
    #[test]
    fn other_note_change_triggers_rebuild_not_mismatch() {
        let h = Harness::new();
        let limits = h.limits();
        let (package, plan, file_rev_plan) = h.plan_append(&limits);
        let descriptor = {
            let container = UmContainer::open(&h.path, &limits).unwrap();
            container.find_note(&h.note1).unwrap().clone()
        };
        // the concurrent writer lands AFTER the plan
        h.mutate_other_note();
        let other_before = h.read_note_payload(&h.note2);

        let lock =
            crate::lock::acquire_write_lock(&h.path, "apply", &LockConfig::defaults()).unwrap();
        let outcome = commit_um_locked(
            &h.path,
            &descriptor,
            &package,
            &plan,
            file_rev_plan,
            &h.rules,
            &h.generator,
            &h.addressable,
            &h.pm,
            &limits,
        );
        let _ = lock.release();
        let response = outcome.expect("the commit re-applies onto the fresh generation");
        assert_eq!(response["applied"], true);
        // the target got the appended block
        let target = h.read_note_payload(&h.note1);
        let last = &target["content"].as_array().unwrap().last().unwrap();
        assert_eq!(last["content"][0]["text"], "raced in");
        // the other note kept its concurrent change
        let other = h.read_note_payload(&h.note2);
        assert_eq!(
            other["content"].as_array().unwrap().len(),
            other_before["content"].as_array().unwrap().len(),
            "the concurrent change of the other note survived"
        );
    }

    /// Criterion 39: a change of the TARGET note under the lock refuses with
    /// NOTE_REVISION_MISMATCH and leaves the (concurrently changed) file
    /// untouched.
    #[test]
    fn target_note_change_under_the_lock_refuses() {
        let h = Harness::new();
        let limits = h.limits();
        let (package, plan, file_rev_plan) = h.plan_append(&limits);
        let descriptor = {
            let container = UmContainer::open(&h.path, &limits).unwrap();
            container.find_note(&h.note1).unwrap().clone()
        };
        h.mutate_target_note();
        let mutated = fs::read(&h.path).unwrap();

        let lock =
            crate::lock::acquire_write_lock(&h.path, "apply", &LockConfig::defaults()).unwrap();
        let outcome = commit_um_locked(
            &h.path,
            &descriptor,
            &package,
            &plan,
            file_rev_plan,
            &h.rules,
            &h.generator,
            &h.addressable,
            &h.pm,
            &limits,
        );
        let _ = lock.release();
        let err = outcome.expect_err("the stale target base refuses");
        assert_eq!(err.code, ErrorCode::NoteRevisionMismatch, "code: {err:?}");
        assert_eq!(
            fs::read(&h.path).unwrap(),
            mutated,
            "the refusal touched nothing"
        );
    }

    /// Criterion 40: the race window bounded by UM_MAX_COMMIT_RETRIES —
    /// with zero retries a changed file is WRITE_CONFLICT, never a silent
    /// overwrite.
    #[test]
    fn exhausted_retries_report_write_conflict() {
        let h = Harness::new();
        let mut limits = h.limits();
        limits.max_commit_retries = 0;
        let (package, plan, file_rev_plan) = h.plan_append(&limits);
        let descriptor = {
            let container = UmContainer::open(&h.path, &limits).unwrap();
            container.find_note(&h.note1).unwrap().clone()
        };
        h.mutate_other_note();
        let mutated = fs::read(&h.path).unwrap();

        let lock =
            crate::lock::acquire_write_lock(&h.path, "apply", &LockConfig::defaults()).unwrap();
        let outcome = commit_um_locked(
            &h.path,
            &descriptor,
            &package,
            &plan,
            file_rev_plan,
            &h.rules,
            &h.generator,
            &h.addressable,
            &h.pm,
            &limits,
        );
        let _ = lock.release();
        let err = outcome.expect_err("zero retries must conflict");
        assert_eq!(err.code, ErrorCode::WriteConflict, "code: {err:?}");
        // zero retries means zero re-applications were performed — the
        // report must not count the observed-but-never-re-applied change
        assert_eq!(
            err.details
                .as_ref()
                .and_then(|details| details.get("attempts"))
                .and_then(Value::as_u64),
            Some(0),
            "details: {err:?}"
        );
        assert!(
            err.message.contains("0 re-applications"),
            "message: {}",
            err.message
        );
        assert_eq!(fs::read(&h.path).unwrap(), mutated, "no silent overwrite");
    }

    /// The target note vanished from the fresh generation (a competitor
    /// deleted it): the recheck refuses with NOTE_REVISION_MISMATCH (exit 7,
    /// re-read-and-rebuild advice), not a lookup/internal error.
    #[test]
    fn target_note_removed_under_the_lock_is_revision_mismatch() {
        let h = Harness::new();
        let limits = h.limits();
        let (package, plan, file_rev_plan) = h.plan_append(&limits);
        let descriptor = {
            let container = UmContainer::open(&h.path, &limits).unwrap();
            container.find_note(&h.note1).unwrap().clone()
        };
        h.remove_note(&h.note1);

        let lock =
            crate::lock::acquire_write_lock(&h.path, "apply", &LockConfig::defaults()).unwrap();
        let outcome = commit_um_locked(
            &h.path,
            &descriptor,
            &package,
            &plan,
            file_rev_plan,
            &h.rules,
            &h.generator,
            &h.addressable,
            &h.pm,
            &limits,
        );
        let _ = lock.release();
        let err = outcome.expect_err("the removed target refuses");
        assert_eq!(err.code, ErrorCode::NoteRevisionMismatch, "code: {err:?}");
        assert_eq!(
            err.details
                .as_ref()
                .and_then(|details| details.get("actual"))
                .and_then(Value::as_str),
            Some("<absent>"),
            "details: {err:?}"
        );
    }

    /// Engine-level: two insert_after groups on one anchor keep the
    /// operation order (§8.4, criterion 33) at the engine layer.
    #[test]
    fn insert_after_cursors_keep_the_group_order() {
        let h = Harness::new();
        let limits = h.limits();
        let mut container = UmContainer::open(&h.path, &limits).unwrap();
        let descriptor = container.find_note(&h.note1).unwrap().clone();
        let parsed = container.read_note_with_rev(&descriptor, &limits).unwrap();
        let mut doc = parsed.note.doc.clone();
        let mut engine = Engine::new(
            h.rules.attrs_key(),
            &h.addressable,
            id_counts(&doc, h.rules.attrs_key()),
        );
        engine
            .apply(
                &mut doc,
                &[
                    Operation::InsertAfter {
                        id: "para1a2b3c".to_owned(),
                        nodes: vec![
                            serde_json::json!({"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "a1"}]}),
                            serde_json::json!({"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "a2"}]}),
                        ],
                    },
                    Operation::InsertAfter {
                        id: "para1a2b3c".to_owned(),
                        nodes: vec![
                            serde_json::json!({"type": "paragraph", "attrs": {}, "content": [{"type": "text", "text": "b1"}]}),
                        ],
                    },
                ],
            )
            .unwrap();
        let texts: Vec<String> = doc["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| {
                block["content"][0]["text"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned()
            })
            .collect();
        let ia1 = texts.iter().position(|t| t == "a1").unwrap();
        let ia2 = texts.iter().position(|t| t == "a2").unwrap();
        let ib1 = texts.iter().position(|t| t == "b1").unwrap();
        assert!(ia1 < ia2 && ia2 < ib1, "group order kept: {texts:?}");
    }

    /// The wire rule (§8.2): a replace whose input re-states the target id
    /// is accepted; a different id is BLOCK_ID_IMMUTABLE.
    #[test]
    fn replace_input_id_rules() {
        let h = Harness::new();
        let limits = h.limits();
        let mut container = UmContainer::open(&h.path, &limits).unwrap();
        let descriptor = container.find_note(&h.note1).unwrap().clone();
        let parsed = container.read_note_with_rev(&descriptor, &limits).unwrap();
        let mut doc = parsed.note.doc.clone();
        let mut engine = Engine::new(
            h.rules.attrs_key(),
            &h.addressable,
            id_counts(&doc, h.rules.attrs_key()),
        );

        // restate the id: fine
        engine
            .apply_one(
                &mut doc,
                &Operation::Replace {
                    id: "para1a2b3c".to_owned(),
                    node: serde_json::json!({
                        "type": "paragraph",
                        "attrs": {"id": "para1a2b3c"},
                        "content": [{"type": "text", "text": "replaced"}]
                    }),
                },
            )
            .unwrap();
        // the target sits after noteTitle + noteSummary in the fixture
        let position = doc["content"]
            .as_array()
            .unwrap()
            .iter()
            .position(|block| {
                block
                    .get("attrs")
                    .and_then(|attrs| attrs.get("id"))
                    .and_then(Value::as_str)
                    == Some("para1a2b3c")
            })
            .expect("the target stays in the document");
        let node = doc["content"][position].clone();
        assert_eq!(node["attrs"]["id"], "para1a2b3c");
        assert_eq!(node["content"][0]["text"], "replaced");

        // a different id: refused
        let err = engine
            .apply_one(
                &mut doc,
                &Operation::Replace {
                    id: "para1a2b3c".to_owned(),
                    node: serde_json::json!({
                        "type": "paragraph",
                        "attrs": {"id": "other000000"},
                        "content": [{"type": "text", "text": "hijack"}]
                    }),
                },
            )
            .expect_err("a foreign root id refuses");
        assert_eq!(err.code, ErrorCode::BlockIdImmutable, "code: {err:?}");
    }

    /// A failed operation names its position in the package (message and
    /// `operation_index` detail) — here op 1 targets an ID op 0 deleted.
    #[test]
    fn engine_failure_names_the_operation_index() {
        let h = Harness::new();
        let limits = h.limits();
        let mut container = UmContainer::open(&h.path, &limits).unwrap();
        let descriptor = container.find_note(&h.note1).unwrap().clone();
        let parsed = container.read_note_with_rev(&descriptor, &limits).unwrap();
        let mut doc = parsed.note.doc.clone();
        let mut engine = Engine::new(
            h.rules.attrs_key(),
            &h.addressable,
            id_counts(&doc, h.rules.attrs_key()),
        );
        let err = engine
            .apply(
                &mut doc,
                &[
                    Operation::Delete {
                        id: "para1a2b3c".to_owned(),
                    },
                    Operation::Delete {
                        id: "para1a2b3c".to_owned(),
                    },
                ],
            )
            .expect_err("the second op hits a deleted ID");
        assert_eq!(err.code, ErrorCode::BlockNotFound, "code: {err:?}");
        assert_eq!(
            err.details
                .as_ref()
                .and_then(|details| details.get("operation_index"))
                .and_then(Value::as_u64),
            Some(1),
            "details: {err:?}"
        );
        assert!(
            err.message.contains("operation 1"),
            "message: {}",
            err.message
        );
    }

    /// SEC-4: the incremental live-ID set tracks deletes — the removed ID
    /// fails later resolution (BLOCK_NOT_FOUND) and its restatement on
    /// later input is BLOCK_ID_IMMUTABLE (a foreign ID), while surviving
    /// IDs keep resolving across the shifted sibling indexes and still
    /// conflict on restatement.
    #[test]
    fn delete_updates_the_incremental_live_ids() {
        let h = Harness::new();
        let limits = h.limits();
        let mut container = UmContainer::open(&h.path, &limits).unwrap();
        let descriptor = container.find_note(&h.note1).unwrap().clone();
        let parsed = container.read_note_with_rev(&descriptor, &limits).unwrap();
        let mut doc = parsed.note.doc.clone();
        let mut engine = Engine::new(
            h.rules.attrs_key(),
            &h.addressable,
            id_counts(&doc, h.rules.attrs_key()),
        );

        // a later operation on the deleted ID fails with BLOCK_NOT_FOUND
        let err = engine
            .apply(
                &mut doc,
                &[
                    Operation::Delete {
                        id: "para1a2b3c".to_owned(),
                    },
                    Operation::SetAttrs {
                        id: "para1a2b3c".to_owned(),
                        set: [("data".to_owned(), json!("x"))].into_iter().collect(),
                        unset: Vec::new(),
                    },
                ],
            )
            .expect_err("the deleted ID no longer resolves");
        assert_eq!(err.code, ErrorCode::BlockNotFound, "code: {err:?}");

        // a surviving block stays addressable after the sibling-index shift
        engine
            .apply_one(
                &mut doc,
                &Operation::SetAttrs {
                    id: "para4d5e6f".to_owned(),
                    set: [("data".to_owned(), json!("y"))].into_iter().collect(),
                    unset: Vec::new(),
                },
            )
            .expect("surviving IDs keep resolving after the delete");

        // the deleted ID left the live set: restating it is a foreign ID
        let err = engine
            .apply_one(
                &mut doc,
                &Operation::AppendChild {
                    parent_id: None,
                    nodes: vec![serde_json::json!({
                        "type": "paragraph",
                        "attrs": {"id": "para1a2b3c"},
                        "content": [{"type": "text", "text": "reborn"}]
                    })],
                },
            )
            .expect_err("the deleted ID is not a note ID anymore");
        assert_eq!(err.code, ErrorCode::BlockIdImmutable, "code: {err:?}");

        // a surviving ID still conflicts on restatement
        let err = engine
            .apply_one(
                &mut doc,
                &Operation::AppendChild {
                    parent_id: None,
                    nodes: vec![serde_json::json!({
                        "type": "paragraph",
                        "attrs": {"id": "para4d5e6f"},
                        "content": [{"type": "text", "text": "hijack"}]
                    })],
                },
            )
            .expect_err("a surviving ID stays in the live set");
        assert_eq!(err.code, ErrorCode::IdConflict, "code: {err:?}");
    }

    /// SEC-4: insertions shift sibling indexes, yet later operations on
    /// other IDs keep resolving, and the live-ID checks stay exact:
    /// restating a surviving ID conflicts, a fresh ID stays immutable.
    #[test]
    fn inserts_keep_later_operations_and_live_ids_valid() {
        let h = Harness::new();
        let limits = h.limits();
        let mut container = UmContainer::open(&h.path, &limits).unwrap();
        let descriptor = container.find_note(&h.note1).unwrap().clone();
        let parsed = container.read_note_with_rev(&descriptor, &limits).unwrap();
        let mut doc = parsed.note.doc.clone();
        let mut engine = Engine::new(
            h.rules.attrs_key(),
            &h.addressable,
            id_counts(&doc, h.rules.attrs_key()),
        );

        engine
            .apply(
                &mut doc,
                &[
                    Operation::InsertBefore {
                        id: "para4d5e6f".to_owned(),
                        nodes: vec![serde_json::json!({
                            "type": "paragraph",
                            "attrs": {},
                            "content": [{"type": "text", "text": "before"}]
                        })],
                    },
                    Operation::InsertAfter {
                        id: "para1a2b3c".to_owned(),
                        nodes: vec![serde_json::json!({
                            "type": "paragraph",
                            "attrs": {},
                            "content": [{"type": "text", "text": "after"}]
                        })],
                    },
                ],
            )
            .expect("the inserts apply");

        // later operations on other IDs still resolve across the shifts
        engine
            .apply_one(
                &mut doc,
                &Operation::SetAttrs {
                    id: "para4d5e6f".to_owned(),
                    set: [("data".to_owned(), json!("z"))].into_iter().collect(),
                    unset: Vec::new(),
                },
            )
            .expect("post-insert paths stay valid");

        // replace restating its own ID is accepted
        engine
            .apply_one(
                &mut doc,
                &Operation::Replace {
                    id: "para1a2b3c".to_owned(),
                    node: serde_json::json!({
                        "type": "paragraph",
                        "attrs": {"id": "para1a2b3c"},
                        "content": [{"type": "text", "text": "replaced"}]
                    }),
                },
            )
            .expect("replace keeps restating its own ID");

        // restating another live ID conflicts
        let err = engine
            .apply_one(
                &mut doc,
                &Operation::AppendChild {
                    parent_id: None,
                    nodes: vec![serde_json::json!({
                        "type": "paragraph",
                        "attrs": {"id": "para4d5e6f"},
                        "content": [{"type": "text", "text": "hijack"}]
                    })],
                },
            )
            .expect_err("a surviving ID stays in the live set");
        assert_eq!(err.code, ErrorCode::IdConflict, "code: {err:?}");

        // a fresh ID stays immutable
        let err = engine
            .apply_one(
                &mut doc,
                &Operation::AppendChild {
                    parent_id: None,
                    nodes: vec![serde_json::json!({
                        "type": "paragraph",
                        "attrs": {"id": "freshid0001"},
                        "content": [{"type": "text", "text": "new"}]
                    })],
                },
            )
            .expect_err("new IDs are assigned by the CLI");
        assert_eq!(err.code, ErrorCode::BlockIdImmutable, "code: {err:?}");
    }

    /// PRF-2: replace_content collects the preservable IDs over a borrow of
    /// the current content — restating an ID from elsewhere in the note is
    /// ID_CONFLICT, a fresh ID is BLOCK_ID_IMMUTABLE, ID-less content
    /// applies and lands in the working copy.
    #[test]
    fn replace_content_borrowed_subtree_id_rules() {
        let h = Harness::new();
        let limits = h.limits();
        let mut container = UmContainer::open(&h.path, &limits).unwrap();
        let descriptor = container.find_note(&h.note1).unwrap().clone();
        let parsed = container.read_note_with_rev(&descriptor, &limits).unwrap();
        let mut doc = parsed.note.doc.clone();
        let mut engine = Engine::new(
            h.rules.attrs_key(),
            &h.addressable,
            id_counts(&doc, h.rules.attrs_key()),
        );

        // restating a live ID from outside the target's current content
        let err = engine
            .apply_one(
                &mut doc,
                &Operation::ReplaceContent {
                    id: "para4d5e6f".to_owned(),
                    content: vec![serde_json::json!({
                        "type": "paragraph",
                        "attrs": {"id": "para1a2b3c"},
                        "content": [{"type": "text", "text": "stolen"}]
                    })],
                },
            )
            .expect_err("an ID from another block conflicts");
        assert_eq!(err.code, ErrorCode::IdConflict, "code: {err:?}");

        // a fresh ID is not acceptable either
        let err = engine
            .apply_one(
                &mut doc,
                &Operation::ReplaceContent {
                    id: "para4d5e6f".to_owned(),
                    content: vec![serde_json::json!({
                        "type": "paragraph",
                        "attrs": {"id": "freshid0001"},
                        "content": [{"type": "text", "text": "new"}]
                    })],
                },
            )
            .expect_err("new IDs are assigned by the CLI");
        assert_eq!(err.code, ErrorCode::BlockIdImmutable, "code: {err:?}");

        // ID-less content applies over the borrowed collection
        engine
            .apply_one(
                &mut doc,
                &Operation::ReplaceContent {
                    id: "para4d5e6f".to_owned(),
                    content: vec![
                        serde_json::json!({
                            "type": "paragraph",
                            "attrs": {},
                            "content": [{"type": "text", "text": "new content"}]
                        }),
                        serde_json::json!({
                            "type": "paragraph",
                            "attrs": {},
                            "content": [{"type": "text", "text": "second"}]
                        }),
                    ],
                },
            )
            .expect("ID-less content applies");
        let target = doc["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["attrs"]["id"] == "para4d5e6f")
            .expect("the target survives");
        assert_eq!(target["content"].as_array().unwrap().len(), 2);
    }

    /// A minimal title-family document over the test artifact's vocabulary
    /// (leading noteTitle with an ID, then one addressable block).
    fn titled_doc() -> Value {
        serde_json::json!({
            "type": "noteDoc",
            "content": [
                {"type": "noteTitle", "attrs": {"id": "t1tl2e3h4e"},
                 "content": [{"type": "text", "text": "Alpha"}]},
                {"type": "paragraph", "attrs": {"id": "para1a2b3c"},
                 "content": [{"type": "text", "text": "one"}]}
            ]
        })
    }

    /// Run `operations` against [`titled_doc`] through the plan pipeline.
    fn build_titled(
        h: &Harness,
        limits: &Limits,
        operations: Vec<Operation>,
    ) -> Result<Generation, UmError> {
        let package = Package {
            note_id: Some(h.note1.clone()),
            base_note_rev: format!("sha256:{}", "0".repeat(64)),
            if_match: None,
            operations,
        };
        build_generation(
            &titled_doc(),
            &package,
            &h.rules,
            &h.generator,
            &h.addressable,
            &h.pm,
            limits,
        )
    }

    /// A title-family source must not lose its leading noteTitle: a delete
    /// of the title (the artifact root rule `noteTitle block*` would reject
    /// the result) refuses as CONTENT_NOT_ALLOWED.
    #[test]
    fn deleting_the_title_refuses_as_content_not_allowed() {
        let h = Harness::new();
        let limits = h.limits();
        let err = build_titled(
            &h,
            &limits,
            vec![Operation::Delete {
                id: "t1tl2e3h4e".to_owned(),
            }],
        )
        .expect_err("removing the leading noteTitle refuses");
        assert_eq!(err.code, ErrorCode::ContentNotAllowed, "code: {err:?}");
    }

    /// insert_before ahead of the title displaces it from the root head —
    /// the same title-family loss, the same refusal.
    #[test]
    fn inserting_before_the_title_refuses_as_content_not_allowed() {
        let h = Harness::new();
        let limits = h.limits();
        let err = build_titled(
            &h,
            &limits,
            vec![Operation::InsertBefore {
                id: "t1tl2e3h4e".to_owned(),
                nodes: vec![serde_json::json!({
                    "type": "paragraph",
                    "attrs": {},
                    "content": [{"type": "text", "text": "ahead of the title"}]
                })],
            }],
        )
        .expect_err("displacing the leading noteTitle refuses");
        assert_eq!(err.code, ErrorCode::ContentNotAllowed, "code: {err:?}");
    }

    /// Appending AFTER the title keeps the family: the gate is not a blanket
    /// content veto on title-family documents.
    #[test]
    fn appending_after_the_title_stays_allowed() {
        let h = Harness::new();
        let limits = h.limits();
        let generation = build_titled(
            &h,
            &limits,
            vec![Operation::AppendChild {
                parent_id: None,
                nodes: vec![serde_json::json!({
                    "type": "paragraph",
                    "attrs": {},
                    "content": [{"type": "text", "text": "appended"}]
                })],
            }],
        )
        .expect("the title stays first");
        let content = generation.doc["content"].as_array().unwrap();
        assert_eq!(content.first().unwrap()["type"], "noteTitle");
        assert_eq!(content.last().unwrap()["content"][0]["text"], "appended");
    }

    /// A package nested deeper than UM_MAX_DEPTH keeps the container-level
    /// LIMIT_EXCEEDED verdict (exit 5) — not a package-envelope USAGE.
    #[test]
    fn package_over_depth_limit_is_limit_exceeded() {
        let limits = Limits::defaults();
        let depth = limits.max_depth + 10;
        let bytes = format!("{}1{}", "[".repeat(depth), "]".repeat(depth)).into_bytes();
        let err = parse_package(&bytes, &limits, true, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::LimitExceeded, "code: {err:?}");
    }

    /// Non-UTF-8 package bytes are an envelope violation: USAGE (exit 2),
    /// not INVALID_OPERATION.
    #[test]
    fn non_utf8_package_is_usage() {
        let limits = Limits::defaults();
        let bytes = vec![0x7B, 0xFF, 0x7D];
        let err = parse_package(&bytes, &limits, true, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage, "code: {err:?}");
    }

    /// plan == commit for an already-envelope .note whose `schema` key is
    /// missing: the dry-run refuses with the same code the commit produces
    /// (NOTE_DOC_INVALID) and the file is left untouched.
    #[test]
    fn envelope_without_schema_key_refuses_on_plan_like_commit() {
        let dir = tempfile::TempDir::new().unwrap();
        let artifact = dir.path().join("artifact.json");
        std::fs::write(&artifact, serde_json::to_vec(&*TEST_ARTIFACT).unwrap()).unwrap();
        let path = dir.path().join("doc.note");
        let envelope = serde_json::json!({
            "format": "inscriptum-note",
            "version": 1,
            "features": {"block_ids": 1},
            "document": {
                "type": "noteDoc",
                "content": [
                    {"type": "noteTitle", "attrs": {"id": "t1tl2e3h4e"},
                     "content": [{"type": "text", "text": "Alpha"}]},
                    {"type": "paragraph", "attrs": {"id": "para1a2b3c"},
                     "content": [{"type": "text", "text": "one"}]}
                ]
            }
        });
        std::fs::write(&path, envelope.to_string()).unwrap();
        let before = fs::read(&path).unwrap();
        let package = serde_json::json!({
            "api_version": 1,
            "note_id": Value::Null,
            "base_note_rev": note_rev_for_file_bytes(&before),
            "operations": [{
                "op": "append_child",
                "parent_id": null,
                "nodes": [{"type": "paragraph", "attrs": {},
                           "content": [{"type": "text", "text": "appended"}]}]
            }],
        });
        let bytes = serde_json::to_vec(&package).unwrap();
        let limits = Limits::defaults();

        let dry = apply_note_file(&path, &bytes, None, Some(&artifact), true, &limits)
            .expect_err("the plan refuses the schema-less envelope");
        assert_eq!(dry.code, ErrorCode::NoteDocInvalid, "code: {dry:?}");
        let commit = apply_note_file(&path, &bytes, None, Some(&artifact), false, &limits)
            .expect_err("the commit refuses with the same code");
        assert_eq!(commit.code, ErrorCode::NoteDocInvalid, "code: {commit:?}");
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "the refusal touched nothing"
        );
    }
}
