//! `insum tree` — compact block structure with JSON Pointers (spec §6.2).

use std::path::Path;

use serde_json::{Map, Value, json};

use insum_core::model::BlockEntry;
use insum_core::{ErrorCode, Limits, UmError};

use crate::cursor::Cursor;
use crate::support::{NoteView, Revs, SourceReader, render_path};

/// Serialized `attrs` are capped: a hostile document can carry megabytes in
/// attributes, and the response budget must not be broken by one entry
///.
const ATTRS_CAP: usize = 2048;

pub fn run(
    path: &Path,
    note: &[String],
    depth: Option<usize>,
    text_limit: usize,
    max_bytes: Option<usize>,
    cursor_token: Option<&str>,
    limits: &Limits,
) -> Result<Value, UmError> {
    let rendered = render_path(path);
    let cursor = match cursor_token {
        Some(token) => Some(Cursor::decode(token)?),
        None => None,
    };

    // The exposed sequence is NOT narrowed by the cursor — pagination
    // must reach every note (§6.2). The reader parses payloads lazily, so the
    // lazy-read win (a page only pays for the notes it emits) is preserved.
    let filters = if note.is_empty() { None } else { Some(note) };
    let mut reader = SourceReader::open(path, filters, limits, Revs::Compute)?;

    // Mid-note is determined by the POSITION (block_index > 0), never
    // by the optional np field — a stripped np cannot deactivate the guards.
    // validate() enforces the bi⇔np invariant and the unconditional rev
    // binding after the pinned note is read; paginate_note keeps the
    // pointer match against the resumed position.
    let resume_pointer = cursor.as_ref().and_then(|c| c.next_pointer.clone());
    let start_note = cursor.as_ref().map_or(0, |c| c.note_index);
    let budget = limits.output_bytes(max_bytes);

    if cursor.is_some() && start_note >= reader.len() {
        return Err(stale_cursor(
            "cursor points past the last note of this response",
        ));
    }

    // The budget is per response. Page shape: the first note may be
    // partial (mid-note cursor); every following note is emitted whole or the
    // page stops on the note boundary with a cursor to it (the walk
    // reaches every note instead of silently ending).
    let mut used: usize = 256; // envelope overhead estimate
    let mut notes_out = Vec::new();
    let mut pos = start_note;

    // first note (partial allowed, mid-note cursor)
    if pos < reader.len() {
        let view = reader.read_at(pos)?;
        if let Some(cursor) = &cursor {
            // Rev binding is unconditional — the pinned/target note is
            // read here anyway, so boundary resumes check it too
            cursor.validate(
                "tree",
                &rendered,
                view.note_id.as_deref(),
                view.note_rev.as_str(),
                depth,
                text_limit,
            )?;
        }
        let resume = resume_pointer.as_deref();
        let start_block = cursor.as_ref().map_or(0, |c| c.block_index);
        let (note_json, note_used, has_more) = paginate_note(
            view,
            &rendered,
            pos,
            start_block,
            resume,
            depth,
            text_limit,
            budget.saturating_sub(used),
            limits,
        )?;
        used += note_used;
        notes_out.push(note_json);
        if has_more {
            return Ok(json!({
                "api_version": 1,
                "path": rendered,
                "notes": notes_out,
            }));
        }
        pos += 1;
    }

    // following notes: whole-fit or boundary cursor
    while pos < reader.len() {
        let view = reader.read_at(pos)?;
        let (full_json, full_size) = note_full(view, depth, text_limit, limits)?;
        if used + full_size < budget {
            used += full_size + 1;
            notes_out.push(full_json);
            pos += 1;
            continue;
        }
        // boundary cursor: continue with this note on the next page.
        // It binds to the TARGET note's revision — the note is already
        // parsed for the size check, so no extra work is needed.
        let cursor = Cursor::tree(
            rendered.clone(),
            view.note_id.clone(),
            view.note_rev.clone(),
            pos,
            0,
            depth,
            text_limit,
            None,
        );
        notes_out.last_mut().expect("a note was just pushed")["next_cursor"] =
            Value::String(cursor.encode());
        return Ok(json!({
            "api_version": 1,
            "path": rendered,
            "notes": notes_out,
        }));
    }

    Ok(json!({
        "api_version": 1,
        "path": rendered,
        "notes": notes_out,
    }))
}

fn stale_cursor(message: &str) -> UmError {
    UmError::new(ErrorCode::CursorStale, message.to_owned())
        .with_hint("Re-run the command without --cursor and paginate from the start.")
}

