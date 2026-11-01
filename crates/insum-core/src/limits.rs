//! Safety limits — spec §5.5. Configured via environment variables, checked
//! against actual data (spec §12), not just headers.

use std::env;

/// One limit: default value, absolute safety ceiling, and the env var name.
/// The ceiling is the "absolute safety cap" of §5.5: environment values above
/// it fall back to the default, and per-command options are clamped to it.
#[derive(Debug, Clone, Copy)]
pub struct LimitSpec {
    pub env: &'static str,
    pub default: usize,
    pub cap: usize,
}

const GIB: usize = 1024 * 1024 * 1024;

const LIMITS: [LimitSpec; 9] = [
    LimitSpec {
        env: "UM_MAX_ENTRY_BYTES",
        default: 33_554_432,
        cap: GIB,
    },
    LimitSpec {
        env: "UM_MAX_ARCHIVE_ENTRIES",
        default: 10_000,
        cap: 1_000_000,
    },
    LimitSpec {
        env: "UM_MAX_TOTAL_BYTES",
        default: 268_435_456,
        cap: 8 * GIB,
    },
    LimitSpec {
        env: "UM_MAX_DEPTH",
        default: 128,
        cap: 1024,
    },
    LimitSpec {
        env: "UM_MAX_OUTPUT_BYTES",
        default: 65_536,
        cap: GIB,
    },
    LimitSpec {
        env: "UM_MAX_OPS_BYTES",
        default: 4_194_304,
        cap: GIB,
    },
    LimitSpec {
        env: "UM_MAX_SEARCH_MATCHES",
        default: 1_000,
        cap: 1_000,
    },
    LimitSpec {
        env: "UM_MAX_APPLY_OPS",
        default: 1_000,
        cap: 100_000,
    },
    LimitSpec {
        env: "UM_MAX_COMMIT_RETRIES",
        default: 3,
        cap: 100,
    },
];

/// The resolved limit values for one process run.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_entry_bytes: usize,
    pub max_archive_entries: usize,
    pub max_total_bytes: usize,
    pub max_depth: usize,
    pub max_output_bytes: usize,
    pub max_ops_bytes: usize,
    pub max_search_matches: usize,
    pub max_apply_ops: usize,
    pub max_commit_retries: usize,
}

impl Limits {
    /// Read limits from the environment. Values that are absent, unparsable
    /// or above the absolute safety cap fall back to the default; a stricter
    /// configured value is honored (spec §5.5).
    pub fn from_env() -> Self {
        let mut limits = Self::defaults();
        for (slot, spec) in [
            (&mut limits.max_entry_bytes, &LIMITS[0]),
            (&mut limits.max_archive_entries, &LIMITS[1]),
            (&mut limits.max_total_bytes, &LIMITS[2]),
            (&mut limits.max_depth, &LIMITS[3]),
            (&mut limits.max_output_bytes, &LIMITS[4]),
            (&mut limits.max_ops_bytes, &LIMITS[5]),
            (&mut limits.max_search_matches, &LIMITS[6]),
            (&mut limits.max_apply_ops, &LIMITS[7]),
            (&mut limits.max_commit_retries, &LIMITS[8]),
        ] {
            if let Some(value) = read_limit(spec) {
                *slot = value;
            }
        }
        limits
    }

    pub fn defaults() -> Self {
        Self {
            max_entry_bytes: 33_554_432,
            max_archive_entries: 10_000,
            max_total_bytes: 268_435_456,
            max_depth: 128,
            max_output_bytes: 65_536,
            max_ops_bytes: 4_194_304,
            max_search_matches: 1_000,
            max_apply_ops: 1_000,
            max_commit_retries: 3,
        }
    }

    /// The output budget for one response: `--max-bytes` override or the
    /// default, clamped to the absolute ceiling (spec §5.5: options refine
    /// the limit but never break the safety cap).
    pub fn output_bytes(&self, requested: Option<usize>) -> usize {
        match requested {
            Some(value) => value.min(GIB),
            None => self.max_output_bytes,
        }
    }
}

fn read_limit(spec: &LimitSpec) -> Option<usize> {
    let raw = env::var(spec.env).ok()?;
    match raw.trim().parse::<usize>() {
        // A value above the absolute cap silently falls back to the default:
        // safety caps are not reachable through the environment.
        Ok(value) if value <= spec.cap => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec_55() {
        let limits = Limits::defaults();
        assert_eq!(limits.max_entry_bytes, 32 * 1024 * 1024);
        assert_eq!(limits.max_archive_entries, 10_000);
        assert_eq!(limits.max_total_bytes, 256 * 1024 * 1024);
        assert_eq!(limits.max_depth, 128);
        assert_eq!(limits.max_output_bytes, 64 * 1024);
        assert_eq!(limits.max_ops_bytes, 4 * 1024 * 1024);
        assert_eq!(limits.max_search_matches, 1_000);
        assert_eq!(limits.max_apply_ops, 1_000);
        assert_eq!(limits.max_commit_retries, 3);
    }

    #[test]
    fn output_budget_never_breaks_the_cap() {
        let limits = Limits::defaults();
        assert_eq!(limits.output_bytes(Some(1024)), 1024);
        assert_eq!(limits.output_bytes(Some(10 * GIB)), GIB);
        assert_eq!(limits.output_bytes(None), 65_536);
    }
}
