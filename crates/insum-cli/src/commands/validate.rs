//! `insum validate` — read-only validation with explicit levels.
//!
//! Without an applicable schema artifact the level is `structural`:
//! containers, manifests, note JSON structure, entry names, limits and ID
//! rules for documents marked `features.block_ids`. With an applicable
//! artifact (explicit `--schema` or the plugin runtime artifact found by
//! the vault rule, hash verified) inventory checks against the artifact run
//! and the level is `schema`. The bundled fallback is a reserve for
//! diagnostics — it never lifts the level. `full` is reserved for the
//! differential corpus (§4.8) and is never claimed by this release.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use insum_core::pmschema::PmSchema;
use insum_core::schema::{ResolvedSchema, SchemaSource, resolve};
use insum_core::{Limits, UmError};

use crate::support::{Revs, collect_files, error_value, read_notes, render_path};

pub fn run(
    paths: &[PathBuf],
    strict: bool,
    schema: Option<&Path>,
    hidden: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    if paths.is_empty() {
        return Err(UmError::usage("validate requires at least one path"));
    }

    // An explicit artifact is loaded once and applies to every file; a
    // missing or corrupt explicit file is a hard error (the user pointed
    // at it), matching the resolution contract in insum-core.
    let explicit: Option<Arc<ResolvedSchema>> = match schema {
        Some(path) => Some(Arc::new(resolve(Some(path), Path::new("."), limits)?)),
        None => None,
    };

    let (files, walk_errors) = collect_files(paths, hidden)?;
    let mut file_results = Vec::new();
    let mut all_valid = true;
    // The reported level is the weakest level actually applied: a single
    // structurally-validated file keeps the whole report honest.
    let mut all_schema = true;

    // locations that could not be enumerated lower the result
    for walk_error in &walk_errors {
        all_valid = false;
        all_schema = false;
        file_results.push(json!({
            "path": walk_error.path,
            "valid": false,
            "validation_level": "structural",
            "notes": [],
            "errors": [{"code": "IO_ERROR", "message": walk_error.message}],
            "warnings": [],
        }));
    }

    // runtime artifacts are looked up once per parent directory; the
    // compiled parity schema is reused across files (keyed by artifact path)
    let mut runtime_cache: HashMap<PathBuf, Option<Result<Arc<ResolvedSchema>, UmError>>> =
        HashMap::new();
    let mut pm_cache: HashMap<PathBuf, Arc<PmSchema>> = HashMap::new();
    let mut explicit_pm: Option<Arc<PmSchema>> = None;

    for file in &files {
        let rendered = render_path(file);
        let file_schema = file_schema(
            file,
            explicit.as_ref(),
            &mut runtime_cache,
            &mut pm_cache,
            &mut explicit_pm,
            limits,
        );
        if file_schema.level != "schema" {
            all_schema = false;
        }
        let result = validate_file(file, strict, file_schema.pm.as_deref(), limits);
        match result {
            Ok(mut file_json) => {
                file_json["path"] = json!(rendered);
                file_json["validation_level"] = json!(file_schema.level);
                if let Some(info) = file_schema.info {
                    file_json["schema"] = info;
                }
                if !file_schema.warnings.is_empty() {
                    // §6.6: --strict turns every warning into an error —
                    // artifact configuration warnings included.
                    if strict {
                        let mut errors = file_schema.warnings;
                        if let Some(existing) = file_json["errors"].as_array() {
                            errors.extend(existing.iter().cloned());
                        }
                        file_json["errors"] = json!(errors);
                        file_json["valid"] = json!(false);
                    } else {
                        let mut warnings = file_schema.warnings;
                        if let Some(existing) = file_json["warnings"].as_array() {
                            warnings.extend(existing.iter().cloned());
                        }
                        file_json["warnings"] = json!(warnings);
                    }
                }
                if file_json["valid"] != json!(true) {
                    all_valid = false;
                }
                file_results.push(file_json);
            }
            Err(err) => {
                all_valid = false;
                all_schema = false;
                file_results.push(json!({
                    "path": rendered,
                    "valid": false,
                    "validation_level": "structural",
                    "notes": [],
                    "errors": [error_value(&err)],
                    "warnings": [],
                }));
            }
        }
    }

    // walk errors are appended after the walked files — §5.3 requires the
    // files[] array globally sorted by path
    file_results.sort_by_key(|entry| {
        entry["path"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_default()
    });

    // An empty file set validated nothing: the report must not claim the
    // schema level for a run without a single validated file.
    let top_level_schema = all_schema && !file_results.is_empty();
    Ok(json!({
        "api_version": 1,
        "validation_level": if top_level_schema { "schema" } else { "structural" },
        "valid": all_valid,
        "files": file_results,
    }))
}

/// The per-file artifact decision: which level applies, whether the
/// ProseMirror-parity check runs, what the report shows and which
/// configuration diagnostics (stale or unusable artifact) surface.
struct FileSchema {
    level: &'static str,
    pm: Option<Arc<PmSchema>>,
    info: Option<Value>,
    warnings: Vec<Value>,
}

/// Compile the parity schema once per artifact; a broken artifact (bad
/// content expression) becomes a configuration warning, not a crash.
fn compiled(resolved: &ResolvedSchema) -> Result<Arc<PmSchema>, UmError> {
    PmSchema::build(&resolved.artifact).map(Arc::new)
}

fn file_schema(
    file: &Path,
    explicit: Option<&Arc<ResolvedSchema>>,
    runtime_cache: &mut HashMap<PathBuf, Option<Result<Arc<ResolvedSchema>, UmError>>>,
    pm_cache: &mut HashMap<PathBuf, Arc<PmSchema>>,
    explicit_pm: &mut Option<Arc<PmSchema>>,
    limits: &Limits,
) -> FileSchema {
    let info_of = |resolved: &ResolvedSchema| {
        json!({
            "source": resolved.source.as_str(),
            "source_path": resolved.source_path.as_ref().map(|p| p.display().to_string()),
            "schema_version": resolved.artifact.schema_version,
            "plugin_version": resolved.artifact.plugin_version,
            "schema_hash": resolved.artifact.schema_hash,
            "hash_verified": resolved.hash_verified,
            "validation_capability": resolved.artifact.validation_capability,
        })
    };

    if let Some(resolved) = explicit {
        if resolved.hash_verified {
            if explicit_pm.is_none() {
                *explicit_pm = compiled(resolved).ok();
            }
            let Some(pm) = explicit_pm.clone() else {
                return FileSchema {
                    level: "structural",
                    pm: None,
                    info: Some(info_of(resolved)),
                    warnings: vec![json!({
                        "code": "SCHEMA_ARTIFACT_INVALID",
                        "message": "schema artifact could not be compiled; validation stays structural",
                    })],
                };
            };
            return FileSchema {
                level: "schema",
                pm: Some(pm),
                info: Some(info_of(resolved)),
                warnings: Vec::new(),
            };
        }
        return FileSchema {
            level: "structural",
            pm: None,
            info: Some(info_of(resolved)),
            warnings: vec![json!({
                "code": "SCHEMA_MISMATCH",
                "message": "explicit schema artifact failed the hash check; validation stays structural",
            })],
        };
    }

    let start = file.parent().unwrap_or_else(|| Path::new("."));
    let cached = runtime_cache
        .entry(start.to_path_buf())
        .or_insert_with(|| None)
        .clone();
    let resolved = match cached {
        Some(result) => result,
        None => {
            let result = resolve(None, start, limits).map(Arc::new);
            runtime_cache.insert(start.to_path_buf(), Some(result.clone()));
            result
        }
    };

    match resolved {
        Ok(resolved) if resolved.source == SchemaSource::BundledFallback => FileSchema {
            level: "structural",
            pm: None,
            info: Some(info_of(&resolved)),
            warnings: Vec::new(),
        },
        Ok(resolved) if !resolved.hash_verified => FileSchema {
            level: "structural",
            pm: None,
            info: Some(info_of(&resolved)),
            warnings: vec![json!({
                "code": "SCHEMA_MISMATCH",
                "message": format!(
                    "runtime schema artifact at {} failed the hash check; validation stays structural",
                    resolved.source_path.as_ref().map_or_else(String::new, |p| p.display().to_string())
                ),
            })],
        },
        Ok(resolved) => {
            // compile once per artifact path (bundled has no path and is
            // never compiled — it does not lift the level)
            let pm = match &resolved.source_path {
                Some(path) => {
                    if let Some(cached) = pm_cache.get(path) {
                        Some(cached.clone())
                    } else {
                        let pm = compiled(&resolved).ok();
                        if let Some(pm) = &pm {
                            pm_cache.insert(path.clone(), pm.clone());
                        }
                        pm
                    }
                }
                None => None,
            };
            match pm {
                Some(pm) => FileSchema {
                    level: "schema",
                    pm: Some(pm),
                    info: Some(info_of(&resolved)),
                    warnings: Vec::new(),
                },
                None => FileSchema {
                    level: "structural",
                    pm: None,
                    info: Some(info_of(&resolved)),
                    warnings: vec![json!({
                        "code": "SCHEMA_ARTIFACT_INVALID",
                        "message": "runtime schema artifact could not be compiled; validation stays structural",
                    })],
                },
            }
        }
        // The registry code of the actual failure (invalid, oversized,
        // unreadable) is kept — repair hints differ per class.
        Err(err) => FileSchema {
            level: "structural",
            pm: None,
            info: None,
            warnings: vec![json!({
                "code": err.code.as_str(),
                "message": format!("runtime schema artifact could not be used: {}", err.message),
            })],
        },
    }
}

fn validate_file(
    path: &Path,
    strict: bool,
    pm: Option<&PmSchema>,
    limits: &Limits,
) -> Result<Value, UmError> {
    let views = read_notes(path, None, limits, Revs::Skip)?.views;
    let mut notes = Vec::new();
    let mut file_valid = true;

    for view in views {
        // validation logic lives in insum-core; the CLI
        // only shapes the output
        let validation = insum_core::validate_note_doc_with_schema(
            &view.doc,
            view.block_ids_enabled,
            limits,
            pm,
        )?;
        if !validation.valid() {
            file_valid = false;
        }
        if strict && !validation.warnings.is_empty() {
            // --strict promotes warnings to errors. NOTE_DOC_INVALID
            // is the data-level code the registry offers for it (exit 4);
            // without --strict a warning never flips `valid`.
            file_valid = false;
            let mut strict_errors = warnings_as_errors(&validation.warnings);
            strict_errors.extend(diagnostics(&validation.errors));
            notes.push(json!({
                "id": view.note_id,
                "valid": false,
                "errors": strict_errors,
                "warnings": [],
            }));
            continue;
        }
        notes.push(json!({
            "id": view.note_id,
            "valid": validation.valid(),
            "errors": diagnostics(&validation.errors),
            "warnings": diagnostics(&validation.warnings),
        }));
    }

    Ok(json!({
        "valid": file_valid,
        "notes": notes,
        "errors": [],
        "warnings": [],
    }))
}

fn warnings_as_errors(warnings: &[insum_core::Diag]) -> Vec<Value> {
    warnings
        .iter()
        .map(|warning| {
            json!({
                "code": warning.code,
                "message": format!("strict: {}", warning.message)
            })
        })
        .collect()
}

fn diagnostics(diags: &[insum_core::Diag]) -> Vec<Value> {
    diags
        .iter()
        .map(|diag| json!({"code": diag.code, "message": diag.message}))
        .collect()
}
