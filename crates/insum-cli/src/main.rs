//! `insum` — CLI for reading and writing Inscriptum knowledge bases.
//!
//! Global conventions: spec §5. Every command outputs exactly one JSON
//! document on stdout (terminated by a newline); on failure stdout stays
//! empty and a single JSON error object goes to stderr with the exit code
//! from the §11 registry.

use clap::{CommandFactory, Parser};
use insum_core::UmError;

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

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Commands of the read API (spec §6) are added by the read-api task.
    #[command(hide = true)]
    Placeholder,
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

    match cli.command {
        // No commands are implemented yet (read-api task adds them, spec §6).
        // `insum` without a command prints help; anything unrecognized is a
        // USAGE error handled in exit_with_clap_error.
        None => {
            let _ = Cli::command().print_help();
            std::process::exit(0);
        }
        Some(_) => {
            let err = UmError::usage("this command is not implemented yet");
            fail(&err, cli.pretty);
        }
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
        println!("{:#}", value);
    } else {
        println!("{}", value);
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
            let pretty = std::env::args_os().any(|a| a == "--pretty");
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
