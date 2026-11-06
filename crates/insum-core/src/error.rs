//! Error registry — (codes) and (exit codes).
//!
//! `code` is a stable contract; `message` and `hint` are English, human-facing
//! and may change. Errors are rendered as a single JSON object on stderr with
//! an empty stdout.

use serde::{Serialize, Serializer};

/// Stable error codes from the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorCode {
    // command level
    Usage,
    IoError,
    Internal,
    // lookup
    PathNotFound,
    NoteNotFound,
    BlockNotFound,
    PointerNotFound,
    // file level
    NotAZip,
    ManifestMissing,
    ManifestInvalid,
    UnsupportedVersion,
    UnsupportedType,
    // note level
    NoteDescriptorInvalid,
    NoteFileMissing,
    NoteJsonInvalid,
    NoteDocInvalid,
    // archive level
    ArchiveEntryNameInvalid,
    ArchiveDuplicateEntry,
    // limits
    LimitExceeded,
    // schema / feature
    SchemaUnavailable,
    SchemaMismatch,
    SchemaValidationIncomplete,
    UnsupportedNodeType,
    SchemaArtifactInvalid,
    BlockIdsNotEnabled,
    // block IDs
    BlockIdMissing,
    BlockIdDuplicate,
    BlockIdInvalid,
    BlockIdImmutable,
    IdConflict,
    // operations (write API)
    InvalidOperation,
    InvalidNode,
    ContentNotAllowed,
    // concurrency / read conflicts
    NoteRevisionMismatch,
    WriteConflict,
    WriteLocked,
    CursorStale,
}

impl ErrorCode {
    /// Wire representation of the code,
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Usage => "USAGE",
            ErrorCode::IoError => "IO_ERROR",
            ErrorCode::Internal => "INTERNAL",
            ErrorCode::PathNotFound => "PATH_NOT_FOUND",
            ErrorCode::NoteNotFound => "NOTE_NOT_FOUND",
            ErrorCode::BlockNotFound => "BLOCK_NOT_FOUND",
            ErrorCode::PointerNotFound => "POINTER_NOT_FOUND",
            ErrorCode::NotAZip => "NOT_A_ZIP",
            ErrorCode::ManifestMissing => "MANIFEST_MISSING",
            ErrorCode::ManifestInvalid => "MANIFEST_INVALID",
            ErrorCode::UnsupportedVersion => "UNSUPPORTED_VERSION",
            ErrorCode::UnsupportedType => "UNSUPPORTED_TYPE",
            ErrorCode::NoteDescriptorInvalid => "NOTE_DESCRIPTOR_INVALID",
            ErrorCode::NoteFileMissing => "NOTE_FILE_MISSING",
            ErrorCode::NoteJsonInvalid => "NOTE_JSON_INVALID",
            ErrorCode::NoteDocInvalid => "NOTE_DOC_INVALID",
            ErrorCode::ArchiveEntryNameInvalid => "ARCHIVE_ENTRY_NAME_INVALID",
            ErrorCode::ArchiveDuplicateEntry => "ARCHIVE_DUPLICATE_ENTRY",
            ErrorCode::LimitExceeded => "LIMIT_EXCEEDED",
            ErrorCode::SchemaUnavailable => "SCHEMA_UNAVAILABLE",
            ErrorCode::SchemaMismatch => "SCHEMA_MISMATCH",
            ErrorCode::SchemaValidationIncomplete => "SCHEMA_VALIDATION_INCOMPLETE",
            ErrorCode::UnsupportedNodeType => "UNSUPPORTED_NODE_TYPE",
            ErrorCode::SchemaArtifactInvalid => "SCHEMA_ARTIFACT_INVALID",
            ErrorCode::BlockIdsNotEnabled => "BLOCK_IDS_NOT_ENABLED",
            ErrorCode::BlockIdMissing => "BLOCK_ID_MISSING",
            ErrorCode::BlockIdDuplicate => "BLOCK_ID_DUPLICATE",
            ErrorCode::BlockIdInvalid => "BLOCK_ID_INVALID",
            ErrorCode::BlockIdImmutable => "BLOCK_ID_IMMUTABLE",
            ErrorCode::IdConflict => "ID_CONFLICT",
            ErrorCode::InvalidOperation => "INVALID_OPERATION",
            ErrorCode::InvalidNode => "INVALID_NODE",
            ErrorCode::ContentNotAllowed => "CONTENT_NOT_ALLOWED",
            ErrorCode::NoteRevisionMismatch => "NOTE_REVISION_MISMATCH",
            ErrorCode::WriteConflict => "WRITE_CONFLICT",
            ErrorCode::WriteLocked => "WRITE_LOCKED",
            ErrorCode::CursorStale => "CURSOR_STALE",
        }
    }

    /// Process exit code for the code,
    pub fn exit_code(self) -> i32 {
        match self {
            // 2 — invalid arguments / option combination
            ErrorCode::Usage | ErrorCode::BlockIdImmutable | ErrorCode::InvalidOperation => 2,
            // 1 — internal error or I/O
            ErrorCode::IoError | ErrorCode::Internal => 1,
            // 3 — path, note or block not found
            ErrorCode::PathNotFound
            | ErrorCode::NoteNotFound
            | ErrorCode::BlockNotFound
            | ErrorCode::PointerNotFound => 3,
            // 4 — invalid or unreadable data
            ErrorCode::NotAZip
            | ErrorCode::ManifestMissing
            | ErrorCode::ManifestInvalid
            | ErrorCode::UnsupportedVersion
            | ErrorCode::UnsupportedType
            | ErrorCode::NoteDescriptorInvalid
            | ErrorCode::NoteFileMissing
            | ErrorCode::NoteJsonInvalid
            | ErrorCode::NoteDocInvalid
            | ErrorCode::ArchiveEntryNameInvalid
            | ErrorCode::ArchiveDuplicateEntry
            | ErrorCode::BlockIdMissing
            | ErrorCode::BlockIdDuplicate
            | ErrorCode::BlockIdInvalid
            | ErrorCode::IdConflict
            | ErrorCode::InvalidNode
            | ErrorCode::ContentNotAllowed => 4,
            // 5 — safety limit exceeded
            ErrorCode::LimitExceeded => 5,
            // 6 — incompatible / unavailable schema or feature
            ErrorCode::SchemaUnavailable
            | ErrorCode::SchemaMismatch
            | ErrorCode::SchemaValidationIncomplete
            | ErrorCode::UnsupportedNodeType
            | ErrorCode::SchemaArtifactInvalid
            | ErrorCode::BlockIdsNotEnabled => 6,
            // 7 — version / write / lock conflict
            ErrorCode::NoteRevisionMismatch
            | ErrorCode::WriteConflict
            | ErrorCode::WriteLocked
            | ErrorCode::CursorStale => 7,
        }
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// A structured error: one JSON object on stderr, empty stdout.
#[derive(Debug, Clone, Serialize)]
pub struct UmError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl UmError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            details: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Usage, message).with_hint("Run `insum --help` for usage.")
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }
}

