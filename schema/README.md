# schema/

Bundled fallback schema artifact (`inscriptum-schema.json`) for the future
`insum schema` command. It will be generated from the runtime schema of the
Inscriptum editor — the directory is a placeholder until that pipeline lands.

The schema has exactly one source: the editor's runtime schema. Hand-maintained
copies of node/mark type lists in Rust are deliberately avoided; the read API
ships with a small built-in dictionary of observed node types instead.
