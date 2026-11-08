# Changelog

All notable changes to the `insum` CLI are documented in this file.
Format — Keep a Changelog; versioning — SemVer.

## [Unreleased]

### Added

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

### Added

- `wikiLink` is a known node: no unknown-type warnings, its label
  participates in text and search, annotated reading renders it as a
  markdown link.
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
