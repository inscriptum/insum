//! `insum` — CLI for reading and writing Inscriptum knowledge bases.
//!
//! Global conventions: Every command outputs exactly one JSON
//! document on stdout (terminated by a newline); on failure stdout stays
//! empty and a single JSON error object goes to stderr with the exit code
//! from the registry. The stage A read API commands live in `commands/`.

mod commands;
mod cursor;
mod support;

use std::path::PathBuf;

use clap::{CommandFactory, Parser};
use insum_core::UmError;

use crate::support::Output;

/// Versions reported by `insum --version`.
const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");
const API_VERSION: u32 = 1;
/// Supported UM Document Container Format versions.
const UM_VERSIONS: [u32; 1] = [1];

#[derive(Parser, Debug)]
#[command(
    name = "insum",
    about = "Read and write Inscriptum knowledge bases (.um / .note files)",
    disable_version_flag = true
)]
struct Cli {
    /// Print version information as JSON and exit.
    #[arg(long)]
    version: bool,

    /// Format JSON output with a two-space indent.
    #[arg(long, global = true)]
    pretty: bool,

    /// Explicit schema artifact for commands that support it.
    #[arg(long, global = true, value_name = "PATH")]
    schema: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// List files and notes.
    Ls {
        /// Include hidden files.
        #[arg(long)]
        hidden: bool,
        /// Add a per-note property summary (name/value pairs). File
        /// listing: on the note descriptors; directory listing: a
        /// note_details array for notebooks, on the entry for `.note`
        /// files. Reads and decompresses every note payload — much
        /// slower than the default listing, which never opens payloads.
        #[arg(long)]
        with_properties: bool,
        /// Add `note_rev` (payload + manifest descriptor per the
        /// revision rule). File listing: on the note descriptors;
        /// directory listing: a note_details array for notebooks, on the
        /// entry for `.note` files. Also reads every payload.
        #[arg(long)]
        with_rev: bool,
        /// Directory or file; defaults to `.`.
        path: Option<String>,
    },
    /// Read-only properties of one note page (`noteProperty` rows).
    Props {
        /// A .um container or a .note file.
        file: PathBuf,
        /// Note by id (ULID) or exact title; default: the title note of a
        /// .um container (the first note in manifest order) or the single
        /// document of a .note file.
        #[arg(long)]
        note: Option<String>,
        /// Return only the property with this exact name; a name held by
        /// one row returns all its value entries (a list-typed row yields
        /// several), a miss is PROPERTY_NOT_FOUND, a name held by several
        /// rows is an ambiguity error.
        #[arg(long)]
        get: Option<String>,
        /// Output format; only `json` is supported.
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Compact block structure with JSON Pointers.
    Tree {
        /// A .um container or a .note file.
        file: PathBuf,
        /// Restrict to notes by id (repeatable, .um only).
        #[arg(long)]
        note: Vec<String>,
        /// Show only the first `n` levels.
        #[arg(long)]
        depth: Option<usize>,
        /// Max characters of `text` per block (default 120).
        #[arg(long, default_value_t = 120)]
        text_limit: usize,
        /// Output budget in bytes (default UM_MAX_OUTPUT_BYTES).
        #[arg(long)]
        max_bytes: Option<usize>,
        /// Resume from a next_cursor token of the previous response.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Full subtree of one block.
    Block {
        #[command(subcommand)]
        action: BlockAction,
    },
    /// Structured read of one table.
    Table {
        #[command(subcommand)]
        action: TableAction,
    },
    /// Full-text search over inline content.
    Search {
        /// Literal search instead of a Rust `regex` pattern.
        #[arg(short = 'F')]
        fixed_strings: bool,
        /// Case-insensitive matching.
        #[arg(short = 'i')]
        ignore_case: bool,
        /// Maximum number of matches (default 50, cap UM_MAX_SEARCH_MATCHES).
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Include hidden files.
        #[arg(long)]
        hidden: bool,
        /// Restrict to one note id.
        #[arg(long)]
        note: Option<String>,
        /// Neighboring structural blocks per match, 0..5.
        #[arg(long, default_value_t = 0)]
        context: usize,
        /// Output budget in bytes.
        #[arg(long)]
        max_bytes: Option<usize>,
        /// Query: regex or literal (with -F).
        query: String,
        /// Files or directories to search; defaults to `.`.
        paths: Vec<PathBuf>,
    },
    /// Read whole notes: JSON envelope or markdown projection (annotated / compact).
    Read {
        /// A .um container or a .note file.
        file: PathBuf,
        /// Note id for .um.
        #[arg(long)]
        note: Option<String>,
        /// `json` (default), `annotated` (projection with anchors) or `compact` (anchor-free projection; document text cannot forge the `[um:` markers — it is neutralized to `[ um:`).
        #[arg(long, default_value = "json")]
        format: String,
        /// Add the annotated anchor comments to the compact projection.
        #[arg(long)]
        with_anchors: bool,
        /// Output budget in bytes.
        #[arg(long)]
        max_bytes: Option<usize>,
        /// Resume an annotated or compact projection from a cursor.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Read-only structural validation.
    Validate {
        /// Turn warnings into errors.
        #[arg(long)]
        strict: bool,
        /// Include hidden files.
        #[arg(long)]
        hidden: bool,
        /// Files or directories to validate.
        paths: Vec<PathBuf>,
    },
    /// Show the applicable schema artifact, its source and fingerprint.
    Schema {
        /// Describe one node type from the artifact.
        #[arg(long, value_name = "NODE")]
        r#type: Option<String>,
    },
    /// Assign guaranteed block IDs and stamp the `features.block_ids`
    /// marker (stage C; the only writer of the marker).
    MigrateIds {
        /// A .um container or a .note file.
        file: PathBuf,
        /// Migrate only these notes (ids, repeatable); default: every note
        /// that needs migration.
        #[arg(long)]
        note: Vec<String>,
        /// Plan without writing; preview IDs may differ from commit IDs.
        #[arg(long)]
        dry_run: bool,
    },
    /// Addressed write of blocks by ID with a revision CAS (stage D; §8).
    ///
    /// Operations: replace, delete, insert_before, insert_after,
    /// append_child, set_attrs, replace_content — plus the state
    /// operations: set_property (upsert a noteProperty row of the title
    /// page by name, text-only value), set_table_cell (write one cell by
    /// table ID + row/column address) and upsert_table_row (insert or
    /// update a row by key columns; `match` is optional — without it the
    /// operation always inserts, `position` is "top" or "bottom", default
    /// "bottom"). IDs are managed by the CLI: new addressable blocks
    /// receive IDs at commit (`created_ids`), existing blocks the
    /// operations rewrite in place keep theirs and are reported in
    /// `changed_ids`.
    Apply {
        /// A .um container or a .note file.
        file: PathBuf,
        /// Operations package: JSON file path, or `-` to read stdin.
        #[arg(long, value_name = "FILE")]
        ops: String,
        /// Validate the package against the current state without writing.
        #[arg(long)]
        dry_run: bool,
        /// Require the current note_rev to start with this prefix (at least
        /// 16 hex characters after `sha256:`); must agree with the package
        /// `base_note_rev` when both are given.
        #[arg(long, value_name = "PREFIX")]
        if_match: Option<String>,
    },
    /// Readable change history of a file from git (read-only).
    History {
        /// A .um container or a .note file.
        file: PathBuf,
        /// Only changes of this note (id or exact title).
        #[arg(long)]
        note: Option<String>,
        /// Last `n` revisions (default 20, cap 200).
        #[arg(long)]
        limit: Option<usize>,
        /// `json` (default) or `annotated`.
        #[arg(long, default_value = "json")]
        format: String,
        /// Output budget in bytes.
        #[arg(long)]
        max_bytes: Option<usize>,
    },
}

#[derive(clap::Subcommand, Debug)]
enum BlockAction {
    /// Get the full subtree of one block.
    Get {
        /// A .um container or a .note file.
        file: PathBuf,
        /// Note id — required for .um, forbidden for .note.
        #[arg(long)]
        note: Option<String>,
        /// JSON Pointer of the block (stage A addressing).
        #[arg(long)]
        pointer: Option<String>,
        /// Block ID — resolves when the ID is present in the note; without
        /// the features.block_ids marker the answer carries a warning
        #[arg(long)]
        id: Option<String>,
        /// Return the original node JSON with `attrs.id` kept.
        #[arg(long)]
        raw: bool,
    },
}

#[derive(clap::Subcommand, Debug)]
enum TableAction {
    /// Structured read of one table of a note (read-only; the response
    /// contract is this help text).
    ///
    /// Address the table with exactly one of: --index N (0-based, document
    /// order), --id <block-id>, --after-heading <text> (the exact text of
    /// the nearest preceding heading; several tables under same-text
    /// headings are an error).
    ///
    /// JSON response fields (fixed set; emitted in lexicographic order):
    /// `api_version`, `path`, `note_id`, `note_rev`, `table` with `pointer`,
    /// `id`, `index`, `table_count`, `heading`, `columns` (each `name`,
    /// `id`, `pointer`; the header row is the table's first row), `rows`
    /// (each `id`, `pointer`, `cells` with `id`, `pointer`, `text`, and
    /// `values` — column name to cell text, `null` when names are
    /// duplicated or withheld), `rows_source`, `rows_matched`, plus
    /// `warnings`, `complete`, `truncated`.
    ///
    /// `--where COL=VALUE` keeps only the rows whose cell in column COL
    /// equals VALUE byte-for-byte: a miss is an empty row list, an unknown
    /// column is an error, any duplicated column name refuses the filter.
    /// `--format markdown` prints a GFM table (first row is the header,
    /// pipes escaped as `\|`, newlines become `<br>`, no anchors);
    /// diagnostics ride as `<!-- um:warning code=... -->` comments after
    /// the table. The output budget (`--max-bytes`, default
    /// UM_MAX_OUTPUT_BYTES) truncates whole rows with `truncated: true`,
    /// `complete: false` and an OUTPUT_BUDGET warning — never silently.
    Get {
        /// A .um container or a .note file.
        file: PathBuf,
        /// Note id — required for .um, forbidden for .note.
        #[arg(long)]
        note: Option<String>,
        /// Document-order index of the table, 0-based.
        #[arg(long, value_name = "N")]
        index: Option<usize>,
        /// Block ID of the table node — resolves when the ID is present in
        /// the note; without the features.block_ids marker the answer
        /// carries a warning.
        #[arg(long, value_name = "ID")]
        id: Option<String>,
        /// Exact text of the nearest heading preceding the table.
        #[arg(long, value_name = "TEXT")]
        after_heading: Option<String>,
        /// Keep only rows whose cell in COL equals VALUE exactly.
        #[arg(long, value_name = "COL=VALUE")]
        r#where: Option<String>,
        /// `json` (default) or `markdown` (GFM projection).
        #[arg(long, default_value = "json")]
        format: String,
        /// Output budget in bytes.
        #[arg(long)]
        max_bytes: Option<usize>,
    },
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => exit_with_clap_error(err),
    };

    if cli.version {
        print_json(&version_json(), cli.pretty);
        std::process::exit(0);
    }

    // --schema is accepted globally but only schema-aware commands know it;
    // say so before running anything
    if !matches!(
        cli.command,
        Some(Command::Validate { .. })
            | Some(Command::Schema { .. })
            | Some(Command::MigrateIds { .. })
            | Some(Command::Apply { .. })
    ) && cli.schema.is_some()
    {
        let err = UmError::usage(
            "--schema is supported by `insum schema`, `insum validate`, `insum migrate-ids` and `insum apply` only",
        );
        fail(&err, cli.pretty);
    }

    let limits = insum_core::Limits::from_env();
    let result: Result<Output, UmError> = match &cli.command {
        None => {
            let _ = Cli::command().print_help();
            std::process::exit(0);
        }
        Some(Command::Ls {
            path,
            hidden,
            with_properties,
            with_rev,
        }) => commands::ls::run(path.clone(), *hidden, *with_properties, *with_rev, &limits)
            .map(Output::Json),
        Some(Command::Props {
            file,
            note,
            get,
            format,
        }) => commands::props::run(file, note.as_deref(), get.as_deref(), format, &limits)
            .map(Output::Json),
        Some(Command::Tree {
            file,
            note,
            depth,
            text_limit,
            max_bytes,
            cursor,
        }) => commands::tree::run(
            file,
            note,
            *depth,
            *text_limit,
            *max_bytes,
            cursor.as_deref(),
            &limits,
        )
        .map(Output::Json),
        Some(Command::Block {
            action:
                BlockAction::Get {
                    file,
                    note,
                    pointer,
                    id,
                    raw,
                },
        }) => commands::block::run(
            file,
            note.as_deref(),
            pointer.as_deref(),
            id.as_deref(),
            *raw,
            &limits,
        )
        .map(Output::Json),
        Some(Command::Table {
            action:
                TableAction::Get {
                    file,
                    note,
                    index,
                    id,
                    after_heading,
                    r#where,
                    format,
                    max_bytes,
                },
        }) => commands::table::run(
            &commands::table::TableGet {
                path: file,
                note: note.as_deref(),
                index: *index,
                id: id.as_deref(),
                after_heading: after_heading.as_deref(),
                where_filter: r#where.as_deref(),
                format,
                max_bytes: *max_bytes,
            },
            &limits,
        ),
        Some(Command::Search {
            fixed_strings,
            ignore_case,
            limit,
            hidden,
            note,
            context,
            max_bytes,
            query,
            paths,
        }) => commands::search::run(
            query,
            &commands::search::SearchOptions {
                fixed_strings: *fixed_strings,
                ignore_case: *ignore_case,
                limit: *limit,
                hidden: *hidden,
                note: note.as_deref(),
                context: *context,
                max_bytes: *max_bytes,
                paths,
            },
            &limits,
        )
        .map(Output::Json),
        Some(Command::Read {
            file,
            note,
            format,
            with_anchors,
            max_bytes,
            cursor,
        }) => commands::read::run(
            file,
            note.as_deref(),
            format,
            *with_anchors,
            *max_bytes,
            cursor.as_deref(),
            &limits,
        ),
        Some(Command::Validate {
            strict,
            hidden,
            paths,
        }) => {
            // --schema is accepted globally but only schema-aware commands know it
            commands::validate::run(paths, *strict, cli.schema.as_deref(), *hidden, &limits)
                .map(Output::Json)
        }
        Some(Command::Schema { r#type }) => {
            commands::schema::run(cli.schema.as_deref(), r#type.as_deref(), &limits)
                .map(Output::Json)
        }
        Some(Command::MigrateIds {
            file,
            note,
            dry_run,
        }) => {
            let filters = if note.is_empty() {
                None
            } else {
                Some(note.clone())
            };
            match file.extension().and_then(|e| e.to_str()) {
                Some(ext) if ext.eq_ignore_ascii_case("um") => commands::migrate_ids::run_um(
                    file,
                    filters.as_deref(),
                    cli.schema.as_deref(),
                    *dry_run,
                    &limits,
                )
                .map(Output::Json),
                Some(ext) if ext.eq_ignore_ascii_case("note") => {
                    if filters.is_some() {
                        Err(UmError::usage(
                            "--note is not allowed for a .note file (it holds a single document)",
                        ))
                    } else {
                        commands::migrate_ids::run_note(
                            file,
                            cli.schema.as_deref(),
                            *dry_run,
                            &limits,
                        )
                        .map(Output::Json)
                    }
                }
                _ => Err(UmError::new(
                    insum_core::ErrorCode::PathNotFound,
                    format!("no .um or .note document at {}", file.display()),
                )),
            }
        }
        Some(Command::Apply {
            file,
            ops,
            dry_run,
            if_match,
        }) => commands::apply::read_ops(ops, &limits).and_then(|package| {
            match file.extension().and_then(|e| e.to_str()) {
                Some(ext) if ext.eq_ignore_ascii_case("um") => commands::apply::run_um(
                    file,
                    &package,
                    if_match.as_deref(),
                    cli.schema.as_deref(),
                    *dry_run,
                    &limits,
                )
                .map(Output::Json),
                Some(ext) if ext.eq_ignore_ascii_case("note") => commands::apply::run_note(
                    file,
                    &package,
                    if_match.as_deref(),
                    cli.schema.as_deref(),
                    *dry_run,
                    &limits,
                )
                .map(Output::Json),
                _ => Err(UmError::new(
                    insum_core::ErrorCode::PathNotFound,
                    format!("no .um or .note document at {}", file.display()),
                )),
            }
        }),
        Some(Command::History {
            file,
            note,
            limit,
            format,
            max_bytes,
        }) => commands::history::run(file, note.as_deref(), *limit, format, *max_bytes, &limits),
    };

    match result {
        Ok(Output::Json(value)) => print_json(&value, cli.pretty),
        Ok(Output::Text(text)) => print!("{text}"),
        Err(err) => fail(&err, cli.pretty),
    }
}

/// The `--version` payload: `cli_version`, supported `api_versions`, UM
/// versions. `api_version` is included per
fn version_json() -> serde_json::Value {
    serde_json::json!({
        "api_version": API_VERSION,
        "cli_version": CLI_VERSION,
        "api_versions": [API_VERSION],
        "um_versions": UM_VERSIONS,
    })
}

/// Print exactly one JSON document plus a newline to stdout.
fn print_json(value: &serde_json::Value, pretty: bool) {
    if pretty {
        println!("{value:#}");
    } else {
        println!("{value}");
    }
}

/// Print the error envelope to stderr, stdout stays empty, exit per
fn fail(err: &UmError, pretty: bool) -> ! {
    let envelope = err.envelope();
    let rendered = if pretty {
        serde_json::to_string_pretty(&envelope).expect("error envelope serializes")
    } else {
        serde_json::to_string(&envelope).expect("error envelope serializes")
    };
    eprintln!("{rendered}");
    std::process::exit(err.code.exit_code());
}

/// clap-level failures: help goes to stdout (exit 0), argument errors become
/// JSON USAGE errors on stderr (exit 2,/).
fn exit_with_clap_error(err: clap::Error) -> ! {
    use clap::error::ErrorKind::*;

    match err.kind() {
        DisplayHelp | DisplayHelpOnMissingArgumentOrSubcommand => {
            let _ = err.print();
            std::process::exit(0);
        }
        DisplayVersion => {
            let _ = err.print();
            std::process::exit(0);
        }
        _ => {
            // Honor --pretty even on a failed parse (it only
            // changes formatting).
            let pretty = std::env::args_os().any(|arg| arg == "--pretty");
            let usage = UmError::usage(err.to_string().trim_end());
            fail(&usage, pretty);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_json_has_contract_fields() {
        let v = version_json();
        assert_eq!(v["api_version"], 1);
        assert_eq!(v["cli_version"], CLI_VERSION);
        assert_eq!(v["api_versions"], serde_json::json!([1]));
        assert_eq!(v["um_versions"], serde_json::json!([1]));
    }
}
