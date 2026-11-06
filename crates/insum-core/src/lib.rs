//! insum-core — core library of the `insum` CLI.
//!
//! Owns everything that is not command-line plumbing: reading `.um` ZIP
//! containers and `.note` documents, the manifest model, JSON traversal,
//! JSON Pointers, note/file revisions, limits, and the error registry
//! (see docs/insum-spec.md). CLI commands must not duplicate this logic.
//!
//! The public behavior contract is summarized in `docs/insum-spec.md`.

pub mod canon;
pub mod container;
pub mod error;
pub mod limits;
pub mod model;
pub mod pointer;
pub mod validate;

pub use container::{NoteFile, UmContainer};
pub use error::{ErrorCode, UmError};
pub use limits::Limits;
pub use model::{BlockEntry, Manifest, NoteDescriptor, NoteDoc, traverse};
pub use pointer::{encode_token, resolve, split};
pub use validate::{Diag, NoteValidation, validate_note_doc};
