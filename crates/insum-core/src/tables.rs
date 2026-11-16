//! Structured read model of ProseMirror tables (`table` / `tableRow` /
//! `tableHeader` / `tableCell`) — the traversal behind `insum table get`.
//!
//! The walk collects every table of a document in document order (preorder),
//! each with the text of its nearest preceding heading and its rows/cells
//! (pointers, IDs, values). Cell text follows the same discipline as
//! `tree`/`search` (one inline-text rule): text is withheld entirely when an
//! unknown node type makes it unknowable — no partial, silent text.
//! Structural anomalies (ragged rows, duplicate header names, colspan/
//! rowspan, nested tables) are explicit diagnostics; the read itself never
//! fails on them (§10.2: warnings, not data loss).

use serde_json::{Map, Value};

use crate::model::{ID_ATTR, INLINE_NODE_TYPES, KNOWN_NODE_TYPES, inline_text};

/// One structural diagnostic of the walk (§10.2 warnings). Error codes stay
/// in the registry; these describe data properties the projection preserves
/// with an explicit note instead of a silent interpretation.
#[derive(Debug, Clone)]
pub struct TableDiagnostic {
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
}

impl TableDiagnostic {
    pub fn to_value(&self) -> Value {
        let mut value = serde_json::json!({
            "code": self.code,
            "message": self.message,
        });
        if let Some(details) = &self.details {
            value["details"] = details.clone();
        }
        value
    }

    fn new(code: &'static str, message: String) -> Self {
        Self {
            code,
            message,
            details: None,
        }
    }

    fn with_details(code: &'static str, message: String, details: Value) -> Self {
        Self {
            code,
            message,
            details: Some(details),
        }
    }
}

/// One cell of a table row.
#[derive(Debug, Clone)]
pub struct TableCell {
    pub pointer: String,
    pub id: Option<String>,
    /// `tableCell` or `tableHeader` as stored.
    pub node_type: String,
    /// `attrs.colspan` / `attrs.rowspan`; missing or non-integer reads as 1.
    pub colspan: u64,
    pub rowspan: u64,
    /// Aggregated cell text; `None` when the subtree withholds text
    /// (unknown node type — the same rule as `tree`/`search`).
    pub text: Option<String>,
}

/// One row of a table.
#[derive(Debug, Clone)]
pub struct TableRow {
    pub pointer: String,
    pub id: Option<String>,
    pub cells: Vec<TableCell>,
}

/// One table of a document, in document order.
///
/// The header row of the projection is the FIRST row — the GFM convention
/// the annotated projection already renders by. `tableHeader` cell types do
/// NOT select the header: real kanbans store `tableHeader` cells in data
/// rows (observed in issues.um), so the type is not a reliable header marker.
#[derive(Debug, Clone)]
pub struct TableEntry {
    pub pointer: String,
    pub id: Option<String>,
    /// Text of the nearest preceding heading (or noteTitle — the semantic
    /// title node) in document order; `None` when no heading precedes or its
    /// text is withheld.
    pub heading: Option<String>,
    pub rows: Vec<TableRow>,
    pub diagnostics: Vec<TableDiagnostic>,
}

impl TableEntry {
    /// The header row cells (the first row); empty for a row-less table.
    pub fn columns(&self) -> &[TableCell] {
        self.rows
            .first()
            .map(|row| row.cells.as_slice())
            .unwrap_or(&[])
    }

    /// Names appearing on two or more header cells (first-occurrence order).
    /// Withheld names (`None`) cannot be queried and are not listed.
    pub fn duplicate_column_names(&self) -> Vec<String> {
        duplicate_names_of(self.columns())
    }

    /// Column lookup by exact name: unique, ambiguous, or absent.
    pub fn lookup_column(&self, name: &str) -> ColumnLookup {
        let matches: Vec<usize> = self
            .columns()
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.text.as_deref() == Some(name))
            .map(|(index, _)| index)
            .collect();
        match matches.as_slice() {
            [] => ColumnLookup::NotFound,
            [one] => ColumnLookup::Found(*one),
            many => ColumnLookup::Ambiguous(many.to_vec()),
        }
    }
}