impl std::fmt::Display for UmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for UmError {}

/// The JSON envelope printed to stderr on failure.
#[derive(Serialize)]
pub struct ErrorEnvelope<'a> {
    pub api_version: u32,
    pub error: &'a UmError,
}

impl UmError {
    pub fn envelope(&self) -> ErrorEnvelope<'_> {
        ErrorEnvelope {
            api_version: 1,
            error: self,
        }
    }
}

impl From<std::io::Error> for UmError {
    fn from(err: std::io::Error) -> Self {
        UmError::new(ErrorCode::IoError, err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec — the minimal registry: every code keeps its exit code.
    #[test]
    fn exit_codes_match_spec() {
        let cases: &[(ErrorCode, i32)] = &[
            (ErrorCode::Usage, 2),
            (ErrorCode::IoError, 1),
            (ErrorCode::Internal, 1),
            (ErrorCode::PathNotFound, 3),
            (ErrorCode::NoteNotFound, 3),
            (ErrorCode::BlockNotFound, 3),
            (ErrorCode::PointerNotFound, 3),
            (ErrorCode::NotAZip, 4),
            (ErrorCode::ManifestMissing, 4),
            (ErrorCode::ManifestInvalid, 4),
            (ErrorCode::UnsupportedVersion, 4),
            (ErrorCode::UnsupportedType, 4),
            (ErrorCode::NoteDescriptorInvalid, 4),
            (ErrorCode::NoteFileMissing, 4),
            (ErrorCode::NoteJsonInvalid, 4),
            (ErrorCode::NoteDocInvalid, 4),
            (ErrorCode::ArchiveEntryNameInvalid, 4),
            (ErrorCode::ArchiveDuplicateEntry, 4),
            (ErrorCode::LimitExceeded, 5),
            (ErrorCode::SchemaUnavailable, 6),
            (ErrorCode::SchemaMismatch, 6),
            (ErrorCode::SchemaValidationIncomplete, 6),
            (ErrorCode::UnsupportedNodeType, 6),
            (ErrorCode::SchemaArtifactInvalid, 6),
            (ErrorCode::BlockIdsNotEnabled, 6),
            (ErrorCode::BlockIdMissing, 4),
            (ErrorCode::BlockIdDuplicate, 4),
            (ErrorCode::BlockIdInvalid, 4),
            (ErrorCode::BlockIdImmutable, 2),
            (ErrorCode::IdConflict, 4),
            (ErrorCode::InvalidOperation, 2),
            (ErrorCode::InvalidNode, 4),
            (ErrorCode::ContentNotAllowed, 4),
            (ErrorCode::NoteRevisionMismatch, 7),
            (ErrorCode::WriteConflict, 7),
            (ErrorCode::WriteLocked, 7),
            (ErrorCode::CursorStale, 7),
        ];
        for (code, exit) in cases {
            assert_eq!(
                code.exit_code(),
                *exit,
                "wrong exit code for {}",
                code.as_str()
            );
        }
    }

    #[test]
    fn wire_names_match_spec() {
        assert_eq!(ErrorCode::NotAZip.as_str(), "NOT_A_ZIP");
        assert_eq!(
            ErrorCode::NoteRevisionMismatch.as_str(),
            "NOTE_REVISION_MISMATCH"
        );
        assert_eq!(ErrorCode::CursorStale.as_str(), "CURSOR_STALE");
    }

    #[test]
    fn envelope_serializes_api_version_and_omits_empty() {
        let err = UmError::new(ErrorCode::PathNotFound, "no such path");
        let json = serde_json::to_value(err.envelope()).unwrap();
        assert_eq!(json["api_version"], 1);
        assert_eq!(json["error"]["code"], "PATH_NOT_FOUND");
        assert!(json["error"].get("hint").is_none());
        assert!(json["error"].get("details").is_none());
    }
}