/// Emit the first note of the page, blocks cut within the remaining budget;
/// returns `(json, bytes_used, has_more)`.
#[allow(clippy::too_many_arguments)]
fn paginate_note(
    view: &NoteView,
    rendered: &str,
    note_index: usize,
    start_block: usize,
    resume_pointer: Option<&str>,
    depth: Option<usize>,
    text_limit: usize,
    budget: usize,
    limits: &Limits,
) -> Result<(Value, usize, bool), UmError> {
    let traversal = insum_core::traverse(&view.doc, depth, limits.max_depth);
    if traversal.depth_exceeded {
        return Err(UmError::new(
            ErrorCode::LimitExceeded,
            format!("document deeper than UM_MAX_DEPTH ({})", limits.max_depth),
        ));
    }
    // a cursor position past the document would silently produce an empty
    // `complete` page — treat it as stale instead (§6.2)
    if start_block >= traversal.blocks.len() && start_block > 0 {
        return Err(stale_cursor(
            "cursor block position is past the end of the note",
        ));
    }
    // The pointer recorded with the cursor must match the block at the
    // resumed position — tampering is stale, not a silent skip
    if let Some(expected) = resume_pointer
        && start_block > 0
        && !traversal
            .blocks
            .get(start_block)
            .is_some_and(|block| block.pointer == expected)
    {
        return Err(stale_cursor(
            "cursor block position does not match the recorded pointer",
        ));
    }

    let mut blocks_out: Vec<Value> = Vec::new();
    let mut used: usize = 384; // note envelope + blocks array overhead
    let mut has_more = false;

    for block in traversal.blocks.iter().skip(start_block) {
        let entry = block_json(block, text_limit);
        let entry_size = serde_json::to_string(&entry).map_or(256, |s| s.len() + 1);
        if !blocks_out.is_empty() && used + entry_size > budget {
            has_more = true;
            break;
        }
        used += entry_size;
        blocks_out.push(entry);
    }

    let children_truncated = traversal.blocks.iter().any(|b| b.children_truncated);
    let next_cursor = if has_more {
        let next_index = start_block + blocks_out.len();
        let next_pointer = traversal
            .blocks
            .get(next_index)
            .map(|block| block.pointer.clone());
        let cursor = Cursor::tree(
            rendered.to_owned(),
            view.note_id.clone(),
            view.note_rev.clone(),
            note_index,
            next_index,
            depth,
            text_limit,
            next_pointer,
        );
        Value::String(cursor.encode())
    } else {
        Value::Null
    };

    let complete = !has_more && !children_truncated;
    let mut note_json = json!({
        "note_id": view.note_id,
        "note_title": view.note_title,
        "note_rev": view.note_rev,
        "blocks": blocks_out,
        "complete": complete,
        "next_cursor": next_cursor,
    });
    if traversal.unknown_inline {
        // Text of unknown inline nodes is withheld — no silent losses
        note_json["warnings"] = json!([{
            "code": "UNKNOWN_NODE_TYPE",
            "message": "some blocks contain unknown inline node types; their text is not extracted",
        }]);
    }
    Ok((note_json, used, has_more))
}

/// The complete JSON of one note with all blocks (no budget cut) — used to
/// decide whether a following note fits on the page whole.
fn note_full(
    view: &NoteView,
    depth: Option<usize>,
    text_limit: usize,
    limits: &Limits,
) -> Result<(Value, usize), UmError> {
    let traversal = insum_core::traverse(&view.doc, depth, limits.max_depth);
    if traversal.depth_exceeded {
        return Err(UmError::new(
            ErrorCode::LimitExceeded,
            format!("document deeper than UM_MAX_DEPTH ({})", limits.max_depth),
        ));
    }
    let blocks_out: Vec<Value> = traversal
        .blocks
        .iter()
        .map(|block| block_json(block, text_limit))
        .collect();
    let complete = !traversal.blocks.iter().any(|b| b.children_truncated);
    let mut note_json = json!({
        "note_id": view.note_id,
        "note_title": view.note_title,
        "note_rev": view.note_rev,
        "blocks": blocks_out,
        "complete": complete,
        "next_cursor": Value::Null,
    });
    if traversal.unknown_inline {
        note_json["warnings"] = json!([{
            "code": "UNKNOWN_NODE_TYPE",
            "message": "some blocks contain unknown inline node types; their text is not extracted",
        }]);
    }
    let size = serde_json::to_string(&note_json).map_or(384, |s| s.len() + 1);
    Ok((note_json, size))
}

fn block_json(block: &BlockEntry, text_limit: usize) -> Value {
    let (attrs, attrs_truncated) = capped_attrs(block);
    let mut entry = json!({
        "pointer": block.pointer,
        "id": block.id,
        "type": block.node_type,
        "parent_pointer": block.parent_pointer,
        "depth": block.depth,
        "index": block.index,
        "attrs": attrs,
    });
    if attrs_truncated {
        entry["attrs_truncated"] = json!(true);
    }
    if let Some(text) = &block.text {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() > text_limit {
            let truncated: String = chars[..text_limit].iter().collect();
            entry["text"] = json!(truncated);
            entry["text_truncated"] = json!(true);
        } else {
            entry["text"] = json!(text);
        }
    }
    if block.text_incomplete {
        entry["text_incomplete"] = json!(true);
    }
    if block.children_truncated {
        entry["children_truncated"] = json!(true);
    }
    entry
}

/// Copy attrs key by key until the serialized size hits the cap; the marker
/// `attrs_truncated` rides on the block entry.
fn capped_attrs(block: &BlockEntry) -> (Value, bool) {
    let Value::Object(attrs) = &block.attrs else {
        return (block.attrs.clone(), false);
    };
    let mut out = Map::new();
    let mut size = 2usize; // "{}"
    for (key, value) in attrs {
        let chunk = serde_json::to_string(&(key, value)).map_or(key.len() + 8, |s| s.len() + 1);
        if size + chunk > ATTRS_CAP {
            return (Value::Object(out), true);
        }
        size += chunk;
        out.insert(key.clone(), value.clone());
    }
    (Value::Object(out), false)
}
