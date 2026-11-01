//! `insum tree` — compact block structure with JSON Pointers (spec §6.2).

use std::path::Path;

use serde_json::{Value, json};

use insum_core::model::BlockEntry;
use insum_core::{Limits, UmError};

use crate::cursor::Cursor;
use crate::support::{NoteView, read_views, render_path};

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
    let mut selected: Vec<NoteView> = Vec::new();
    if note.is_empty() {
        selected = read_views(path, None, limits)?;
    } else {
        for note_id in note {
            let mut one = read_views(path, Some(note_id), limits)?;
            selected.append(&mut one);
        }
    }

    // cursor contract: same query, same revision (validated per note below)
    let cursor = match cursor_token {
        Some(token) => Some(Cursor::decode(token)?),
        None => None,
    };
    if let Some(cursor) = &cursor {
        let view = selected.get(cursor.note_index);
        cursor.validate(
            "tree",
            &rendered,
            view.and_then(|v| v.note_id.as_deref()),
            view.map(|v| v.note_rev.as_str()).unwrap_or_default(),
        )?;
    }
    let start_note = cursor.as_ref().map_or(0, |c| c.note_index);
    let budget = limits.output_bytes(max_bytes);

    let mut notes_out = Vec::new();
    let mut resumed = false;
    for (note_index, view) in selected.iter().enumerate().skip(start_note) {
        let start_block = if note_index == start_note && !resumed {
            cursor.as_ref().map_or(0, |c| c.block_index)
        } else {
            0
        };
        resumed = true;
        let ctx = PageCtx {
            rendered: &rendered,
            note_index,
            depth,
            text_limit,
            budget,
        };
        let (note_json, has_more) = paginate_note(view, &ctx, start_block, limits)?;
        notes_out.push(note_json);
        if has_more {
            // stop emitting further notes; the cursor resumes inside this one
            break;
        }
    }

    Ok(json!({
        "api_version": 1,
        "path": rendered,
        "notes": notes_out,
    }))
}

/// Query context threaded through note pagination.
struct PageCtx<'a> {
    rendered: &'a str,
    note_index: usize,
    depth: Option<usize>,
    text_limit: usize,
    budget: usize,
}

/// Emit one note's blocks within the budget; returns `(json, has_more)`.
fn paginate_note(
    view: &NoteView,
    ctx: &PageCtx,
    start_block: usize,
    limits: &Limits,
) -> Result<(Value, bool), UmError> {
    let all_blocks = insum_core::traverse(&view.doc, ctx.depth, limits.max_depth);
    let mut blocks_out: Vec<Value> = Vec::new();
    let mut used: usize = 512; // envelope overhead estimate
    let mut has_more = false;

    for block in all_blocks.iter().skip(start_block) {
        let entry = block_json(block, ctx.text_limit);
        let entry_size = serde_json::to_string(&entry).map_or(256, |s| s.len() + 1);
        if !blocks_out.is_empty() && used + entry_size > ctx.budget {
            has_more = true;
            break;
        }
        used += entry_size;
        blocks_out.push(entry);
    }

    let children_truncated = all_blocks.iter().any(|b| b.children_truncated);
    let next_cursor = if has_more {
        let next_index = start_block + blocks_out.len();
        let cursor = Cursor::tree(
            ctx.rendered.to_owned(),
            view.note_id.clone(),
            view.note_rev.clone(),
            ctx.note_index,
            next_index,
            ctx.depth,
            ctx.text_limit,
        );
        Value::String(cursor.encode())
    } else {
        Value::Null
    };

    let complete = !has_more && !children_truncated;
    Ok((
        json!({
            "note_id": view.note_id,
            "note_title": view.note_title,
            "note_rev": view.note_rev,
            "blocks": blocks_out,
            "complete": complete,
            "next_cursor": next_cursor,
        }),
        has_more,
    ))
}

fn block_json(block: &BlockEntry, text_limit: usize) -> Value {
    let mut entry = json!({
        "pointer": block.pointer,
        "id": block.id,
        "type": block.node_type,
        "parent_pointer": block.parent_pointer,
        "depth": block.depth,
        "index": block.index,
        "attrs": block.attrs,
    });
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
    if block.children_truncated {
        entry["children_truncated"] = json!(true);
    }
    entry
}
