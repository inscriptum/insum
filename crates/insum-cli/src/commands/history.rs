//! `insum history <file>` — readable change history from git (read-only).
//!
//! The JSON envelope lists revisions newest-first, each with its per-note
//! structural diff against the parent revision (core::history). The output
//! budget never truncates silently: when revisions do not fit, the oldest
//! are dropped, `complete` becomes false and a hint names the knob.

use std::path::Path;

use serde_json::{Value, json};

use insum_core::history::{DEFAULT_REVISIONS, collect};
use insum_core::{ErrorCode, Limits, UmError};

use crate::support::{Output, escape_comment_value, render_path};

pub fn run(
    file: &Path,
    note: Option<&str>,
    limit: Option<usize>,
    format: &str,
    max_bytes: Option<usize>,
    limits: &Limits,
) -> Result<Output, UmError> {
    if format != "json" && format != "annotated" {
        return Err(UmError::usage(format!(
            "unknown --format {format:?}: expected json or annotated"
        )));
    }
    if format == "annotated" && max_bytes.is_some() {
        // the annotated projection is bounded by --limit; honoring a byte
        // budget for it would need a text pager — refuse instead of
        // silently ignoring the option
        return Err(UmError::usage(
            "--max-bytes is not supported with --format annotated",
        ));
    }
    let history = collect(file, note, limit.unwrap_or(DEFAULT_REVISIONS), limits)?;

    match format {
        // the annotated projection is compact: the revision limit bounds it
        "annotated" => Ok(Output::Text(render_annotated(&history))),
        _ => {
            let mut value = json!({
                "api_version": 1,
                "path": render_path(file),
                "repo_root": history.repo_root,
                "file_in_repo": history.file_in_repo,
                "revisions": history.revisions,
                "complete": history.complete,
                "warnings": history.warnings,
            });
            apply_budget(&mut value, limits.output_bytes(max_bytes), file)?;
            Ok(Output::Json(value))
        }
    }
}

/// Drop the oldest revisions until the document fits the budget; at least
/// one revision is kept (a single oversized revision is a LIMIT_EXCEEDED).
fn apply_budget(value: &mut Value, budget: usize, file: &Path) -> Result<(), UmError> {
    let mut dropped = 0usize;
    let mut markers = false;
    loop {
        let serialized = serde_json::to_string(value)
            .map_err(|err| UmError::internal(format!("history does not serialize: {err}")))?;
        if serialized.len() <= budget {
            if dropped > 0 {
                let mut warnings = value["warnings"].as_array().cloned().unwrap_or_default();
                for warning in warnings.iter_mut() {
                    if warning["code"] == json!("OUTPUT_BUDGET") {
                        warning["message"] = json!(format!(
                            "{dropped} oldest revision(s) dropped to fit the output budget; pass --max-bytes to raise it"
                        ));
                    }
                }
                value["warnings"] = json!(warnings);
            }
            return Ok(());
        }
        let revisions = value["revisions"]
            .as_array_mut()
            .ok_or_else(|| UmError::internal("history revisions must be an array".to_owned()))?;
        if revisions.len() <= 1 {
            return Err(UmError::new(
                ErrorCode::LimitExceeded,
                format!(
                    "history of {} does not fit the output budget ({serialized} > {budget} bytes) even for one revision",
                    file.display()
                ),
            )
            .with_hint("Raise --max-bytes or narrow the history with --note."));
        }
        revisions.pop();
        dropped += 1;
        // the counter is live from the first drop; the marker fields are
        // part of the output from now on — their bytes count in every
        // later measurement
        value["revisions_dropped"] = json!(dropped);
        if !markers {
            value["complete"] = json!(false);
            let mut warnings = value["warnings"].as_array().cloned().unwrap_or_default();
            warnings.push(json!({
                "code": "OUTPUT_BUDGET",
                "message": "oldest revision(s) dropped to fit the output budget",
            }));
            value["warnings"] = json!(warnings);
            markers = true;
        }
    }
}

/// RFC 3339 UTC from epoch seconds (no local clock, deterministic).
fn iso_utc(timestamp: i64) -> String {
    let days = timestamp.div_euclid(86_400);
    let secs_of_day = timestamp.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

/// Howard Hinnant's civil-from-days: days since 1970-01-01 → y/m/d.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn render_annotated(history: &insum_core::history::History) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "<!-- um:history file_in_repo=\"{}\" revisions={} complete={} -->\n",
        history.file_in_repo,
        history.revisions.len(),
        history.complete
    ));
    for warning in &history.warnings {
        out.push_str(&format!(
            "<!-- um:warning code=\"{}\" --> {}\n",
            warning["code"].as_str().unwrap_or("?"),
            warning["message"].as_str().unwrap_or("")
        ));
    }
    for revision in &history.revisions {
        let revision = &revision.revision;
        out.push_str(&format!(
            "\n@{} {} {} <{}>\n",
            &revision.sha[..7.min(revision.sha.len())],
            iso_utc(revision.timestamp),
            revision.author,
            revision.subject
        ));
    }
    for revision_diff in &history.revisions {
        if let Some(warning) = &revision_diff.warning {
            out.push_str(&format!(
                "  ! {}\n",
                warning["message"].as_str().unwrap_or("")
            ));
        }
        for note in &revision_diff.notes {
            let mut parts = Vec::new();
            if note.added {
                parts.push("note added".to_owned());
            }
            if note.removed {
                parts.push("note removed".to_owned());
            }
            if !(note.blocks_added.is_empty()
                && note.blocks_removed.is_empty()
                && note.blocks_changed.is_empty())
            {
                parts.push(format!(
                    "+{} -{} ~{} blocks",
                    note.blocks_added.len(),
                    note.blocks_removed.len(),
                    note.blocks_changed.len()
                ));
            }
            if parts.is_empty() {
                continue;
            }
            let title = note
                .note_title
                .as_deref()
                .map(|t| format!(" {:?}", escape_comment_value(t)))
                .unwrap_or_default();
            out.push_str(&format!(
                "  note {}{}: {}\n",
                escape_comment_value(&note.note_id),
                title,
                parts.join(", ")
            ));
            for change in &note.blocks_added {
                out.push_str(&render_change(" +", change));
            }
            for change in &note.blocks_removed {
                out.push_str(&render_change(" -", change));
            }
            for change in &note.blocks_changed {
                out.push_str(&render_change(" ~", change));
            }
        }
    }
    out
}

fn render_change(marker: &str, change: &insum_core::history::BlockChange) -> String {
    let id = change
        .id
        .as_deref()
        .map(|id| format!(" id={id}"))
        .unwrap_or_default();
    let text = change
        .text
        .as_deref()
        .map(|text| format!(" {text:?}"))
        .unwrap_or_default();
    format!(
        "{marker} {} {}{}{text}\n",
        change.pointer, change.block_type, id
    )
}