/// Result of a column lookup by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnLookup {
    Found(usize),
    NotFound,
    Ambiguous(Vec<usize>),
}

/// All tables of one document, in document order.
#[derive(Debug, Default)]
pub struct Tables {
    pub entries: Vec<TableEntry>,
    /// True when the document is deeper than the enforced JSON depth limit
    /// (UM_MAX_DEPTH) — callers must fail with `LIMIT_EXCEEDED`.
    pub depth_exceeded: bool,
}

/// Collect every table of the document in document order (preorder).
pub fn collect_tables(doc: &Value, max_json_depth: usize) -> Tables {
    let mut tables = Tables::default();
    let Some(children) = doc.get("content").and_then(Value::as_array) else {
        return tables;
    };
    let mut heading = HeadingContext::default();
    for (index, child) in children.iter().enumerate() {
        walk(
            child,
            None,
            index,
            0,
            &mut heading,
            max_json_depth,
            &mut tables,
        );
    }
    tables
}

fn walk(
    node: &Value,
    parent_pointer: Option<&str>,
    index: usize,
    json_depth: usize,
    heading: &mut HeadingContext,
    max_json_depth: usize,
    tables: &mut Tables,
) {
    if json_depth > max_json_depth {
        tables.depth_exceeded = true;
        return;
    }
    let Some(object) = node.as_object() else {
        return;
    };
    let node_type = object.get("type").and_then(Value::as_str).unwrap_or("");
    if INLINE_NODE_TYPES.contains(&node_type) {
        return;
    }
    let pointer = match parent_pointer {
        None => format!("/content/{index}"),
        Some(parent) => format!("{parent}/content/{index}"),
    };
    match node_type {
        // The page title node counts as a heading (the same rule as the
        // note-title fallback): tables under a title-schema page have the
        // title as their first heading context.
        "heading" | "noteTitle" => {
            *heading = heading_context(object, &pointer, json_depth, max_json_depth, tables);
        }
        "table" => {
            let mut entry = collect_table(
                object,
                &pointer,
                heading.text.clone(),
                max_json_depth,
                tables,
            );
            // A withheld heading context explains itself: the table's
            // `heading: null` must not be mistaken for "no heading above"
            // (no silent context loss — §10.2).
            let heading_warnings: Vec<TableDiagnostic> = heading
                .withheld
                .iter()
                .map(|(diag_pointer, node_type)| {
                    TableDiagnostic::new(
                        "UNKNOWN_NODE_TYPE",
                        format!(
                            "unknown node type {node_type:?} at {diag_pointer}: the heading text is withheld — --after-heading cannot match this heading"
                        ),
                    )
                })
                .collect();
            entry.diagnostics.splice(0..0, heading_warnings);
            tables.entries.push(entry);
        }
        _ => {
            if let Some(children) = object.get("content").and_then(Value::as_array) {
                for (child_index, child) in children.iter().enumerate() {
                    walk(
                        child,
                        Some(&pointer),
                        child_index,
                        json_depth + 1,
                        heading,
                        max_json_depth,
                        tables,
                    );
                }
            }
        }
    }
}

/// Heading context of subsequent tables: the exact inline text when
/// extractable, or the unknown-node evidence when the text is withheld —
/// a `heading: null` must be distinguishable from "no heading above".
#[derive(Default, Clone)]
struct HeadingContext {
    text: Option<String>,
    /// Unknown (pointer, type) pairs of the last heading when its text is
    /// withheld; empty when no heading precedes or the text is plain.
    withheld: Vec<(String, String)>,
}

fn heading_context(
    object: &Map<String, Value>,
    pointer: &str,
    json_depth: usize,
    max_json_depth: usize,
    tables: &mut Tables,
) -> HeadingContext {
    let (text, incomplete) = inline_text(object);
    if !incomplete {
        return HeadingContext {
            text: Some(text.unwrap_or_default()),
            withheld: Vec::new(),
        };
    }
    // Reuse the cell subtree walk to NAME the unknown nodes of the heading
    // (headings hold inline nodes — no nested tables to expect here).
    let mut ctx = CellContext::default();
    gather_object(
        object,
        pointer,
        json_depth,
        max_json_depth,
        tables,
        &mut ctx,
    );
    HeadingContext {
        text: None,
        withheld: ctx.unknown,
    }
}

