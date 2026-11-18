//! The one error type `import_handoff` returns.

use avs_core::EditError;
use avs_session::{ErrorKind, SessionError};
use serde::Serialize;
use serde_json::{Value, json};
use std::fmt;

/// A stable `code`, a human message and structured `details` an agent can act on.
///
/// Errors raised by this crate use the `CODEXVIDEO_HANDOFF_*` codes. Errors from the
/// storage layer (`REVISION_MISMATCH`, `IMPORT_MEDIA_UNSUPPORTED_EXTENSION`, ...) and
/// from the timeline core (`CLIP_OVERLAP`, `TRACK_LOCKED`, ...) pass through with their
/// own code, kind and details unchanged.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HandoffError {
    pub kind: ErrorKind,
    pub code: String,
    pub message: String,
    pub details: Value,
}

pub type Result<T> = std::result::Result<T, HandoffError>;

impl HandoffError {
    pub(crate) fn new(kind: ErrorKind, code: &str, message: impl Into<String>) -> Self {
        HandoffError { kind, code: code.to_string(), message: message.into(), details: json!({}) }
    }

    /// The input (manifest or arguments) is malformed.
    pub(crate) fn invalid(code: &str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Invalid, code, message)
    }

    pub(crate) fn not_found(code: &str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, code, message)
    }

    pub(crate) fn conflict(code: &str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, code, message)
    }

    pub(crate) fn with(mut self, key: &str, value: impl Serialize) -> Self {
        if let Value::Object(map) = &mut self.details {
            map.insert(key.to_string(), serde_json::to_value(value).unwrap_or(Value::Null));
        }
        self
    }
}

impl From<SessionError> for HandoffError {
    fn from(error: SessionError) -> Self {
        HandoffError { kind: error.kind, code: error.code, message: error.message, details: error.details }
    }
}

impl From<EditError> for HandoffError {
    fn from(error: EditError) -> Self {
        SessionError::from(error).into()
    }
}

impl fmt::Display for HandoffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for HandoffError {}
