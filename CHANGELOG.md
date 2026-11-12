# Changelog

All notable changes to the `insum` CLI are documented in this file.
Format — Keep a Changelog; versioning — SemVer.

## [Unreleased]

### Added

- `insum migrate-ids <file> [--note <id>]... [--dry-run]` (stage C, §7.2):
  the one command allowed to stamp the `features.block_ids` marker (§4.6).
  Plans ID fixes per note (missing/duplicate/invalid per document order —
  the first valid ID is kept), returns the
  `{pointer, old_id, new_id, type}` mapping; dry-run never touches bytes
  or mtime and its preview IDs may differ from the commit. Real commit:
  sidecar lock + dirty-lease refusal (§9), file and per-note revision
  recheck inside the lock, atomic replace; untouched ZIP entries are
  copied raw (byte-identical), unknown manifest fields survive. The
  marker rides in the note descriptor's `features` object
  (`block_ids: 1` + the schema fingerprint it was stamped under); the
  descriptor's `schema` field (the editor profile) is untouched. Legacy
  `.note` converts to the envelope shape of §4.6. A note whose IDs are
  already complete still gets the marker — the marker records the
  verification. `block get --id` works whenever the ID is present in the
  note (§4.6 п.4, owner decision 2026-10-30 — read-by-ID on presence):
  without the `features.block_ids` marker the response carries a
  `BLOCK_IDS_NOT_GUARANTEED` warning (§6.5 — no silent answers), an
  absent ID stays `BLOCK_NOT_FOUND`, and the marker gate remains
  mandatory for the write API (stage D).
- `migrate-ids` responses respect `UM_MAX_OUTPUT_BYTES` (§5.5): note
  plans are appended whole while they fit the budget; an oversized
  report is explicitly incomplete — `complete: false`, `truncated:
  true`, `notes_total`/`notes_returned` and an `OUTPUT_BUDGET` warning
  naming `--note <id>` filters as the way to the remainder (§6.5 —
  silent truncation is forbidden). Every response carries `complete`.
  A cursor pagination would not fit this command: the commit is one
  transaction, so a second page could not reproduce mappings from an
  already-migrated file. The operation itself always covers every
  selected note — only the report can be partial; the first note plan
  above the whole budget is emitted whole (the same documented
  exception `tree` applies to a single oversized block).
- Non-dry-run `migrate-ids` responses distinguish wrote from no-op with
  the top-level `written: true|false` flag.
- `migrate-ids` response revisions are unambiguous: `notes[].note_rev`
  is the PRE-write revision — the base a stage-D write must build its
  CAS on (§5.4); the additive `notes[].new_note_rev` (computed from the
  final bytes, staged descriptor included) and the top-level `file_rev`
  are post-write. The no-op rerun of a commit reports the committed
  `new_note_rev` values as its pre-write `note_rev`s.
- Search attribution invariant pinned: repeated stress runs over a
  multi-note corpus (heavy text sharing, `--note` filter, `.um`+`.note`
  mix) verify every match resolves `block get` by its own `note_id` and
  `pointer`; manifest-faithful attribution (including the foreign
  descriptor-path data class) is pinned by test — audit outcome for the
  cli-feedback `search-note-attribution-flake` report.
- `READ_TORN` error code (exit 4): an absent, empty, or cut-off `.um`
  container (and an absent or empty `.note`) answers a single speaking
  diagnosis from every reading command (`ls`, `tree`, `block get`, `read`,
  `search`, `validate`) with a machine-readable `cause`
  (`absent`/`empty`/`truncated`) and a retry hint, instead of the
  misleading `NOTE_NOT_FOUND` (or a bare `NOT_A_ZIP`) the non-atomic
  rewrites of um-build produced. A `.note` with torn JSON content still
  answers `NOTE_JSON_INVALID` — a JSON document has no signature to detect
  truncation. `history` is intentionally exempt: it reads git, not the
  live container bytes, and keeps its reviewed absent-path contract.
  `search`/`validate` fold the new code into their existing per-file
  `skipped`/`files[]` entries; error entries in those reports now carry
  `details` too.
- Differential TS×Rust corpus (§4.8): a TypeScript harness builds a real
  prosemirror-model Schema from the artifact and records its verdicts for
  122 documents (valid compositions, targeted mutations, unknown types,
  mark-set rules, text and shape edge cases); the committed corpus is
  replayed by the Rust gate `corpus_gate` with zero divergences, and CI
  regenerates it requiring a byte-identical result.
