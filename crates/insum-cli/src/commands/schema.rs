//! `insum schema` — show the applicable schema artifact, its source and
//! fingerprint (§7.1), or one node type descriptor with `--type`.
//!
//! Read-only. The resolution order is fixed: explicit `--schema <file>`,
//! the plugin runtime artifact found by the vault rule (walking up from the
//! working directory), the bundled fallback. The fallback is always labeled
//! and is never evidence that writing is allowed.

use std::path::Path;

use serde_json::{Value, json};

use insum_core::schema::resolve;
use insum_core::{ErrorCode, Limits, UmError};

pub fn run(
    schema: Option<&Path>,
    type_name: Option<&str>,
    limits: &Limits,
) -> Result<Value, UmError> {
    let start = std::env::current_dir()
        .map_err(|err| UmError::new(ErrorCode::IoError, format!("cannot read cwd: {err}")))?;
    let resolved = resolve(schema, &start, limits)?;

    let mut warnings: Vec<Value> = Vec::new();
    if !resolved.hash_verified {
        warnings.push(json!({
            "code": "SCHEMA_MISMATCH",
            "message": "recomputed schema hash does not match the declared schema_hash (the artifact is stale or was edited)",
        }));
    }

    let mut out = json!({
        "api_version": 1,
        "schema": {
            "id": resolved.artifact.schema_id,
            "schema_version": resolved.artifact.schema_version,
            "schema_hash": resolved.artifact.schema_hash,
            "source": resolved.source.as_str(),
            "source_path": resolved.source_path.as_ref().map(|p| p.display().to_string()),
            "plugin_version": resolved.artifact.plugin_version,
            "validation_capability": resolved.artifact.validation_capability,
            "hash_verified": resolved.hash_verified,
            "warnings": warnings,
        },
    });

    if let Some(name) = type_name {
        let Some(spec) = resolved.artifact.nodes.get(name) else {
            let mut available: Vec<&String> = resolved.artifact.nodes.keys().collect();
            available.sort();
            return Err(UmError::new(
                ErrorCode::UnsupportedNodeType,
                format!("node type {name:?} is not in the applicable schema artifact"),
            )
            .with_details(json!({ "available": available }))
            .with_hint(
                "Run `insum schema` to see the artifact source, or pick a type from `available`.",
            ));
        };
        let mut node = json!({
            "name": name,
            "content": spec.content,
            "groups": spec.groups,
            "top_node": spec.top_node,
            "attrs": spec.attrs,
            "marks": spec.marks,
            "addressable": spec.addressable,
            "inline_content": spec.inline_content,
            "searchable": spec.searchable,
        });
        if let Some(example) = &spec.example {
            node["example"] = example.clone();
        }
        out["node"] = node;
    }

    Ok(out)
}
