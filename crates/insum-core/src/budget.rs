//! The shared output-budget discipline (§5.5, §6.5): `UM_MAX_OUTPUT_BYTES`
//! bounds the serialized response, and an incomplete response says so
//! explicitly — silent truncation is forbidden. Every command that shapes a
//! response charges its envelope skeleton first (the largest shape it can
//! emit, truncation markers and OUTPUT_BUDGET texts included) and then fits
//! its list entries into what remains. The commands keep their skeletons and
//! their warning texts — their response shapes differ — while this module
//! owns the one measure/fit/totals rule they share (CON-7: a single
//! discipline, not per-command implementations that can drift).

use serde_json::Value;

/// Defensive size for an entry whose serialization fails. An in-memory
/// `Value` with string map keys cannot fail; the fallback only keeps the
/// budget honest if one ever does.
const ENTRY_SIZE_FALLBACK: usize = 256;

/// The byte ledger of one response: the budget, and the bytes charged so
/// far by the envelope overhead and the entries kept so far.
pub(crate) struct OutputBudget {
    budget: usize,
    used: usize,
}

impl OutputBudget {
    /// Start a ledger. `charged` is the response's envelope overhead plus
    /// any per-entry post-write reserve the command must account for before
    /// its entries are fitted (migrate's D-1 commit reserve; a response
    /// without post-write growth charges nothing beyond the skeleton).
    pub(crate) fn new(budget: usize, charged: usize) -> Self {
        Self {
            budget,
            used: charged,
        }
    }

    /// The serialized size of one list entry, comma included.
    fn entry_size(entry: &Value) -> usize {
        serde_json::to_string(entry).map_or(ENTRY_SIZE_FALLBACK, |s| s.len() + 1)
    }

    /// Keep the whole prefix of `entries` that fits the remaining budget and
    /// return `(kept, dropped)` — the honest totals pair for the caller's
    /// truncation markers (§6.5). The FIRST entry is always kept, even above
    /// the budget: the documented oversized-first-entry exception (same rule
    /// as `tree`'s oversized block) — a response must at least name what it
    /// did. Truncation drops from the first entry that does not fit onward;
    /// entries are never reordered or skipped.
    pub(crate) fn fit(&mut self, entries: &[Value]) -> (Vec<Value>, usize) {
        let mut kept = Vec::with_capacity(entries.len());
        for entry in entries {
            let size = Self::entry_size(entry);
            if !kept.is_empty() && self.used + size > self.budget {
                break;
            }
            self.used += size;
            kept.push(entry.clone());
        }
        let dropped = entries.len() - kept.len();
        (kept, dropped)
    }
}

/// Append `warning` to a response's `warnings` array in place. The commands
/// build their OUTPUT_BUDGET texts themselves (mode-aware, migrate's D-2);
/// this is only the array merge they all repeat.
pub(crate) fn push_warning(response: &mut Value, warning: Value) {
    let mut warnings = response
        .get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    warnings.push(warning);
    response["warnings"] = Value::Array(warnings);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The fit rule keeps a prefix in order: entries before the first
    /// non-fitting one are kept, everything from it onward is dropped —
    /// even when a later entry would have fit on its own.
    #[test]
    fn fit_keeps_a_prefix_and_reports_honest_totals() {
        let entries: Vec<Value> = (0..10)
            .map(|i| json!({"i": i, "pad": "x".repeat(30)}))
            .collect();
        let mut budget = OutputBudget::new(200, 0);
        let (kept, dropped) = budget.fit(&entries);
        assert_eq!(kept.len() + dropped, entries.len(), "totals stay honest");
        assert!(
            dropped > 0 && !kept.is_empty(),
            "the oversized list is actually cut, the prefix survives"
        );
        for (index, entry) in kept.iter().enumerate() {
            assert_eq!(entry["i"], index, "the kept prefix is in order");
        }
    }

    /// The documented oversized-first-entry exception: the first entry is
    /// emitted whole even when it alone exceeds the budget — a response
    /// must at least name what it did (§6.5, same rule as `tree`).
    #[test]
    fn the_first_entry_is_emitted_whole_even_above_the_budget() {
        let big = json!({"pad": "x".repeat(500)});
        let small = json!({"pad": "y"});
        let mut budget = OutputBudget::new(100, 0);
        let (kept, dropped) = budget.fit(&[big, small]);
        assert_eq!(kept.len(), 1, "the oversized first entry is kept");
        assert_eq!(dropped, 1, "nothing after it fits");
    }
}
