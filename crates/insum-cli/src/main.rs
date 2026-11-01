//! `insum` — CLI for reading and writing Inscriptum knowledge bases.
//!
//! Global conventions: spec §5. Every command outputs exactly one JSON
//! document on stdout (terminated by a newline); on failure stdout stays
//! empty and a single JSON error object goes to stderr with the exit code
//! from the §11 registry. The stage A read API commands live in `commands/`.

mod commands;
mod cursor;
mod support;

use std::path::PathBuf;

use clap::{CommandFactory, Parser};
use insum_core::UmError;

use crate::support::Output;

/// Versions reported by `insum --version` (spec §4.1).
const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");
const API_VERSION: u32 = 1;
/// Supported UM Document Container Format versions (spec §4.2).
const UM_VERSIONS: [u32; 1] = [1];

#[derive(Parser, Debug)]
#[command(
    name = "insum",
    about = "Read and write Inscriptum knowledge bases (.um / .note files)",
    disable_version_flag = true
)]
struct Cli {
    /// Print version information as JSON (spec §4.1) and exit.
    #[arg(long)]
    version: bool,

    /// Format JSON output with a two-space indent (spec §5.1).
    #[arg(long, global = true)]
    pretty: bool,

    /// Explicit schema artifact for commands that support it (spec §5.1).
    #[arg(long, global = true, value_name = "PATH")]
    schema: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// List files and notes (spec §6.1).
    Ls {
        /// Include hidden files (§5.2).
        #[arg(long)]
        hidden: bool,
        /// Directory or file; defaults to `.`.
        path: Option<String>,
    },
    /// Compact block structure with JSON Pointers (spec §6.2).
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
    /// Full subtree of one block (spec §6.3).
    Block {
        #[command(subcommand)]
        action: BlockAction,
    },
    /// Full-text search over inline content (spec §6.4).
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
        /// Include hidden files (§5.2).
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
    /// Read whole notes: JSON envelope or annotated projection (spec §6.5).
    Read {
        /// A .um container or a .note file.
        file: PathBuf,
        /// Note id for .um.
        #[arg(long)]
        note: Option<String>,
        /// `json` (default) or `annotated`.
        #[arg(long, default_value = "json")]
        format: String,
        /// Output budget in bytes.
        #[arg(long)]
        max_bytes: Option<usize>,
        /// Resume an annotated projection from a cursor.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Read-only structural validation (spec §6.6).
    Validate {
        /// Turn warnings into errors.
        #[arg(long)]
        strict: bool,
        /// Include hidden files (§5.2).
        #[arg(long)]
        hidden: bool,
        /// Files or directories to validate.
        paths: Vec<PathBuf>,
    },
}

#[derive(clap::Subcommand, Debug)]
enum BlockAction {
    /// Get the full subtree of one block (spec §6.3).
    Get {
        /// A .um container or a .note file.
        file: PathBuf,
        /// Note id — required for .um, forbidden for .note.
        #[arg(long)]
        note: Option<String>,
        /// JSON Pointer of the block (stage A addressing, spec §4.3).
        #[arg(long)]
        pointer: Option<String>,
        /// Block ID — available after the migration stage (§2.3).
        #[arg(long)]
        id: Option<String>,
        /// Return the original node JSON with `attrs.id` kept.
        #[arg(long)]
        raw: bool,
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

    // --schema is accepted globally but only validate knows it (§5.1); say
    // so before running anything
    if !matches!(cli.command, Some(Command::Validate { .. })) && cli.schema.is_some() {
        let err = UmError::usage("--schema is supported by `insum validate` only in this release");
        fail(&err, cli.pretty);
    }

    let limits = insum_core::Limits::from_env();
    let result: Result<Output, UmError> = match &cli.command {
        None => {
            let _ = Cli::command().print_help();
            std::process::exit(0);
        }
        Some(Command::Ls { path, hidden }) => {
            commands::ls::run(path.clone(), *hidden, &limits).map(Output::Json)
        }
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
            max_bytes,
            cursor,
        }) => commands::read::run(
            file,
            note.as_deref(),
            format,
            *max_bytes,
            cursor.as_deref(),
            &limits,
        ),
        Some(Command::Validate {
            strict,
            hidden,
            paths,
        }) => {
            // --schema is accepted globally but only validate knows it (§5.1)
            commands::validate::run(paths, *strict, cli.schema.as_deref(), *hidden, &limits)
                .map(Output::Json)
        }
    };

    match result {
        Ok(Output::Json(value)) => print_json(&value, cli.pretty),
        Ok(Output::Text(text)) => print!("{text}"),
        Err(err) => fail(&err, cli.pretty),
    }
}

/// The `--version` payload: `cli_version`, supported `api_versions`, UM
/// versions (spec §4.1). `api_version` is included per §5.3.
fn version_json() -> serde_json::Value {
    serde_json::json!({
        "api_version": API_VERSION,
        "cli_version": CLI_VERSION,
        "api_versions": [API_VERSION],
        "um_versions": UM_VERSIONS,
    })
}

/// Print exactly one JSON document plus a newline to stdout (spec §5.3).
fn print_json(value: &serde_json::Value, pretty: bool) {
    if pretty {
        println!("{value:#}");
    } else {
        println!("{value}");
    }
}

/// Print the error envelope to stderr, stdout stays empty, exit per §11.
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
/// JSON USAGE errors on stderr (exit 2, spec §5.6/§11).
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
            // Honor --pretty even on a failed parse (spec §5.3: it only
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