fn collect_table(
    object: &Map<String, Value>,
    pointer: &str,
    heading: Option<String>,
    max_json_depth: usize,
    tables: &mut Tables,
) -> TableEntry {
    let id = object
        .get("attrs")
        .and_then(|attrs| attrs.get(ID_ATTR))
        .and_then(Value::as_str)
        .map(str::to_owned);

    let mut rows: Vec<TableRow> = Vec::new();
    let mut walked_diagnostics: Vec<TableDiagnostic> = Vec::new();
    let mut nested_pointers: Vec<String> = Vec::new();
    let mut unknown: Vec<(String, String)> = Vec::new();

    let children = object.get("content").and_then(Value::as_array);
    for (index, child) in children.into_iter().flatten().enumerate() {
        let row_pointer = format!("{pointer}/content/{index}");
        let Some(row_object) = child.as_object() else {
            walked_diagnostics.push(TableDiagnostic::new(
                "TABLE_STRUCTURE",
                format!("table child at {row_pointer} is not a node object — skipped"),
            ));
            continue;
        };
        let row_type = row_object.get("type").and_then(Value::as_str).unwrap_or("");
        if row_type != "tableRow" {
            if KNOWN_NODE_TYPES.contains(&row_type) {
                walked_diagnostics.push(TableDiagnostic::new(
                    "TABLE_STRUCTURE",
                    format!(
                        "table child at {row_pointer} is {row_type:?}, not a tableRow — skipped"
                    ),
                ));
            } else {
                unknown.push((row_pointer.clone(), row_type.to_owned()));
            }
            continue;
        }
        let row_id = row_object
            .get("attrs")
            .and_then(|attrs| attrs.get(ID_ATTR))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut cells: Vec<TableCell> = Vec::new();
        let row_children = row_object.get("content").and_then(Value::as_array);
        for (cell_index, cell_node) in row_children.into_iter().flatten().enumerate() {
            let cell_pointer = format!("{row_pointer}/content/{cell_index}");
            let Some(cell_object) = cell_node.as_object() else {
                walked_diagnostics.push(TableDiagnostic::new(
                    "TABLE_STRUCTURE",
                    format!("row child at {cell_pointer} is not a node object — skipped"),
                ));
                continue;
            };
            let cell_type = cell_object
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("");
            if cell_type != "tableCell" && cell_type != "tableHeader" {
                if KNOWN_NODE_TYPES.contains(&cell_type) {
                    walked_diagnostics.push(TableDiagnostic::new(
                        "TABLE_STRUCTURE",
                        format!(
                            "row child at {cell_pointer} is {cell_type:?}, not a tableCell/tableHeader — skipped"
                        ),
                    ));
                } else {
                    unknown.push((cell_pointer.clone(), cell_type.to_owned()));
                }
                continue;
            }
            let attrs_span = |name: &str| {
                cell_object
                    .get("attrs")
                    .and_then(|attrs| attrs.get(name))
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
            };
            let mut ctx = CellContext::default();
            gather_cell_text(
                cell_node,
                &cell_pointer,
                0,
                max_json_depth,
                tables,
                &mut ctx,
            );
            if let Some(nested) = ctx.nested {
                nested_pointers.push(nested);
            }
            unknown.extend(ctx.unknown);
            cells.push(TableCell {
                pointer: cell_pointer,
                id: cell_object
                    .get("attrs")
                    .and_then(|attrs| attrs.get(ID_ATTR))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                node_type: cell_type.to_owned(),
                colspan: attrs_span("colspan"),
                rowspan: attrs_span("rowspan"),
                text: if ctx.incomplete {
                    None
                } else {
                    Some(ctx.texts.join("\n"))
                },
            });
        }
        rows.push(TableRow {
            pointer: row_pointer,
            id: row_id,
            cells,
        });
    }

    let mut diagnostics: Vec<TableDiagnostic> = Vec::new();
    let columns: Vec<TableCell> = rows
        .first()
        .map(|row| row.cells.clone())
        .unwrap_or_default();
    let width = columns.len();

    if rows.is_empty() {
        diagnostics.push(TableDiagnostic::new(
            "TABLE_EMPTY",
            format!("table at {pointer} has no rows"),
        ));
    } else if !columns.iter().any(|cell| cell.node_type == "tableHeader") {
        // The first row is projected as the header whatever its cell types
        // are; an unmarked header means the names were not written as
        // headers — say so instead of implying a vouched-for header.
        diagnostics.push(TableDiagnostic::new(
            "TABLE_HEADER_UNMARKED",
            format!(
                "the header row (first row) of the table at {pointer} holds no tableHeader cells — its texts are projected as column names anyway"
            ),
        ));
    }
    for name in duplicate_names_of(&columns) {
        let pointers: Vec<&str> = columns
            .iter()
            .filter(|cell| cell.text.as_deref() == Some(name.as_str()))
            .map(|cell| cell.pointer.as_str())
            .collect();
        diagnostics.push(TableDiagnostic::with_details(
            "TABLE_HEADER_DUPLICATE",
            format!(
                "column name {name:?} appears {} times in the header row of the table at {pointer}",
                pointers.len()
            ),
            serde_json::json!({ "name": name, "pointers": pointers }),
        ));
    }
    for (diag_pointer, node_type) in &unknown {
        walked_diagnostics.push(TableDiagnostic::new(
            "UNKNOWN_NODE_TYPE",
            format!("unknown node type {node_type:?} at {diag_pointer} (preserved as-is; surrounding cell text is withheld)"),
        ));
    }
    for row in rows.iter().skip(1) {
        if row.cells.len() != width {
            diagnostics.push(TableDiagnostic::with_details(
                "TABLE_ROW_RAGGED",
                format!(
                    "row at {} holds {} cells, the header defines {} columns",
                    row.pointer,
                    row.cells.len(),
                    width
                ),
                serde_json::json!({
                    "row_pointer": row.pointer,
                    "cells": row.cells.len(),
                    "columns": width,
                }),
            ));
        }
    }
    let spans: Vec<Value> = rows
        .iter()
        .flat_map(|row| row.cells.iter())
        .filter(|cell| cell.colspan > 1 || cell.rowspan > 1)
        .map(|cell| {
            serde_json::json!({
                "pointer": cell.pointer,
                "colspan": cell.colspan,
                "rowspan": cell.rowspan,
            })
        })
        .collect();
    if !spans.is_empty() {
        diagnostics.push(TableDiagnostic::with_details(
            "TABLE_SPAN",
            format!(
                "{} cell(s) of the table at {pointer} carry colspan/rowspan; the projection keeps them as stored cells without expanding the grid",
                spans.len()
            ),
            serde_json::json!({ "cells": spans }),
        ));
    }
    if !nested_pointers.is_empty() {
        diagnostics.push(TableDiagnostic::with_details(
            "TABLE_NESTED",
            format!(
                "{} nested table(s) inside the cells of the table at {pointer}; their content is not aggregated into the outer cell text",
                nested_pointers.len()
            ),
            serde_json::json!({ "pointers": nested_pointers }),
        ));
    }
    diagnostics.extend(walked_diagnostics);

    TableEntry {
        pointer: pointer.to_owned(),
        id,
        heading,
        rows,
        diagnostics,
    }
}