- `insum validate` at the `schema` level now performs full
  ProseMirror-parity checks: content expressions, children's marks against
  the parent's mark set (`MARK_NOT_ALLOWED`), unknown mark types
  (`UNKNOWN_MARK_TYPE`), repeated mark types (`MARK_DUPLICATE`), content
  mismatches (`CONTENT_MISMATCH`), required attributes (`ATTR_REQUIRED`),
  empty text nodes. Documents outside the artifact's declared scope
  (title-less noteDoc roots, legacy node types) are reported with
  `SCHEMA_FAMILY_UNCOVERED` / `SCHEMA_NODE_LEGACY` warnings instead of
  invented verdicts.
- `wikiLink` is a known node: no unknown-type warnings, its label
  participates in text and search, annotated reading renders it as a
  markdown link.
- New limit `UM_MAX_SCHEMA_BYTES` (default 1 MiB) caps schema artifact
  files loaded by `insum schema` and `insum validate`.
- Lock/dirty-lease protocol prototype (§9.2) in `insum-core`: hidden
  sidecar files (`.<name>.insum-lock`, `.<name>.insum-lease`) next to the
  target, atomic create-new acquisition, wait/timeout with `WRITE_LOCKED`,
  op-id-checked release, and crash recovery (dead pid on the same host is
  broken immediately; cross-host and corrupt files recover by age). The
  future `apply`/`migrate-ids` entry point (`acquire_write_lock`) refuses
  to write while a live editor lease exists.
- `insum history <file> [--note <id|title>] [--limit n] [--format json|annotated]`:
  a readable change history from the git history of the repository —
  revisions newest-first (sha, UTC timestamp, author, subject), each with
  a per-note structural diff against its parent (blocks added/removed/
  changed by JSON Pointer, with `attrs.id` and a text preview when
  present). Read-only (blobs via `git show`, no checkout); a file outside
  git or without history is an explicit warning with an empty list; the
  output budget drops the oldest revisions with `complete: false` instead
  of truncating silently. Requires git in PATH.

### Fixed

- `migrate-ids` no longer fails with `INTERNAL` on a repeated `--note <id>`
  filter: a note named several times is selected and planned once, so the
  independently generated preview ids can no longer collide in the rewrite.
- Annotated reading neutralizes the `<!--` sequence coming from document
  text — document content can no longer forge CLI anchors or markers.
- The oversized-block marker is no longer lost when content follows the
  oversized block on the same page.
- Empty table rows keep their pointer anchors; multi-paragraph cells are
  separated; list items inside table cells with nested blocks stay explicit.
- `search --note <title>` with an ambiguous title aggregates the matching
  note ids across all searched files instead of reporting only the first.
- A `--note` title lookup is strictly exact: case and prefix variants miss
  (with a hint pointing to `insum ls`).

- Table projection in annotated reading no longer silently drops cell
  content: lists in cells flatten with their markers, bold/italic/code
  survive, unknown inline nodes get explicit markers, and row/cell/paragraph
 anchors are emitted inline.
- `search --note <title>` no longer masks an ambiguous title as "not found":
  the ambiguity error now lists the matching note ids.
- A block larger than the whole page budget is emitted whole with an explicit
  oversized marker (documented exception), instead of silently exceeding the
  budget.
- CI: skip a unix-only symlink test on Windows (was red under `-D warnings`).
- Annotated reading (`read --format annotated`) renders tables as markdown —
  kanban-style pages stay readable with cell text and escaped pipes.
- Read API — six commands over any `.um`/`.note` knowledge base: `ls`,
  `search`, `tree`, `block get`, `read`, `validate`. Agents get a safe JSON
  view with pointers, revisions, paging and stable error codes; files are
  never modified.
- Rust workspace scaffold with three crates (`insum-core`, `insum-cli`,
  `insum-test-utils`), CI on Linux, macOS and Windows.
- `insum --version` reports version info as JSON, `--pretty` indents any
  output, errors arrive as a single JSON object with stable codes.
- Fixture generator produces valid `.um` containers (minimal, empty, regular)
  and legacy/envelope `.note` documents for tests.

