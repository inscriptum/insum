//! insum-core — core library of the `insum` CLI.
//!
//! Owns everything that is not command-line plumbing: reading `.um` ZIP
//! containers and `.note` documents, the manifest model, JSON traversal,
//! JSON Pointers, note/file revisions, limits, and the error registry
//! (spec §5–§6, §11–§12). CLI commands must not duplicate this logic.
//!
//! The specification canon lives in the Inscriptum knowledge base
//! (`DEVELOPMENT/insum.um`, pages `spec-*`), not in this repository.

pub mod error;

pub use error::{ErrorCode, UmError};
