# schema/

Bundled fallback schema artifact for `insum schema` (spec §7.1,
`schema/inscriptum-schema.json`). It is generated from the real ProseMirror
schema of the obsidian-plugin by the schema-cli task — the directory is a
placeholder until then.

The schema has one source: the plugin's runtime schema. Hand-maintained copies
of node/mark type lists in Rust are forbidden (spec §3.4).
