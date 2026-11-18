//! The one error type every edit returns.

use serde::Serialize;
use serde_json::{Value, json};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EditError {
    /// Stable machine-readable code, e.g. `CLIP_OVERLAP`.
    pub code: &'static str,
    pub message: String,
    /// Structured context an agent can inspect (ids, times).
    pub details: Value,
}

impl EditError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), details: json!({}) }
    }

    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        if let Value::Object(map) = &mut self.details {
            map.insert(key.to_string(), value.into());
        }
        self
    }
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for EditError {}

pub type Res<T> = Result<T, EditError>;
