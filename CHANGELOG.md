# Changelog

All notable changes to the `insum` CLI are documented in this file.
Format — Keep a Changelog; versioning — SemVer.

## [Unreleased]

### Added

- Rust workspace scaffold with three crates (`insum-core`, `insum-cli`,
  `insum-test-utils`), CI on Linux, macOS and Windows.
- `insum --version` reports version info as JSON, `--pretty` indents any
  output, errors arrive as a single JSON object with stable codes.
- Fixture generator produces valid `.um` containers (minimal, empty, regular)
  and legacy/envelope `.note` documents for tests.
