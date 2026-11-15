//! `insum read` — whole-note reading: JSON envelope or the markdown
//! projections. `annotated` carries pointer anchors (the historical shape);
//! `compact` is the same pipeline with anchors off — not a single service
//! comment; `--with-anchors` grows them back (it is annotated by another
//! name). The projections are read-only text; they are never storage
//! formats and have no parser.

use std::path::Path;

use serde_json::json;

use insum_core::model::BlockEntry;
use insum_core::projection::Projection;
use insum_core::{ErrorCode, Limits, UmError};

use crate::cursor::Cursor;
use crate::support::{Output, Revs, SourceReader, escape_comment_value, render_path};

pub fn run(
    path: &Path,
    note: Option<&str>,
    format: &str,
    with_anchors: bool,
    max_bytes: Option<usize>,
    cursor_token: Option<&str>,
    limits: &Limits,
) -> Result<Output, UmError> {
    match format {
        "json" | "annotated" | "compact" => {}
        other => {
            return Err(UmError::usage(format!(
                "--format must be `json`, `annotated` or `compact`, got {other:?}"
            )));
        }
    }
    // Anchors are the annotated shape; compact grows them only on request.
    // No silent flag ignoring — a wrong combination is a usage error.
    if with_anchors && format != "compact" {
        return Err(
            UmError::usage("--with-anchors applies to --format compact only").with_hint(match format {
                "json" => "json output has no anchors; exact reading is the JSON envelope itself.",
                _ => "The annotated projection always carries anchors — it is compact with anchors already.",
            }),
        );
    }
    if format == "json" && cursor_token.is_some() {
        // --cursor was silently ignored on json output — reject instead
        return Err(
            UmError::usage("--cursor applies to --format annotated only in this release")
                .with_hint(
                    "json reads the whole note; use `insum tree` for block-wise JSON pagination.",
                ),
        );
    }

    let rendered = render_path(path);
    let cursor = match cursor_token {
        Some(token) => Some(Cursor::decode(token)?),
        None => None,
    };
    // No cursor-based narrowing — a multi-note container is read
    // across notes; the lazy reader keeps the per-page cost bounded.
    let filters = note.map(|n| vec![n.to_owned()]);
    let mut reader = SourceReader::open(path, filters.as_deref(), limits, Revs::Compute)?;
    let start_note = cursor.as_ref().map_or(0, |c| c.note_index);
    let budget = limits.output_bytes(max_bytes);

    if cursor.is_some() && start_note >= reader.len() {
        return Err(stale_cursor(
            "cursor points past the last note of this response",
        ));
    }

    if format == "json" {
        // The budget is per response — no silent oversize; the command
        // names the required size instead of exceeding it
        let mut notes = Vec::new();
        let mut used: usize = 256;
        for pos in 0..reader.len() {
            let view = reader.read_at(pos)?;
            let note_value = json!({
                "note_id": view.note_id,
                "note_title": view.note_title,
                "note_rev": view.note_rev,
                "features": view.features,
                "document": view.doc,
            });
            let size = serde_json::to_string(&note_value).map_or(0, |text| text.len());
            if used + size + 1 > budget {
                return Err(UmError::new(
                    ErrorCode::LimitExceeded,
                    format!(
                        "note does not fit into the output budget ({budget} bytes, need {needed})",
                        needed = used + size + 1
                    ),
                )
                .with_hint(
                    "Raise --max-bytes, or use `insum tree` / `insum read --format annotated` for block-wise pagination.",
                ));
            }
            used += size + 1;
            notes.push(note_value);
        }
        return Ok(Output::Json(json!({
            "api_version": 1,
            "path": rendered,
            "file_rev": reader.file_rev()?,
            "notes": notes,
        })));
    }

    // annotated / compact: paginate by top-level blocks across notes.
    // Page shape mirrors tree: the first note may be partial (mid-note
    // cursor); every following note is emitted whole or the page stops on
    // the note boundary with an incomplete marker + cursor to it.
    // annotated == compact --with-anchors byte-for-byte: one projection
    // pipeline, the anchors switch is the only difference.
    let anchors = format == "annotated" || with_anchors;
    let projection = Projection::new(anchors);
    let mut out = String::new();
    let mut pos = start_note;

    // first note of the page (partial allowed, mid-note cursor)
    if pos < reader.len() {
        let view = reader.read_at(pos)?;
        if let Some(cursor) = &cursor {
            // Unconditional rev binding; the bi⇔np invariant
            // is enforced inside validate
            cursor.validate(
                "read",
                &rendered,
                view.note_id.as_deref(),
                view.note_rev.as_str(),
                None,
                0,
            )?;
        }
        let start_block = cursor.as_ref().map_or(0, |c| c.block_index);
        if anchors {
            // the annotated header is part of the mode's byte contract;
            // compact without anchors carries no service comment at all
            let header = format!(
                "<!-- um:api_version=1 note_id=\"{}\" note_rev=\"{}\" -->\n",
                escape_comment_value(&view.note_id.clone().unwrap_or_default()),
                view.note_rev
            );
            out.push_str(&header);
        }
        let mut used = out.len();
        let blocks = insum_core::traverse(&view.doc, None, limits.max_depth);
        if blocks.depth_exceeded {
            return Err(UmError::new(
                ErrorCode::LimitExceeded,
                format!("document deeper than UM_MAX_DEPTH ({})", limits.max_depth),
            ));
        }
        // Children are indexed once per note instead of O(B²) scans
        let children_of = Projection::children_index(&blocks.blocks);
        let top_level: Vec<&BlockEntry> = blocks.blocks.iter().filter(|b| b.depth == 0).collect();
        // Parity with tree: a mid-note resume pointer must match the
        // block at the resumed position
        if let Some(expected) = cursor.as_ref().and_then(|c| c.next_pointer.clone())
            && start_block > 0
            && !top_level
                .get(start_block)
                .is_some_and(|block| block.pointer == expected)
        {
            return Err(stale_cursor(
                "cursor block position does not match the recorded pointer",
            ));
        }
        let mut oversized: Vec<&BlockEntry> = Vec::new();
        for (block_position, block) in top_level.iter().enumerate().skip(start_block) {
            let mut rendered_block = String::new();
            projection.render_block(block, &view.doc, &children_of, &mut rendered_block);
            let chunk = format!("\n{rendered_block}");
            if used + chunk.len() > budget && block_position > start_block {
                // whole blocks only — no silent or partial truncation.
                // W-01: flush the oversized markers collected so far BEFORE
                // the incomplete return — they belong to blocks already
                // emitted, whatever follows them.
                for pending in &oversized {
                    out.push_str(&format!(
                        "\n{}\n",
                        projection.service_marker(&format!(
                            "um:oversized-block pointer=\"{}\" budget=\"{}\"",
                            pending.pointer, budget
                        ))
                    ));
                }
                let next_pointer = top_level.get(block_position).map(|b| b.pointer.clone());
                let cursor = Cursor::read(
                    rendered.clone(),
                    view.note_id.clone(),
                    view.note_rev.clone(),
                    pos,
                    block_position,
                    None,
                    0,
                    next_pointer,
                );
                out.push_str(&format!(
                    "\n{}\n",
                    projection.service_marker(&format!(
                        "um:incomplete next_cursor=\"{}\"",
                        cursor.encode()
                    ))
                ));
                return Ok(finish_text(out, anchors));
            }
            // PR review: a single block larger than the whole budget is still
            // emitted whole (documented exception) — but explicitly marked.
            if used + chunk.len() > budget {
                oversized.push(block);
            }
            used += chunk.len();
            out.push_str(&chunk);
        }
        for block in oversized {
            out.push_str(&format!(
                "\n{}\n",
                projection.service_marker(&format!(
                    "um:oversized-block pointer=\"{}\" budget=\"{}\"",
                    block.pointer, budget
                ))
            ));
        }
        pos += 1;
    }

    // following notes: whole-fit or boundary marker
    while pos < reader.len() {
        let view = reader.read_at(pos)?;
        let mut scratch = String::new();
        if !out.is_empty() {
            scratch.push('\n');
        }
        if anchors {
            scratch.push_str(&format!(
                "<!-- um:api_version=1 note_id=\"{}\" note_rev=\"{}\" -->\n",
                escape_comment_value(&view.note_id.clone().unwrap_or_default()),
                view.note_rev
            ));
        }
        let blocks = insum_core::traverse(&view.doc, None, limits.max_depth);
        if blocks.depth_exceeded {
            return Err(UmError::new(
                ErrorCode::LimitExceeded,
                format!("document deeper than UM_MAX_DEPTH ({})", limits.max_depth),
            ));
        }
        let children_of = Projection::children_index(&blocks.blocks);
        for block in &blocks.blocks {
            if block.depth != 0 {
                continue;
            }
            let mut rendered_block = String::new();
            projection.render_block(block, &view.doc, &children_of, &mut rendered_block);
            scratch.push_str(&format!("\n{rendered_block}"));
        }
        if out.len() + scratch.len() <= budget {
            out.push_str(&scratch);
            pos += 1;
            continue;
        }
        // Boundary marker + cursor to this note
        let cursor = Cursor::read(
            rendered.clone(),
            view.note_id.clone(),
            view.note_rev.clone(),
            pos,
            0,
            None,
            0,
            None,
        );
        out.push_str(&format!(
            "\n{}\n",
            projection.service_marker(&format!(
                "um:incomplete next_cursor=\"{}\"",
                cursor.encode()
            ))
        ));
        return Ok(finish_text(out, anchors));
    }

    Ok(finish_text(out, anchors))
}

/// Close a text page. The anchor-free compact mode has no header, so the
/// first block chunk's leading newline would open the page with a blank
/// line — strip it. The budget accounting keeps that byte, which only ever
/// stops a page one byte earlier than strictly needed (never overruns).
fn finish_text(out: String, anchors: bool) -> Output {
    if anchors {
        return Output::Text(out);
    }
    match out.strip_prefix('\n') {
        Some(rest) => Output::Text(rest.to_owned()),
        None => Output::Text(out),
    }
}

fn stale_cursor(message: &str) -> UmError {
    UmError::new(insum_core::ErrorCode::CursorStale, message.to_owned())
        .with_hint("Re-run the command without --cursor and paginate from the start.")
}