fn duplicate_names_of(columns: &[TableCell]) -> Vec<String> {
    let mut duplicates: Vec<String> = Vec::new();
    for (index, cell) in columns.iter().enumerate() {
        let Some(name) = cell.text.as_deref() else {
            continue;
        };
        if duplicates.iter().any(|d| d == name) {
            continue;
        }
        if columns[index + 1..]
            .iter()
            .any(|other| other.text.as_deref() == Some(name))
        {
            duplicates.push(name.to_owned());
        }
    }
    duplicates
}

/// Aggregated text of one cell subtree (preorder): text-bearing blocks
/// contribute their inline text, containers are descended, nested tables are
/// excluded (their content is not the outer cell's value), unknown node
/// types withhold the whole cell text.
#[derive(Default)]
struct CellContext {
    texts: Vec<String>,
    incomplete: bool,
    nested: Option<String>,
    unknown: Vec<(String, String)>,
}

fn gather_cell_text(
    node: &Value,
    pointer: &str,
    json_depth: usize,
    max_json_depth: usize,
    tables: &mut Tables,
    ctx: &mut CellContext,
) {
    if let Some(object) = node.as_object() {
        gather_object(object, pointer, json_depth, max_json_depth, tables, ctx);
    }
}

fn gather_object(
    object: &Map<String, Value>,
    pointer: &str,
    json_depth: usize,
    max_json_depth: usize,
    tables: &mut Tables,
    ctx: &mut CellContext,
) {
    if json_depth > max_json_depth {
        tables.depth_exceeded = true;
        return;
    }
    let node_type = object.get("type").and_then(Value::as_str).unwrap_or("");
    if node_type == "table" {
        // A nested table is a block of its own, not part of the outer cell
        // value; TABLE_NESTED names it.
        ctx.nested.get_or_insert_with(|| pointer.to_owned());
        return;
    }
    if !INLINE_NODE_TYPES.contains(&node_type) && !KNOWN_NODE_TYPES.contains(&node_type) {
        ctx.incomplete = true;
        ctx.unknown.push((pointer.to_owned(), node_type.to_owned()));
        return;
    }
    let (text, incomplete) = inline_text(object);
    if incomplete {
        // The node's own text is withheld — but the unknown nodes that
        // caused it must still be NAMED (heading context, cell warnings):
        // descend instead of returning silently.
        ctx.incomplete = true;
    }
    if let Some(text) = text {
        ctx.texts.push(text);
        return;
    }
    if let Some(children) = object.get("content").and_then(Value::as_array) {
        for (index, child) in children.iter().enumerate() {
            gather_cell_text(
                child,
                &format!("{pointer}/content/{index}"),
                json_depth + 1,
                max_json_depth,
                tables,
                ctx,
            );
        }
    }
}

