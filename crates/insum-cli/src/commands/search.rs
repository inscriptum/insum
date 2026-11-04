//! `insum search` — full-text search over inline content (spec §6.4).
//!
//! Matches live on the block that directly contains the inline text — never
//! aggregated on ancestors (§15.1 №8). Best-effort over directories: unread
//! files are reported in `skipped` and lower `complete` (§11).

use std::path::PathBuf;

use regex::RegexBuilder;
use serde_json::{Value, json};

use insum_core::{ErrorCode, Limits, UmError};

use crate::support::{Revs, collect_files, error_value, read_notes, render_path};

const DEFAULT_WINDOW: usize = 160;

/// Options of one `search` invocation (spec §6.4).
pub struct SearchOptions<'a> {
    pub fixed_strings: bool,
    pub ignore_case: bool,
    pub limit: usize,
    pub hidden: bool,
    pub note: Option<&'a str>,
    pub context: usize,
    pub max_bytes: Option<usize>,
    pub paths: &'a [PathBuf],
}

pub fn run(query: &str, options: &SearchOptions, limits: &Limits) -> Result<Value, UmError> {
    let SearchOptions {
        fixed_strings,
        ignore_case,
        limit,
        hidden,
        note,
        context,
        max_bytes,
        paths,
    } = *options;
    if context > 5 {
        return Err(UmError::usage("--context accepts 0..5"));
    }
    let pattern = if fixed_strings {
        regex::escape(query)
    } else {
        query.to_owned()
    };
    let regex = RegexBuilder::new(&pattern)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|err| UmError::usage(format!("invalid regular expression: {err}")))?;

    let search_paths: Vec<PathBuf> = if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths.to_vec()
    };
    let (files, walk_errors) = collect_files(&search_paths, hidden)?;

    let effective_limit = limit.min(limits.max_search_matches);
    let budget = limits.output_bytes(max_bytes);
    let mut matches: Vec<Value> = Vec::new();
    let mut matches_size: usize = 128; // envelope overhead estimate
    let mut skipped: Vec<Value> = Vec::new();
    let mut unknown_inline_files: Vec<Value> = Vec::new();
    let mut not_found_files = 0usize;
    let mut truncated = false;

    // locations that could not be enumerated count into `skipped` (§11)
    for walk_error in &walk_errors {
        skipped.push(json!({
            "path": walk_error.path,
            "error": {"code": "IO_ERROR", "message": walk_error.message},
        }));
    }

    'files: for file in &files {
        if matches.len() >= effective_limit {
            truncated = true;
            break;
        }
        // a .note file cannot contain a note selected by --note: skip it
        if note.is_some()
            && !file
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("um"))
        {
            continue;
        }
        let views = match read_notes(
            file,
            note.map(|n| vec![n.to_owned()]).as_deref(),
            limits,
            Revs::Skip,
        ) {
            Ok(source) => source.views,
            Err(err) => {
                // Search is best-effort — ANY per-file error lands in
                // skipped and lowers complete (§11); one broken file must not
                // kill the whole run
                if err.code == ErrorCode::NoteNotFound {
                    not_found_files += 1;
                }
                skipped.push(json!({
                    "path": render_path(file),
                    "error": error_value(&err),
                }));
                continue;
            }
        };
        for view in views {
            let traversal = insum_core::traverse(&view.doc, None, limits.max_depth);
            let blocks = &traversal.blocks;
            if traversal.unknown_inline {
                unknown_inline_files.push(json!({
                    "path": render_path(file),
                    "note_id": view.note_id,
                    "code": "UNKNOWN_NODE_TYPE",
                    "message": "some blocks contain unknown inline node types; search completeness is not confirmed",
                }));
            }
            for (block_index, block) in blocks.iter().enumerate() {
                let Some(text) = &block.text else {
                    continue;
                };
                // The §6.4 text window is defined around the FIRST
                // match of the block; all hits in this block share it
                let mut block_window: Option<String> = None;
                for found in regex.find_iter(text) {
                    if matches.len() >= effective_limit {
                        truncated = true;
                        break 'files;
                    }
                    if block_window.is_none() {
                        block_window = Some(window(text, found.start(), found.end()));
                    }
                    let hit = json!({
                        "path": render_path(file),
                        "note_id": view.note_id,
                        "note_title": view.note_title,
                        "block_id": block.id,
                        "pointer": block.pointer,
                        "type": block.node_type,
                        "match": found.as_str(),
                        "text": block_window.clone().unwrap_or_default(),
                        "context": context_blocks(blocks, block_index, context),
                    });
                    // budget: whole matches only, no silent truncation —
                    // the first match is subject to the budget too
                    let hit_size = serde_json::to_string(&hit).map_or(256, |s| s.len() + 1);
                    if matches_size + hit_size > budget {
                        truncated = true;
                        break 'files;
                    }
                    matches_size += hit_size;
                    matches.push(hit);
                }
            }
        }
    }

    // NOTE_NOT_FOUND is only an error when the note exists nowhere
    if let Some(note_id) = note
        && matches.is_empty()
        && not_found_files > 0
        && not_found_files == skipped.len()
        && !skipped.is_empty()
    {
        return Err(UmError::new(
            ErrorCode::NoteNotFound,
            format!("note {note_id:?} was not found in any searched container"),
        ));
    }

    // Notes with unknown inline types cannot prove the absence of data
    let complete = !truncated && skipped.is_empty() && unknown_inline_files.is_empty();
    let mut response = json!({
        "api_version": 1,
        "query": query,
        "mode": if fixed_strings { "literal" } else { "regex" },
        "ignore_case": ignore_case,
        "matches": matches,
        "truncated": truncated,
        "complete": complete,
        "skipped": skipped,
    });
    if !unknown_inline_files.is_empty() {
        response["warnings"] = json!(unknown_inline_files);
    }
    Ok(response)
}

