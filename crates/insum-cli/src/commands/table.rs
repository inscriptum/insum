//! `insum table get` — structured read of one table of a note.
//!
//! Read-only: the file is opened for reading only. The addressing follows
//! the `block get` conventions (`--note` required for `.um`, forbidden for
//! `.note`, W-09 ambiguity signalled structurally via `details`). The
//! header row of the projection is the table's first row; the response
//! carries explicit diagnostics for ragged rows, duplicate header names,
//! colspan/rowspan, nested tables and unknown node types — the data itself
//! is never dropped or reinterpreted silently.

use std::path::Path;

use serde_json::{Value, json};

use insum_core::tables::ColumnLookup;
use insum_core::{ErrorCode, Limits, TableEntry, UmError, tables};

use crate::support::{NoteView, Output, Revs, escape_comment_value, read_notes, render_path};

/// The parsed `table get` arguments (bundled to keep the run signature
/// within the arity lint).
pub struct TableGet<'a> {
    pub path: &'a Path,
    pub note: Option<&'a str>,
    pub index: Option<usize>,
    pub id: Option<&'a str>,
    pub after_heading: Option<&'a str>,
    pub where_filter: Option<&'a str>,
    pub format: &'a str,
    pub max_bytes: Option<usize>,
}

pub fn run(options: &TableGet<'_>, limits: &Limits) -> Result<Output, UmError> {
    let TableGet {
        path,
        note,
        index,
        id,
        after_heading,
        where_filter,
        format,
        max_bytes,
    } = *options;
    match format {
        "json" | "markdown" => {}
        other => {
            return Err(UmError::usage(format!(
                "--format must be `json` or `markdown`, got {other:?}"
            )));
        }
    }
    let addressed = [index.is_some(), id.is_some(), after_heading.is_some()]
        .iter()
        .filter(|set| **set)
        .count();
    if addressed != 1 {
        return Err(UmError::usage(
            "table get requires exactly one of --index <n>, --id <block-id> or --after-heading <text>",
        )
        .with_hint("The three ways to address a table are mutually exclusive."));
    }

    let rendered = render_path(path);
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match extension.as_deref() {
        Some("um") if note.is_none() => {
            return Err(UmError::usage(
                "table get on a .um container requires --note <id>",
            ));
        }
        Some("note") if note.is_some() => {
            return Err(UmError::usage("--note is not allowed for a .note file"));
        }
        _ => {}
    }

    let filters = note.map(|n| vec![n.to_owned()]);
    let mut views = read_notes(path, filters.as_deref(), limits, Revs::Compute)?.views;
    let view = views
        .pop()
        .ok_or_else(|| UmError::new(ErrorCode::NoteNotFound, "the container has no notes"))?;

    let collected = tables::collect_tables(&view.doc, limits.max_depth);
    if collected.depth_exceeded {
        return Err(UmError::new(
            ErrorCode::LimitExceeded,
            format!("document deeper than UM_MAX_DEPTH ({})", limits.max_depth),
        ));
    }
    let table_count = collected.entries.len();
    let (table_index, entry) =
        address_table(&collected.entries, table_count, index, id, after_heading)?;

    let mut warnings: Vec<Value> = entry
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.to_value())
        .collect();
    if id.is_some() && !view.block_ids_enabled {
        // §4.6 п.4 read-by-ID-on-presence, block get parity: the answer
        // works but the note's IDs are not guaranteed — never a silent one
        warnings.insert(
            0,
            json!({
                "code": "BLOCK_IDS_NOT_GUARANTEED",
                "message": "IDs not guaranteed: the note carries no features.block_ids marker (run `insum migrate-ids` to verify the IDs and stamp the marker)",
            }),
        );
    }

    // --where: exact cell-text match on one column. WHY strict: any
    // duplicated header name refuses the filter even when the queried name
    // is unique — the lookup must be unambiguous against the table as it
    // is, not against the one column the caller happened to pick.
    let matched: Vec<usize> = match where_filter {
        None => (1..entry.rows.len()).collect(),
        Some(spec) => {
            let (column, value) = parse_where(spec)?;
            let duplicates = entry.duplicate_column_names();
            if !duplicates.is_empty() {
                return Err(duplicate_columns_error(&duplicates));
            }
            match entry.lookup_column(column) {
                ColumnLookup::Found(column_index) => Ok((1..entry.rows.len())
                    .filter(|row_index| {
                        entry.rows[*row_index]
                            .cells
                            .get(column_index)
                            .and_then(|cell| cell.text.as_deref())
                            == Some(value)
                    })
                    .collect()),
                ColumnLookup::Ambiguous(indices) => {
                    let pointers: Vec<&str> = indices
                        .iter()
                        .map(|&i| entry.columns()[i].pointer.as_str())
                        .collect();
                    Err(UmError::new(
                        ErrorCode::BlockNotFound,
                        format!(
                            "column {column:?} is ambiguous: {} columns of the table at {} share this name",
                            indices.len(),
                            entry.pointer
                        ),
                    )
                    .with_hint("Rename one of the columns, or address the table without --where.")
                    .with_details(json!({ "ambiguous": true, "pointers": pointers })))
                }
                ColumnLookup::NotFound => {
                    let names: Vec<String> = entry
                        .columns()
                        .iter()
                        .filter_map(|cell| cell.text.clone())
                        .collect();
                    Err(UmError::new(
                        ErrorCode::BlockNotFound,
                        format!(
                            "column {column:?} is not a column of the table at {}",
                            entry.pointer
                        ),
                    )
                    .with_hint(format!(
                        "Columns of this table: {}.",
                        if names.is_empty() {
                            "(none)".to_owned()
                        } else {
                            names.join(", ")
                        }
                    )))
                }
            }?
        }
    };
    let rows_source = entry.rows.len().saturating_sub(1);
    let rows_matched = matched.len();
    let budget = limits.output_bytes(max_bytes);

    if format == "markdown" {
        Ok(Output::Text(render_markdown(
            entry, &matched, &warnings, budget,
        )))
    } else {
        Ok(Output::Json(render_json(
            &rendered,
            &view,
            entry,
            table_index,
            table_count,
            &matched,
            &warnings,
            rows_source,
            rows_matched,
            budget,
        )))
    }
}

