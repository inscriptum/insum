# insum

CLI for reading and writing Inscriptum knowledge bases — `.um` containers and `.note` documents.

`insum` gives agents and scripts a machine-readable interface to Inscriptum documents:
a JSON Read API over ZIP containers and ProseMirror JSON, stable block IDs, migration,
and atomic block-level writes.

Command groups:

- **Read** — `ls`, `search`, `tree`, `block get`, `read`, `validate`, `schema`, `history`
- **Write** — `migrate-ids` (shipped); `apply` (block-level writes, planned)

## Requirements

- Rust toolchain to build; the `insum` binary itself has no runtime
  dependencies beyond the OS.
- `git` in PATH — only for `insum history` (it reads the git history of
  the target file); every other command works without git.

## Status

Work in progress. The first read-only release is planned as `0.1.0`; see
[CHANGELOG.md](CHANGELOG.md) for what already works and
[docs/insum-spec.md](docs/insum-spec.md) for the CLI behavior contract.

## Writing: `migrate-ids` and the `features.block_ids` marker

`insum migrate-ids <file> [--note <id>]... [--dry-run]` assigns guaranteed
block IDs to a note's addressable blocks and stamps the
`features.block_ids` marker. It is the **only writer** of the marker: the
marker is stamped after the IDs of that exact note have been verified and
fixed, together with the schema fingerprint they were verified under. The
migration runs transactionally — a sidecar lock, revision rechecks inside
the lock, validation of the result before an atomic replace — and `--dry-run`
never touches the file. The response budget respects `UM_MAX_OUTPUT_BYTES`;
an oversized report is explicitly marked incomplete (`complete: false`,
`truncated: true`) — the write itself always covers every selected note.

Before the marker, IDs in a note are **not guaranteed** (an older editor
version can drop them on save): reading a block by ID still works whenever
the ID is present (`insum block get --id`, with a warning on unmarked
notes), but the marker is what will vouch for IDs in the write API.

Rollback and one-shot semantics:

- The migration is **one-shot**: a rerun on a migrated note is a no-op
  (`written: false`); there is no `--unmark` flag and no built-in undo.
- Rollback is the user's git: restore the file from history
  (`git checkout <commit> -- <file>` for a `.um`/`.note` inside a
  repository). `insum` never rewrites history itself.
- Removing the marker afterwards means hand-editing the manifest descriptor
  (`.um`) or the envelope (`.note`) — possible, but not recommended: the
  marker records a verification, and stripping it by hand leaves the IDs
  unverified again (the same caveat applies to an old plugin version
  resaving the note, which can drop IDs while the marker remains — re-run
  `migrate-ids` to re-verify).

## License

Dual-licensed under `MIT OR Apache-2.0` — see [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).

The previous contents of this repository (a lerna monorepo of UI components and
configs) are archived, with full history, in the [`archive/ui-configs`](https://github.com/inscriptum/insum/tree/archive/ui-configs)
branch under Apache-2.0.
