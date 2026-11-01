//! insum-core — core library of the `insum` CLI.
//!
//! Owns everything that is not command-line plumbing: reading `.um` ZIP
//! containers and `.note` documents, the manifest model, JSON traversal,
//! JSON Pointers, note/file revisions, limits, and the error registry
//! (spec §5–§6, §11–§12). CLI commands must not duplicate this logic.
//!
//! The specification canon lives in the Inscriptum knowledge base
//! (`DEVELOPMENT/insum.um`, pages `spec-*`), not in this repository.

pub mod canon;
pub mod container;
pub mod error;
pub mod limits;
pub mod model;
pub mod pointer;

pub use container::{NoteFile, UmContainer};
pub use error::{ErrorCode, UmError};
pub use limits::Limits;
pub use model::{BlockEntry, Manifest, NoteDescriptor, NoteDoc, traverse};
pub use pointer::{encode_token, resolve, split};
