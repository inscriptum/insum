# insum

CLI for reading and writing Inscriptum knowledge bases — `.um` containers and `.note` documents.

`insum` gives agents and scripts a machine-readable interface to Inscriptum documents:
a JSON Read API over ZIP containers and ProseMirror JSON, stable block IDs, migration,
and atomic block-level writes.

Planned command groups:

- **Read** — `ls`, `search`, `tree`, `block get`, `read`, `validate`
- **Write** (later releases) — `migrate-ids`, `apply`

## Status

Work in progress. The specification lives in the Inscriptum knowledge base
(`DEVELOPMENT/insum.um`, pages `spec-*`); it is the canon, not this repository.
The first read-only release is planned as `0.1.0`.

## License

Dual-licensed under `MIT OR Apache-2.0` — see [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).

The previous contents of this repository (a lerna monorepo of UI components and
configs) are archived, with full history, in the [`archive/ui-configs`](https://github.com/inscriptum/insum/tree/archive/ui-configs)
branch under Apache-2.0.
