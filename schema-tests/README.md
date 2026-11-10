# schema-tests/

Differential corpus (§4.8): proves that the Rust validator agrees with the
real prosemirror-model on the artifact contract.

- `generator/` — TypeScript harness. Builds a real `prosemirror-model`
  Schema from the schema artifact (the plugin's runtime schema snapshot —
  the single source of truth), generates valid documents and targeted
  mutations, and records the verdict of the REAL library
  (`Node.fromJSON` + `Node.check`) for every case. Deterministic:
  fixed seed, no wall clock; re-running with the same artifact must
  produce a byte-identical corpus. Requires node >= 23.6 (type stripping).
- `fixtures/corpus.jsonl` — the committed corpus: one case per line,
  `{id, category, doc, expected}` where `expected` is the verdict
  prosemirror-model computed at generation time.

Gate: `crates/insum-cli/tests/corpus_gate.rs` replays the corpus through
`insum-core` and fails on any unexplained divergence. Scope rules the
generator enforces (documented in `generator.ts`): noteDoc roots lead with
`noteTitle` (the artifact's content rule covers the title family only) and
`attrs` are plain objects (PM ignores other shapes; the structural
contract rejects them — both are outside the §15.4 coverage list).

Run locally: `cd schema-tests/generator && npm install && npm run generate`,
then `cargo test --test corpus_gate`.
