//! `insum read` — whole-note reading: JSON envelope or annotated projection
//! (spec §6.5). The projection is read-only text with pointer anchors; it is
//! never a storage format and has no parser.

use std::path::Path;

use serde_json::{Value, json};

use insum_core::model::BlockEntry;
use insum_core::{ErrorCode, Limits, UmError};

use crate::cursor::Cursor;
use crate::support::{Output, read_views, render_path};

pub fn run(
    path: &Path,
    note: Option<&str>,
    format: &str,
    max_bytes: Option<usize>,
    cursor_token: Option<&str>,
    limits: &Limits,
) -> Result<Output, UmError> {
    match format {
        "json" | "annotated" => {}
        other => {
            return Err(UmError::usage(format!(
                "--format must be `json` or `annotated`, got {other:?}"
            )));
        }
    }
    let rendered = render_path(path);
    let views = read_views(path, note, limits)?;

    if format == "json" {
        let budget = limits.output_bytes(max_bytes);
        let mut notes = Vec::new();
        for view in &views {
            let note_value = json!({
                "note_id": view.note_id,
                "note_title": view.note_title,
                "note_rev": view.note_rev,
                "features": view.features,
                "document": view.doc,
            });
            let size = serde_json::to_string(&note_value).map_or(0, |text| text.len());
            if size + 256 > budget {
                return Err(UmError::new(
                    ErrorCode::LimitExceeded,
                    format!(
                        "note does not fit into the output budget ({budget} bytes, need {size})"
                    ),
                )
                .with_hint(
                    "Raise --max-bytes, or use `insum tree` / `insum read --format annotated` for block-wise pagination.",
                ));
            }
            notes.push(note_value);
        }
        return Ok(Output::Json(json!({
            "api_version": 1,
            "path": rendered,
            "file_rev": file_rev(path, limits)?,
            "notes": notes,
        })));
    }

    // annotated: paginate by top-level blocks (spec §6.5)
    let cursor = match cursor_token {
        Some(token) => Some(Cursor::decode(token)?),
        None => None,
    };
    if let Some(cursor) = &cursor {
        let view = views.get(cursor.note_index);
        cursor.validate(
            "read",
            &rendered,
            view.and_then(|v| v.note_id.as_deref()),
            view.map(|v| v.note_rev.as_str()).unwrap_or_default(),
        )?;
    }
    let start_note = cursor.as_ref().map_or(0, |c| c.note_index);
    let budget = limits.output_bytes(max_bytes);

    let mut out = String::new();
    let mut has_more = false;
    let mut stop_note = 0usize;
    let mut stop_block = 0usize;
    let mut resumed = false;
    for (note_index, view) in views.iter().enumerate().skip(start_note) {
        let start_block = if note_index == start_note && !resumed {
            cursor.as_ref().map_or(0, |c| c.block_index)
        } else {
            0
        };
        resumed = true;
        let header = format!(
            "<!-- um:api_version=1 note_id=\"{}\" note_rev=\"{}\" -->\n",
            view.note_id.clone().unwrap_or_default(),
            view.note_rev
        );
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&header);
        let mut used = out.len();
        let blocks = insum_core::traverse(&view.doc, None, limits.max_depth);
        let top_level: Vec<&BlockEntry> = blocks.iter().filter(|b| b.depth == 0).collect();
        for (block_position, block) in top_level.iter().enumerate().skip(start_block) {
            let mut rendered_block = String::new();
            render_block(block, blocks.as_slice(), &mut rendered_block);
            let chunk = format!("\n{rendered_block}");
            if used + chunk.len() > budget && block_position > start_block {
                // whole blocks only — no silent or partial truncation
                has_more = true;
                stop_note = note_index;
                stop_block = block_position;
                break;
            }
            used += chunk.len();
            out.push_str(&chunk);
        }
        if has_more {
            break;
        }
    }

    if has_more {
        let view = &views[stop_note];
        let cursor = Cursor::read(
            rendered.clone(),
            view.note_id.clone(),
            view.note_rev.clone(),
            stop_note,
            stop_block,
            None,
            0,
        );
        out.push_str(&format!(
            "\n<!-- um:incomplete next_cursor=\"{}\" -->\n",
            cursor.encode()
        ));
    }

    Ok(Output::Text(out))
}

fn file_rev(path: &Path, limits: &Limits) -> Result<String, UmError> {
    match insum_core::container::classify_extension(path) {
        Some("um") => Ok(insum_core::UmContainer::open(path, limits)?.file_rev),
        _ => Ok(insum_core::NoteFile::open(path, limits)?.file_rev),
    }
}

