//! The one error type every session operation returns.

use avs_core::EditError;
use serde::Serialize;
use serde_json::{Value, json};
use std::fmt;
use std::path::Path;

/// Coarse class of an error, so a caller can branch without matching code strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// `expected_revision` did not match the stored revision.
    RevisionMismatch,
    /// The advisory lock could not be taken before the deadline.
    LockTimeout,
    /// `avs_core` rejected an edit; `code` is the core error code.
    Edit,
    /// The operating system refused a read or write.
    Io,
    /// The thing asked for does not exist.
    NotFound,
    /// The input or a stored file is malformed.
    Invalid,
    /// The request is valid but conflicts with current state.
    Conflict,
    /// A safety rule blocks the operation; `details.blockers` lists every reason.
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionError {
    pub kind: ErrorKind,
    /// Stable machine-readable code, e.g. `REVISION_MISMATCH` or a core code such as `CLIP_OVERLAP`.
    pub code: String,
    pub message: String,
    /// Structured context an agent can inspect (revisions, paths, issue lists).
    pub details: Value,
}

pub type Result<T> = std::result::Result<T, SessionError>;

impl SessionError {
    pub fn new(kind: ErrorKind, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { kind, code: code.into(), message: message.into(), details: json!({}) }
    }

    pub fn with(mut self, key: &str, value: impl Serialize) -> Self {
        if let Value::Object(map) = &mut self.details {
            map.insert(key.to_string(), serde_json::to_value(value).unwrap_or(Value::Null));
        }
        self
    }

    pub fn invalid(code: &str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Invalid, code, message)
    }

    pub fn not_found(code: &str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, code, message)
    }

    pub fn conflict(code: &str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, code, message)
    }

    pub fn io(operation: &str, path: &Path, error: &std::io::Error) -> Self {
        Self::new(ErrorKind::Io, "IO_ERROR", format!("{operation} failed for {}: {error}", path.display()))
            .with("operation", operation)
            .with("path", path.display().to_string())
            .with("osError", error.to_string())
    }

    pub fn revision_mismatch(code: &str, what: &str, expected: u64, actual: u64) -> Self {
        Self::new(ErrorKind::RevisionMismatch, code, format!("{what} revision mismatch: expected {expected}, found {actual}."))
            .with("expectedRevision", expected)
            .with("actualRevision", actual)
    }

    pub fn is_revision_mismatch(&self) -> bool {
        self.kind == ErrorKind::RevisionMismatch
    }
}

impl From<EditError> for SessionError {
    fn from(error: EditError) -> Self {
        SessionError { kind: ErrorKind::Edit, code: error.code.to_string(), message: error.message, details: error.details }
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SessionError {}