/// A window of up to `DEFAULT_WINDOW` characters around the first match,
/// with `…` on cut edges (spec §6.4).
fn window(text: &str, start: usize, end: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let match_start = text[..start].chars().count();
    let match_end = match_start + (end - start);
    if chars.len() <= DEFAULT_WINDOW {
        return text.to_owned();
    }
    let span = match_end.saturating_sub(match_start);
    let padding = (DEFAULT_WINDOW - span.min(DEFAULT_WINDOW)) / 2;
    let from = match_start.saturating_sub(padding);
    let to = (match_end + padding).min(chars.len());
    let mut out = String::new();
    if from > 0 {
        out.push('…');
    }
    out.extend(&chars[from..to]);
    if to < chars.len() {
        out.push('…');
    }
    out
}

/// Neighboring structural blocks for `--context > 0` (spec §6.4).
fn context_blocks(blocks: &[insum_core::BlockEntry], index: usize, context: usize) -> Vec<Value> {
    if context == 0 {
        return Vec::new();
    }
    let from = index.saturating_sub(context);
    let to = (index + context + 1).min(blocks.len());
    blocks[from..to]
        .iter()
        .filter(|neighbor| neighbor.pointer != blocks[index].pointer)
        .map(|neighbor| {
            let mut entry = json!({
                "pointer": neighbor.pointer,
                "type": neighbor.node_type,
            });
            if let Some(text) = &neighbor.text {
                // Neighbors get a window too — a 5MB sibling paragraph
                // must not leak into the response whole
                let chars: Vec<char> = text.chars().collect();
                if chars.len() > DEFAULT_WINDOW {
                    let head: String = chars[..DEFAULT_WINDOW].iter().collect();
                    entry["text"] = json!(format!("{head}…"));
                } else {
                    entry["text"] = json!(text);
                }
            }
            entry
        })
        .collect()
}