/// Resolve one addressing option to a table entry; returns the
/// document-order index along with the entry.
fn address_table<'a>(
    entries: &'a [TableEntry],
    table_count: usize,
    index: Option<usize>,
    id: Option<&str>,
    after_heading: Option<&str>,
) -> Result<(usize, &'a TableEntry), UmError> {
    if let Some(index) = index {
        return entries.get(index).map(|entry| (index, entry)).ok_or_else(|| {
            UmError::new(
                ErrorCode::BlockNotFound,
                format!(
                    "no table at index {index}: this note holds {table_count} tables (indexes are 0-based in document order)"
                ),
            )
            .with_hint(
                "Run `insum table get` with a smaller --index, or address by --id / --after-heading.",
            )
        });
    }
    if let Some(id) = id {
        let matches: Vec<(usize, &TableEntry)> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.id.as_deref() == Some(id))
            .collect();
        return match matches.as_slice() {
            [(position, one)] => Ok((*position, one)),
            [] => Err(UmError::new(
                ErrorCode::BlockNotFound,
                format!("table id {id:?} does not address a table of this note"),
            )
            .with_hint(
                "Run `insum tree` to see the block ids of this note revision, or use --index / --after-heading.",
            )),
            many => {
                let pointers: Vec<&str> = many
                    .iter()
                    .map(|(_, entry)| entry.pointer.as_str())
                    .collect();
                Err(UmError::new(
                    ErrorCode::BlockIdDuplicate,
                    format!(
                        "table id {id:?} is duplicated: {} tables carry it",
                        many.len()
                    ),
                )
                .with_details(json!({ "pointers": pointers })))
            }
        };
    }
    let heading = after_heading.expect("exactly one addressing option");
    let matches: Vec<(usize, &TableEntry)> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.heading.as_deref() == Some(heading))
        .collect();
    match matches.as_slice() {
        [(position, one)] => Ok((*position, one)),
        [] => Err(UmError::new(
            ErrorCode::BlockNotFound,
            format!("no table follows a heading with the exact text {heading:?}"),
        )
        .with_hint(
            "The heading text must match exactly; use --index or --id when the heading is unknown.",
        )),
        many => {
            let pointers: Vec<&str> = many
                .iter()
                .map(|(_, entry)| entry.pointer.as_str())
                .collect();
            Err(UmError::new(
                ErrorCode::BlockNotFound,
                format!(
                    "--after-heading {heading:?} is ambiguous: {} tables follow headings with this text",
                    many.len()
                ),
            )
            .with_hint("Address the table by --index or --id instead.")
            .with_details(json!({ "ambiguous": true, "pointers": pointers })))
        }
    }
}

