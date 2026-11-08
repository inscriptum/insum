# insum CLI — behavior contract

A short, implementation-accurate summary of how `insum` behaves today.
The full specification is being prepared for publication in this directory;
until then this document is the reference for consumers (agents and scripts).

## Inputs

- `*.um` — a UM container: a ZIP archive with `manifest.json` at the root and
  note payloads under `notes/<id>.json`. ProseMirror-like JSON documents
  (`type: "noteDoc"`).
- `*.note` — a single note document: a raw `noteDoc` JSON (legacy) or an
  `inscriptum-note` envelope (version 1).
- A directory argument is walked recursively; hidden files and `node_modules`
  are skipped unless `--hidden`; traversal never follows symlinks (explicitly
  named arguments are opened directly).
- Untrusted input: ZIP entry names are validated (no absolute paths, `..`,
  backslashes, duplicates), sizes and totals are enforced on actual data.

## Commands (read-only today)

| Command | Purpose |
| --- | --- |
| `insum ls [--hidden] [<path>]` | Classify files and count notes; a directory listing never hashes files or opens payloads |
| `insum tree <file> [--note <id>]... [--depth n] [--text-limit n] [--max-bytes n] [--cursor t]` | Flat preorder block listing with JSON Pointers |
| `insum block get <file> [--note <id>] --pointer <p>` | Full subtree of one block (wire form without `attrs.id`; `--raw` keeps it) |
| `insum search [-F] [-i] [--limit n] [--context 0..5] <query> [<paths>...]` | Full-text search over inline text; best-effort across files |
| `insum read <file> [--note <id>] [--format json\|annotated]` | Whole-note reading; `annotated` emits `<!-- um:pointer -->` anchors |
| `insum validate [--strict] [--hidden] <paths>...` | Structural validation (`validation_level: "structural"`) |
| `insum history <file> [--note <id\|title>] [--limit n] [--format json\|annotated] [--max-bytes n]` | Change history of the file from git: revisions newest-first, each with a per-note structural diff against its parent |

`--note <value>` resolves the value as the note id (ULID) first, then as an
exact note title; a title matching several notes fails with their ids listed
(no silent first match), and every response still carries the canonical ULID
`note_id` — cursors and future write CAS remain id-bound.

## Output conventions

- Success: exactly one JSON document on stdout, trailing newline; every
  response carries `api_version: 1`.
- Failure: stdout stays empty; one JSON object on stderr —
  `{api_version, error: {code, message, hint?}}` — with a stable `code` from
  the error registry and an exit code per class:
  `0` success · `1` internal/IO · `2` usage · `3` not found · `4` invalid data
  · `5` limit exceeded · `6` schema · `7` conflict/stale.
- `--pretty` changes only the formatting (two-space indent).
- Files are sorted by path; notes by manifest order (`order`, then `id`);
  blocks in document order.

## Revisions

- `note_rev` — SHA-256 of `UM-NOTE-REV-1\x00` + big-endian u64 length +
  raw payload bytes + big-endian u64 length + canonical (sorted-keys,
  compact) JSON of the note's manifest descriptor.
- `file_rev` — SHA-256 of all file bytes; for `.note` files both revisions
  hash the whole file.
- Rendered as `sha256:<64 hex>` in every response.

## Pagination

Responses respect `UM_MAX_OUTPUT_BYTES` / `--max-bytes` and never cut a JSON
record in half — with the single-oversized-block exception documented below.
An incomplete response carries `complete: false` and an opaque `next_cursor`
token; pass it back via `--cursor`. Cursors are bound to the query options
and the revision of the note they continue; a stale cursor fails with exit 7
instead of silently resuming.

Documented exception: a single block that does not fit into the remaining
budget is still emitted whole rather than cut — `read --format annotated`
appends an explicit `<!-- um:oversized-block pointer="..." budget="..." -->`
marker (the marker records the page budget, not the remainder) and `tree`
reports the affected pointers in the note's `oversized_blocks` field.

## Limits

Configured via environment variables with safe defaults, enforced on actual
data: `UM_MAX_ENTRY_BYTES` (32 MiB), `UM_MAX_ARCHIVE_ENTRIES` (10 000),
`UM_MAX_TOTAL_BYTES` (256 MiB), `UM_MAX_DEPTH` (128),
`UM_MAX_OUTPUT_BYTES` (64 KiB), `UM_MAX_SEARCH_MATCHES` (1 000),
`UM_MAX_OPS_BYTES`, `UM_MAX_APPLY_OPS`, `UM_MAX_COMMIT_RETRIES`.

## History (git-backed)

`insum history` reads the git history of the repository containing the
file — read-only: the working tree, the index and HEAD are never touched;
revision blobs come from `git show <sha>:<path>`, and the git environment
is scrubbed of `GIT_*` variables so an inherited `GIT_DIR` cannot redirect
the read. The git binary must be in PATH (CI runners and agent machines
have it; everything else in `insum` works without git). Revisions are
listed newest-first with sha, parents, commit timestamp (UTC, from git),
author and subject — `--full-history` includes commits from side branches,
and every diff runs against the revision's real first parent. Each
revision lists what changed per note: notes added/removed and blocks
added/removed/changed with their JSON Pointer, `attrs.id` when present
(never required), and a short text preview. Blocks are matched
positionally by pointer inside one note — a moved block shows up as
remove+add (documented prototype limitation).

Warnings a consumer must expect: `NO_GIT_REPOSITORY` / `NO_GIT_HISTORY`
(the file is outside a work tree or untouched — empty list, not an
error), `HAS_MORE_REVISIONS` (older revisions exist beyond `--limit`),
`LOG_RECORDS_SKIPPED` (a malformed git record — object names that are not
hex — was ignored), `BLOB_NOT_PARSEABLE` / `PARENT_BLOB_NOT_PARSEABLE`
(a revision or its parent is not a parseable .um/.note — its diff is not
judged). `--limit` bounds the revision count (default 20, cap 200); the
output budget never truncates silently — the oldest revisions are dropped
with `complete: false`, `revisions_dropped` and an `OUTPUT_BUDGET`
warning, and a single revision that cannot fit is a `LIMIT_EXCEEDED`
error. The budget applies to the compact JSON document; `--pretty` renders
larger (a documented systemic property shared by every command).

## Notes on data fidelity

- Unknown node or mark types are preserved verbatim in outputs derived from
  the document and reported as `UNKNOWN_NODE_TYPE` warnings; text behind an
  unknown inline node is withheld (`text_incomplete`) rather than partially
  extracted — search lowers `complete` accordingly, so "zero matches with
  `complete: true`" stays a trustworthy absence proof.
- Annotated projection: document text containing the comment-opener sequence
  `<!--` is neutralized to `<! --` so it cannot forge CLI anchors or markers;
  inside tables, row/cell/paragraph anchors are emitted inline within the
  cells, and anything unrenderable in a cell is marked explicitly.
- No stage-A command modifies input files, creates temp files, executes
  document content, or performs network access.
