//! `insum migrate-ids` — stage C command (§7.2). The engine lives in
//! `insum_core::migrate`; this module only classifies the file type and
//! shapes the call.

use std::path::Path;

use serde_json::Value;

use insum_core::{Limits, UmError};

pub fn run_um(
    file: &Path,
    filters: Option<&[String]>,
    schema: Option<&Path>,
    dry_run: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    insum_core::migrate::migrate_um(file, filters, schema, dry_run, limits)
}

pub fn run_note(
    file: &Path,
    schema: Option<&Path>,
    dry_run: bool,
    limits: &Limits,
) -> Result<Value, UmError> {
    insum_core::migrate::migrate_note_file(file, schema, dry_run, limits)
}
