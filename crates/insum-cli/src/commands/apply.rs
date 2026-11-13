//! `insum apply` — stage D command (§8). The engine lives in
//! `insum_core::apply`; this module reads the operations package (file or
//! stdin, size-capped) and shapes the call.

use std::io::Read;
use std::path::Path;

use serde_json::Value;

use insum_core::{ErrorCode, Limits, UmError};

/// Read the operations package: a JSON file path, or `-` for stdin. The
/// size is capped at UM_MAX_OPS_BYTES (+1 to detect overflow), the byte
/// budget the engine re-checks on the parsed value.
pub fn read_ops(source: &str, limits: &Limits) -> Result<Vec<u8>, UmError> {
    let mut bytes = Vec::new();
    if source == "-" {
        let stdin = std::io::stdin().lock();
        stdin
            .take(limits.max_ops_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| {
                UmError::new(
                    ErrorCode::IoError,
                    format!("cannot read the operations package from stdin: {err}"),
                )
            })?;
    } else {
        let file = std::fs::File::open(source).map_err(|err| {
            UmError::new(
                ErrorCode::PathNotFound,
                format!("cannot open the operations package {}: {err}", source),
            )
        })?;
        file.take(limits.max_ops_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| {
                UmError::new(
                    ErrorCode::IoError,
                    format!("cannot read the operations package {source:?}: {err}"),
                )
            })?;
    }
    if bytes.len() > limits.max_ops_bytes {
        return Err(UmError::new(
            ErrorCode::LimitExceeded,
            format!(
                "operations package exceeds UM_MAX_OPS_BYTES ({} bytes)",
                limits.max_ops_bytes
            ),
        ));
    }
    Ok(bytes)
}

pub fn run_um(
    file: &Path,
    package: &[u8],
    if_match: Option<&str>,
    schema: Option<&Path>,
    dry_run: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    insum_core::apply::apply_um(file, package, if_match, schema, dry_run, limits)
}

pub fn run_note(
    file: &Path,
    package: &[u8],
    if_match: Option<&str>,
    schema: Option<&Path>,
    dry_run: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    insum_core::apply::apply_note_file(file, package, if_match, schema, dry_run, limits)
}