fn render_block(block: &BlockEntry, blocks: &[BlockEntry], out: &mut String) {
    // children of this block, in document order
    let children: Vec<&BlockEntry> = blocks
        .iter()
        .filter(|candidate| candidate.parent_pointer.as_deref() == Some(block.pointer.as_str()))
        .collect();
    let inline = block.text.clone();

    let produces_anchor = inline.is_some();
    if produces_anchor {
        let id_suffix = block
            .id
            .as_deref()
            .map(|id| format!(" um:id=\"{id}\""))
            .unwrap_or_default();
        out.push_str(&format!(
            "<!-- um:pointer=\"{}\"{} -->\n",
            block.pointer, id_suffix
        ));
    }

    match block.node_type.as_str() {
        "heading" => {
            let level = block
                .attrs
                .get("level")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 6) as usize;
            out.push_str(&format!(
                "{} {}\n",
                "#".repeat(level),
                inline.unwrap_or_default()
            ));
        }
        "paragraph" => {
            out.push_str(&inline.unwrap_or_default());
            out.push('\n');
        }
        "noteTitle" => {
            out.push_str(&format!("# {}\n", inline.unwrap_or_default()));
        }
        "noteSummary" => {
            out.push_str(&inline.unwrap_or_default());
            out.push('\n');
        }
        "noteProperty" => {
            // name + values live in child blocks
            let name = children
                .iter()
                .find(|child| child.node_type == "notePropertyName")
                .and_then(|child| child.text.clone())
                .unwrap_or_default();
            let value: Vec<String> = children
                .iter()
                .filter(|child| child.node_type == "notePropertyValue")
                .filter_map(|child| child.text.clone())
                .collect();
            out.push_str(&format!("{name}: {}\n", value.join(" ")));
        }
        "bulletList" | "taskList" => {
            for child in &children {
                render_list_item(child, blocks, "- ", "  ", out);
            }
        }
        "orderedList" => {
            let start = block
                .attrs
                .get("start")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            for (offset, child) in children.iter().enumerate() {
                let marker = format!("{}. ", start + offset as u64);
                render_list_item(child, blocks, &marker, "   ", out);
            }
        }
        "blockquote" => {
            let mut inner = String::new();
            for child in &children {
                render_block(child, blocks, &mut inner);
            }
            for line in inner.lines() {
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
        }
        "hljsCodeBlock" => {
            let language = block
                .attrs
                .get("class")
                .and_then(Value::as_str)
                .and_then(|class| class.strip_prefix("language-"))
                .unwrap_or("");
            out.push_str("```");
            out.push_str(language);
            out.push('\n');
            for child in &children {
                if let Some(text) = &child.text {
                    out.push_str(text);
                    out.push('\n');
                }
            }
            out.push_str("```\n");
        }
        "mermaid" => {
            out.push_str("```mermaid\n");
            if let Some(code) = block.attrs.get("code").and_then(Value::as_str) {
                out.push_str(code);
                out.push('\n');
            }
            out.push_str("```\n");
        }
        "horizontalRule" => {
            out.push_str("---\n");
        }
        _ => {
            // known-but-unrendered or unknown nodes stay explicit (spec §6.5)
            out.push_str(&format!(
                "<!-- um:unrendered type=\"{}\" pointer=\"{}\" -->\n",
                block.node_type, block.pointer
            ));
        }
    }
}

fn render_list_item(
    item: &BlockEntry,
    blocks: &[BlockEntry],
    marker: &str,
    indent: &str,
    out: &mut String,
) {
    let children: Vec<&BlockEntry> = blocks
        .iter()
        .filter(|candidate| candidate.parent_pointer.as_deref() == Some(item.pointer.as_str()))
        .collect();
    let mut first_line = String::new();
    if let Some(task_checked) = item.attrs.get("checked") {
        let checked = task_checked.as_bool().unwrap_or(false);
        first_line.push_str(if checked { "- [x] " } else { "- [ ] " });
    } else {
        first_line.push_str(marker);
    }
    for (position, child) in children.iter().enumerate() {
        if position == 0 {
            if let Some(text) = &child.text {
                first_line.push_str(text);
            }
            out.push_str(&first_line);
            out.push('\n');
            if child.node_type == "paragraph" {
                continue;
            }
        }
        let mut nested = String::new();
        render_block(child, blocks, &mut nested);
        for line in nested.lines() {
            out.push_str(indent);
            out.push_str(line);
            out.push('\n');
        }
    }
}