/// `--where COL=VALUE`: split on the first `=`, the value may contain `=`.
fn parse_where(spec: &str) -> Result<(&str, &str), UmError> {
    let Some((column, value)) = spec.split_once('=') else {
        return Err(
            UmError::usage("--where must be COL=VALUE (exact cell-text match)")
                .with_hint("Example: --where State=Todo."),
        );
    };
    if column.is_empty() {
        return Err(UmError::usage("--where column name is empty"));
    }
    Ok((column, value))
}

fn duplicate_columns_error(duplicates: &[String]) -> UmError {
    UmError::new(
        ErrorCode::BlockNotFound,
        format!(
            "the table has duplicate column names ({}); --where requires unique column names",
            duplicates.join(", ")
        ),
    )
    .with_hint("Drop --where to read the rows positionally, or fix the duplicated headers.")
    .with_details(json!({ "ambiguous": true, "duplicates": duplicates }))
}

/// Column entries of the JSON projection: name (null when withheld), id,
/// pointer.
fn columns_value(entry: &TableEntry) -> Value {
    Value::Array(
        entry
            .columns()
            .iter()
            .map(|cell| {
                json!({
                    "name": cell.text.clone(),
                    "id": cell.id,
                    "pointer": cell.pointer,
                })
            })
            .collect(),
    )
}

/// Whether the per-row `values` map can be built: every column name is
/// present and unique — otherwise the map would silently collapse names.
fn values_are_addressable(entry: &TableEntry) -> bool {
    let columns = entry.columns();
    !columns.is_empty()
        && columns.iter().all(|cell| cell.text.is_some())
        && entry.duplicate_column_names().is_empty()
}

fn row_value(entry: &TableEntry, row_index: usize, values: bool) -> Value {
    let row = &entry.rows[row_index];
    let cells: Vec<Value> = row
        .cells
        .iter()
        .map(|cell| {
            json!({
                "id": cell.id,
                "pointer": cell.pointer,
                "text": cell.text,
            })
        })
        .collect();
    let values = if values {
        let mut map = serde_json::Map::new();
        for (index, column) in entry.columns().iter().enumerate() {
            let name = column.text.clone().expect("checked addressable");
            let text = row
                .cells
                .get(index)
                .and_then(|cell| cell.text.clone())
                .map(Value::String)
                .unwrap_or(Value::Null);
            map.insert(name, text);
        }
        Value::Object(map)
    } else {
        Value::Null
    };
    json!({
        "id": row.id,
        "pointer": row.pointer,
        "cells": cells,
        "values": values,
    })
}

#[allow(clippy::too_many_arguments)]
fn render_json(
    rendered: &str,
    view: &NoteView,
    entry: &TableEntry,
    table_index: usize,
    table_count: usize,
    matched: &[usize],
    warnings: &[Value],
    rows_source: usize,
    rows_matched: usize,
    budget: usize,
) -> Value {
    let addressable = values_are_addressable(entry);
    let row_values: Vec<Value> = matched
        .iter()
        .map(|&index| row_value(entry, index, addressable))
        .collect();
    let sizes: Vec<usize> = row_values
        .iter()
        .map(|row| serde_json::to_string(row).map_or(0, |text| text.len() + 1))
        .collect();
    let rows_total = row_values.len();

    let build = |rows: &[Value], truncated: bool| {
        build_response(
            rendered,
            view,
            entry,
            table_index,
            table_count,
            rows,
            warnings,
            rows_source,
            rows_matched,
            truncated,
            budget,
            rows_total,
        )
    };

    // Measure against the LARGER (truncated) skeleton, so the fitting never
    // under-reserves the OUTPUT_BUDGET warning (the budget discipline of
    // migrate/apply: the skeleton carries its own truncation texts).
    let mut used = serde_json::to_string(&build(&[], true))
        .map(|text| text.len())
        .unwrap_or(usize::MAX);
    let mut kept: Vec<Value> = Vec::new();
    for (position, (row, size)) in row_values.iter().zip(&sizes).enumerate() {
        // The first row is always emitted whole (the documented oversized
        // first-entry exception, same rule as `tree`/migrate); after it,
        // only whole rows that fit.
        if position > 0 && used + *size > budget {
            break;
        }
        used += *size;
        kept.push(row.clone());
    }

    let complete = kept.len() == rows_total;
    let mut response = build(&kept, !complete);
    // The warning text was built with the final counts — when the digits
    // grew the response could cross the budget; drop rows until it holds.
    while !complete
        && kept.len() > 1
        && serde_json::to_string(&response).map_or(usize::MAX, |text| text.len()) > budget
    {
        kept.pop();
        response = build(&kept, true);
    }
    response
}