/// GFM projection of one table: the first row is the header, short rows are
/// padded, long rows keep their extra cells (the ragged diagnostics name the
/// anomaly — no content is dropped here). CR is removed first (deletion-first,
/// review SEC-1: a document-controlled `\r` must not survive the escape and
/// split the GFM row line, carrying a forged marker onto its own line), then
/// newlines become `<br>` and pipes are escaped — byte-for-byte parity with
/// the annotated cell renderer, CR text included.
pub fn escape_cell_text(text: &str) -> String {
    text.replace('\r', "")
        .replace('\n', "<br>")
        .replace('|', "\\|")
}

/// Header and separator lines of the GFM projection, from the table's
/// first row. Empty for a column-less table — markdown cannot represent a
/// column-less table, the JSON projection carries the rows instead.
pub fn markdown_header(entry: &TableEntry) -> String {
    let columns = entry.columns();
    if columns.is_empty() {
        return String::new();
    }
    let header: Vec<String> = columns
        .iter()
        .map(|cell| escape_cell_text(cell.text.as_deref().unwrap_or("")))
        .collect();
    let separator: Vec<&str> = columns.iter().map(|_| "---").collect();
    format!(
        "| {} |\n| {} |\n",
        header.join(" | "),
        separator.join(" | ")
    )
}

