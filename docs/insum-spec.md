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
| `insum schema [--type <node>] [--schema <file>]` | Show the applicable schema artifact (source, path, hash, validation capability); `--type` shows one node descriptor with its example |
| `insum validate [--strict] [--hidden] [--schema <file>] <paths>...` | Validation with an explicit level: `structural` or `schema` |
| `insum history <file> [--note <id\|title>] [--limit n] [--format json\|annotated] [--max-bytes n]` | Change history of the file from git: revisions newest-first, each with a per-note structural diff against its parent |

`--note <value>` resolves the value as the note id (ULID) first, then as an
exact note title; a title matching several notes fails with their ids listed
(no silent first match), and every response still carries the canonical ULID
`note_id` — cursors and future write CAS remain id-bound.

## Schema artifact and validation levels

The editor's schema has exactly one source: the plugin generates
`schema.json` in its own directory at startup (`.obsidian/plugins/inscriptum/`
inside a vault). `insum schema` and `insum validate` resolve the artifact in
a fixed order:

1. explicit `--schema <file>` (reported as `source: "explicit"`);
2. the plugin runtime artifact found by walking up from the target file (or
   the working directory for `insum schema`) — `source: "plugin-runtime"`;
3. the CLI's bundled snapshot — `source: "bundled-fallback"`, a reserve for
   reading and diagnostics.

`insum schema` reports the artifact id, version, hash (`schema_hash`), source,
source path, plugin version and `validation_capability`, plus `hash_verified`:
the hash is recomputed (SHA-256 over RFC 8785 canonical JSON without
`schema_hash` and `plugin_version`) and compared with the declared value. A
mismatch keeps the command working and adds a `SCHEMA_MISMATCH` warning — a
stale artifact is shown, not hidden. `hash_verified` proves integrity
against accidental drift, not authenticity: inside an untrusted workspace
the vault rule trusts the workspace's own artifact; pin a trusted one with
`--schema`. A document root whose type differs from the artifact's
`top_node` is a `SCHEMA_ROOT_MISMATCH` warning (document families are
application semantics, §4.7). `--type <node>` prints the node
descriptor (content rule, groups, attrs, marks, `addressable`,
`top_node`,
`inline_content`, `searchable` and the example when the artifact carries
one); an unknown type fails with exit 6 and lists the available types.

`insum validate` reports `validation_level` per file and for the whole run:

- `structural` — container, manifest, note JSON structure, limits, ID rules
  for documents marked `features.block_ids`. Applied whenever no verified
  applicable artifact is found; the bundled fallback is labeled in the
  report but never lifts the level.
- `schema` — additionally, the document is validated with
  ProseMirror-parity semantics derived from the artifact (proven by the
  differential corpus): content expressions per node type, children's marks
  against the parent's mark set, attribute defaults and requiredness, mark
  sets (a repeated mark type fails), unknown node and mark types. A node
  type the artifact does not declare is an error — unless it is a known
  legacy type observed in real on-disk documents (reported as
  `SCHEMA_NODE_LEGACY`: document version semantics are outside the
  artifact; the legacy dictionary is the observed-inventory list the
  structural level uses, maintained in the project knowledge base). Undeclared attributes are dropped by the editor — reported as
  `SCHEMA_ATTR_UNKNOWN` warnings. A noteDoc root outside the title family
  (no leading `noteTitle`, or carrying legacy types) is outside the
  artifact's content rule by its own declaration — the root content match
  is skipped with `SCHEMA_FAMILY_UNCOVERED`; everything else is still
  checked.

`full` is never claimed by this release: it is reserved for validation proven
 by the differential test corpus. With `--strict` every warning becomes an
error, including artifact configuration warnings (`SCHEMA_MISMATCH`).

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
`UM_MAX_OPS_BYTES`, `UM_MAX_APPLY_OPS`, `UM_MAX_COMMIT_RETRIES`,
`UM_MAX_SCHEMA_BYTES` (1 MiB — schema artifact files).

## History (git-backed)

`insum history` reads the git history of the repository containing the
file; its annotated projection is forge-proof — every document-controlled
value (ids, titles, types, texts, paths, git metadata) is escaped through
the same boundary function before interpolation.
`insum history` reads the git history of the repository containing the
file — read-only: the working tree, the index and HEAD are never touched;
a file deleted from the working copy (including `rm -r` together with its
directory) is still served from git, with a
`FILE_ABSENT_IN_WORKING_COPY` warning;
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
judged), `FILE_ABSENT_IN_REVISION` (the revision deleted the file — the
diff against its parent shows the content removed),
`NOTE_NOT_FOUND_IN_HISTORY` (the `--note` filter matched no revision —
check the id or exact title), `OUTPUT_BUDGET` (the oldest revisions were
dropped to fit the budget). `--limit` bounds the revision count (default 20, cap 200); the
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