#[allow(clippy::too_many_arguments)]
fn build_response(
    rendered: &str,
    view: &NoteView,
    entry: &TableEntry,
    table_index: usize,
    table_count: usize,
    rows: &[Value],
    warnings: &[Value],
    rows_source: usize,
    rows_matched: usize,
    truncated: bool,
    budget: usize,
    rows_total: usize,
) -> Value {
    let mut warnings = warnings.to_vec();
    if truncated {
        warnings.push(json!({
            "code": "OUTPUT_BUDGET",
            "message": format!(
                "the table response holds {}/{rows_total} rows to fit the output budget ({budget} bytes); raise --max-bytes, or narrow the result with --where",
                rows.len()
            ),
        }));
    }
    json!({
        "api_version": 1,
        "path": rendered,
        "note_id": view.note_id,
        "note_rev": view.note_rev,
        "table": {
            "pointer": entry.pointer,
            "id": entry.id,
            "index": table_index,
            "table_count": table_count,
            "heading": entry.heading,
            "columns": columns_value(entry),
            "rows": rows,
            "rows_source": rows_source,
            "rows_matched": rows_matched,
        },
        "warnings": warnings,
        "complete": !truncated,
        "truncated": truncated,
    })
}

fn render_markdown(
    entry: &TableEntry,
    matched: &[usize],
    warnings: &[Value],
    budget: usize,
) -> String {
    // The GFM shaping lives in insum-core alone (CONTRACT: one renderer):
    // this function only measures and fits the core-rendered lines into the
    // output budget.
    let header = tables::markdown_header(entry);
    let row_lines: Vec<String> = if header.is_empty() {
        // markdown cannot represent a column-less table — the rows stay in
        // the JSON projection, the diagnostics explain the shape
        Vec::new()
    } else {
        matched
            .iter()
            .map(|&index| tables::markdown_row(entry, index))
            .collect()
    };
    let lines: Vec<String> = if header.is_empty() {
        Vec::new()
    } else {
        vec![header]
    };
    let warning_comments = || -> String {
        let mut text = String::new();
        for warning in warnings {
            let code = warning.get("code").and_then(Value::as_str).unwrap_or("");
            let message = warning.get("message").and_then(Value::as_str).unwrap_or("");
            text.push_str(&format!(
                "<!-- um:warning code=\"{}\" message=\"{}\" -->\n",
                escape_comment_value(code),
                escape_comment_value(message)
            ));
        }
        text
    };

    // Same discipline as the JSON projection: fit whole row lines after the
    // skeleton (header, separator, diagnostics and the OUTPUT_BUDGET comment
    // reserved at its larger size); the first row is always emitted whole.
    let rows_total = row_lines.len();
    let build = |kept: usize| -> String {
        let mut out = lines.concat();
        for line in row_lines.iter().take(kept) {
            out.push_str(line);
        }
        out.push_str(&warning_comments());
        if kept < rows_total {
            out.push_str(&format!(
                "<!-- um:warning code=\"OUTPUT_BUDGET\" message=\"{}\" -->\n",
                escape_comment_value(&format!(
                    "the table response holds {kept}/{rows_total} rows to fit the output budget ({budget} bytes); raise --max-bytes, or narrow the result with --where"
                ))
            ));
        }
        out
    };
    let mut used = build(0).len();
    let mut kept = 0usize;
    for (position, line) in row_lines.iter().enumerate() {
        if position > 0 && used + line.len() > budget {
            break;
        }
        used += line.len();
        kept += 1;
    }
    let mut out = build(kept);
    while kept > 1 && out.len() > budget {
        kept -= 1;
        out = build(kept);
    }
    out
}