/// One DATA row line of the GFM projection (`row_index` addresses the rows
/// of the entry, the header row included): short rows are padded, long rows
/// keep their extra cells (the ragged diagnostics name the anomaly — no
/// content is dropped here). Pipes are escaped and newlines become `<br>`,
/// byte-for-byte parity with the annotated cell renderer.
pub fn markdown_row(entry: &TableEntry, row_index: usize) -> String {
    let columns = entry.columns();
    if columns.is_empty() {
        return String::new();
    }
    let row = &entry.rows[row_index];
    let mut rendered: Vec<String> = row
        .cells
        .iter()
        .map(|cell| escape_cell_text(cell.text.as_deref().unwrap_or("")))
        .collect();
    while rendered.len() < columns.len() {
        rendered.push(String::new());
    }
    format!("| {} |\n", rendered.join(" | "))
}

/// Render the GFM table body of one entry: the header line comes from the
/// first row, `row_indices` selects the DATA rows (never the header row —
/// it is not a filterable data row). The single GFM shaping implementation:
/// `insum table get --format markdown` composes the budget fit from
/// [`markdown_header`] and [`markdown_row`] — do not duplicate the renderer.
pub fn markdown_body(entry: &TableEntry, row_indices: &[usize]) -> String {
    let mut out = markdown_header(entry);
    for index in row_indices {
        out.push_str(&markdown_row(entry, *index));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(content: Vec<Value>) -> Value {
        json!({ "type": "noteDoc", "content": content })
    }

    fn row(cells: Vec<Value>) -> Value {
        json!({ "type": "tableRow", "content": cells })
    }

    fn header(id: &str, text: &str) -> Value {
        json!({
            "type": "tableHeader",
            "attrs": { "id": id, "colspan": 1, "rowspan": 1 },
            "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": text }] }]
        })
    }

    fn cell(id: &str, text: &str) -> Value {
        json!({
            "type": "tableCell",
            "attrs": { "id": id, "colspan": 1, "rowspan": 1 },
            "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": text }] }]
        })
    }

    fn plain_table() -> Value {
        json!({
            "type": "table",
            "attrs": { "id": "tbl00001aa" },
            "content": [
                row(vec![header("h1", "Name"), header("h2", "State")]),
                row(vec![cell("c1", "first"), cell("c2", "Todo")]),
                row(vec![cell("c3", "second"), cell("c4", "Done")]),
            ]
        })
    }

    #[test]
    fn tables_are_collected_in_document_order_with_headings() {
        let document = doc(vec![
            json!({"type": "paragraph", "content": [{"type": "text", "text": "intro"}]}),
            json!({"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "Backlog"}]}),
            plain_table(),
            json!({"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "Todo"}]}),
            plain_table(),
        ]);
        let tables = collect_tables(&document, 128);
        assert_eq!(tables.entries.len(), 2);
        assert!(!tables.depth_exceeded);
        assert_eq!(tables.entries[0].pointer, "/content/2");
        assert_eq!(tables.entries[0].heading.as_deref(), Some("Backlog"));
        assert_eq!(tables.entries[1].pointer, "/content/4");
        assert_eq!(tables.entries[1].heading.as_deref(), Some("Todo"));
        assert_eq!(tables.entries[0].id.as_deref(), Some("tbl00001aa"));
    }

    #[test]
    fn cells_carry_pointers_ids_and_text() {
        let tables = collect_tables(&doc(vec![plain_table()]), 128);
        let entry = &tables.entries[0];
        assert_eq!(entry.rows.len(), 3);
        assert_eq!(entry.rows[0].pointer, "/content/0/content/0");
        assert_eq!(entry.rows[0].id, None);
        let first_cell = &entry.rows[1].cells[0];
        assert_eq!(first_cell.pointer, "/content/0/content/1/content/0");
        assert_eq!(first_cell.id.as_deref(), Some("c1"));
        assert_eq!(first_cell.text.as_deref(), Some("first"));
        assert_eq!(first_cell.colspan, 1);
        assert_eq!(first_cell.node_type, "tableCell");
    }

    #[test]
    fn ragged_rows_and_duplicate_headers_are_diagnosed() {
        let document = doc(vec![json!({
            "type": "table",
            "content": [
                row(vec![header("h1", "A"), header("h2", "B"), header("h3", "A")]),
                row(vec![cell("c1", "one")]),
                row(vec![cell("c2", "x"), cell("c3", "y"), cell("c4", "z"), cell("c5", "extra")]),
            ]
        })]);
        let tables = collect_tables(&document, 128);
        let entry = &tables.entries[0];
        let codes: Vec<&str> = entry.diagnostics.iter().map(|d| d.code).collect();
        assert!(
            codes.contains(&"TABLE_HEADER_DUPLICATE"),
            "codes: {codes:?}"
        );
        assert!(codes.contains(&"TABLE_ROW_RAGGED"), "codes: {codes:?}");
        let dup = entry
            .diagnostics
            .iter()
            .find(|d| d.code == "TABLE_HEADER_DUPLICATE")
            .unwrap();
        assert_eq!(dup.details.as_ref().unwrap()["name"], "A");
        // duplicate names are visible to the filter guard
        assert_eq!(entry.duplicate_column_names(), vec!["A".to_owned()]);
        assert_eq!(entry.lookup_column("B"), ColumnLookup::Found(1));
        assert_eq!(
            entry.lookup_column("A"),
            ColumnLookup::Ambiguous(vec![0, 2])
        );
        assert_eq!(entry.lookup_column("Z"), ColumnLookup::NotFound);
    }

    #[test]
    fn spans_and_nested_tables_and_empty_tables_are_diagnosed() {
        let spanning = json!({
            "type": "tableCell",
            "attrs": { "id": "s1", "colspan": 2, "rowspan": 1 },
            "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": "wide" }] }]
        });
        let nested_table = json!({
            "type": "tableCell",
            "attrs": { "id": "n1" },
            "content": [
                { "type": "paragraph", "content": [{ "type": "text", "text": "outer" }] },
                { "type": "table", "content": [row(vec![cell("inner1", "inner")])] }
            ]
        });
        let document = doc(vec![
            json!({
                "type": "table",
                "content": [row(vec![header("h1", "A"), header("h2", "B")]), row(vec![spanning])]
            }),
            json!({
                "type": "table",
                "content": [row(vec![header("h1", "A"), header("h2", "B")]), row(vec![nested_table])]
            }),
            json!({"type": "table", "content": []}),
        ]);
        let tables = collect_tables(&document, 128);
        assert_eq!(tables.entries.len(), 3);
        let codes0: Vec<&str> = tables.entries[0]
            .diagnostics
            .iter()
            .map(|d| d.code)
            .collect();
        assert!(codes0.contains(&"TABLE_SPAN"), "codes: {codes0:?}");
        let codes1: Vec<&str> = tables.entries[1]
            .diagnostics
            .iter()
            .map(|d| d.code)
            .collect();
        assert!(codes1.contains(&"TABLE_NESTED"), "codes: {codes1:?}");
        // the nested table's text is not aggregated into the outer cell
        let nested_cell = &tables.entries[1].rows[1].cells[0];
        assert_eq!(nested_cell.text.as_deref(), Some("outer"));
        // the nested table itself is not a separate entry
        assert!(tables.entries[1].rows[1].cells.len() == 1);
        let codes2: Vec<&str> = tables.entries[2]
            .diagnostics
            .iter()
            .map(|d| d.code)
            .collect();
        assert!(codes2.contains(&"TABLE_EMPTY"), "codes: {codes2:?}");
    }

    #[test]
    fn unknown_node_type_withholds_cell_text() {
        let document = doc(vec![json!({
            "type": "table",
            "content": [
                row(vec![header("h1", "A")]),
                row(vec![json!({
                    "type": "tableCell",
                    "attrs": { "id": "c1" },
                    "content": [
                        { "type": "paragraph", "content": [{ "type": "text", "text": "before " }] },
                        { "type": "customWidget", "content": [] }
                    ]
                })])
            ]
        })]);
        let tables = collect_tables(&document, 128);
        let entry = &tables.entries[0];
        let data_cell = &entry.rows[1].cells[0];
        assert_eq!(data_cell.text, None, "text is withheld, not partial");
        let codes: Vec<&str> = entry.diagnostics.iter().map(|d| d.code).collect();
        assert!(codes.contains(&"UNKNOWN_NODE_TYPE"), "codes: {codes:?}");
    }

    #[test]
    fn marks_and_wikilinks_and_hardbreaks_project_into_cell_text() {
        let document = doc(vec![json!({
            "type": "table",
            "content": [
                row(vec![header("h1", "A")]),
                row(vec![json!({
                    "type": "tableCell",
                    "content": [
                        { "type": "paragraph", "content": [
                            { "type": "text", "text": "see " },
                            { "type": "wikiLink", "attrs": { "path": "p", "text": "план" } },
                            { "type": "hardBreak" },
                            { "type": "text", "marks": [{"type": "strong"}], "text": "bold" }
                        ] }
                    ]
                })])
            ]
        })]);
        let tables = collect_tables(&document, 128);
        let cell = &tables.entries[0].rows[1].cells[0];
        assert_eq!(cell.text.as_deref(), Some("see план\nbold"));
    }

    #[test]
    fn withheld_heading_context_names_itself_instead_of_silence() {
        let document = doc(vec![
            json!({
                "type": "heading",
                "attrs": {"level": 2},
                "content": [
                    { "type": "text", "text": "Broken " },
                    { "type": "customWidget", "content": [] }
                ]
            }),
            plain_table(),
        ]);
        let tables = collect_tables(&document, 128);
        let entry = &tables.entries[0];
        assert_eq!(entry.heading, None, "text withheld, not partial");
        let first = entry.diagnostics.first().expect("heading warning first");
        assert_eq!(first.code, "UNKNOWN_NODE_TYPE");
        assert!(
            first.message.contains("heading text is withheld"),
            "message: {}",
            first.message
        );
        assert!(
            first.message.contains("--after-heading"),
            "message: {}",
            first.message
        );
        // a plain table without any heading carries no such warning
        let bare = collect_tables(&doc(vec![plain_table()]), 128);
        assert!(bare.entries[0].heading.is_none());
        assert!(
            bare.entries[0]
                .diagnostics
                .iter()
                .all(|d| d.code != "UNKNOWN_NODE_TYPE"),
            "no heading context — no heading warning"
        );
    }

    #[test]
    fn granular_markdown_pieces_compose_to_markdown_body() {
        let document = doc(vec![json!({
            "type": "table",
            "content": [
                row(vec![header("h1", "A|B"), header("h2", "C")]),
                row(vec![cell("c1", "x|y\nz"), cell("c2", "ok")]),
                row(vec![cell("c3", "short")]),
            ]
        })]);
        let tables = collect_tables(&document, 128);
        let entry = &tables.entries[0];
        let indices = [1usize, 2];
        let composed = format!(
            "{}{}",
            markdown_header(entry),
            indices
                .iter()
                .map(|&i| markdown_row(entry, i))
                .collect::<String>()
        );
        assert_eq!(composed, markdown_body(entry, &indices));
        // the granular row line escapes and pads on its own
        assert_eq!(markdown_row(entry, 1), "| x\\|y<br>z | ok |\n");
    }

    #[test]
    fn markdown_body_escapes_pipes_and_newlines_and_pads() {
        let document = doc(vec![json!({
            "type": "table",
            "content": [
                row(vec![header("h1", "A|B"), header("h2", "C")]),
                row(vec![cell("c1", "x|y\nz"), cell("c2", "ok")]),
                row(vec![cell("c3", "short")]),
            ]
        })]);
        let tables = collect_tables(&document, 128);
        let body = markdown_body(&tables.entries[0], &[1, 2]);
        assert_eq!(
            body,
            "| A\\|B | C |\n| --- | --- |\n| x\\|y<br>z | ok |\n| short |  |\n"
        );
    }

    #[test]
    fn note_title_counts_as_heading_context() {
        let document = doc(vec![
            json!({"type": "noteTitle", "content": [{"type": "text", "text": "issues"}]}),
            plain_table(),
        ]);
        let tables = collect_tables(&document, 128);
        assert_eq!(tables.entries[0].heading.as_deref(), Some("issues"));
    }
}
