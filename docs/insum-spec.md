# insum specification — pointer

The canon of the insum CLI specification is the entity `insum` in the
Inscriptum knowledge base: `~/insum/DEVELOPMENT/insum.um`, pages `spec-*`
(v0.4, Draft).

This file is a pointer, not a copy — the knowledge base is the single source
of truth. Read it with `insum` itself once the read API ships, or with the
knowledge-base helpers (`um-read.mjs`).

Key sections for implementation:

- §4 — versions and data model (revisions, pointers, block IDs)
- §5 — common conventions (invocation, paths, JSON output, limits, exit codes)
- §6 — read API commands (`ls`, `tree`, `block get`, `search`, `read`, `validate`)
- §11 — errors and exit codes; §12 — safety; §14 — implementation layout
- §15 — acceptance criteria per delivery stage
