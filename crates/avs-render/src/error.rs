//! The one error type this crate returns.

use serde::{Serialize, Serializer};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum RenderError {
    /// ffmpeg or ffprobe could not be found or does not run.
    ToolchainMissing(String),
    /// The ffmpeg build lacks a filter the graph needs.
    MissingFilter(String),
    /// The options or the project cannot be rendered as given.
    InvalidOptions(String),
    /// The requested time range holds nothing (empty timeline, or `from` past the end).
    EmptyRange(String),
    /// The output path does not satisfy the export contract.
    InvalidOutput(String),
    /// A clip's media could not be located or opened.
    MediaUnavailable { media_id: String, source: String, reason: String },
    /// No usable caption font is installed.
    FontUnavailable(String),
    /// A helper process did not finish in time.
    Timeout(String),
    /// ffmpeg ran and failed.
    FfmpegFailed { exit_code: Option<i32>, stderr: String, command: String },
    /// The caller cancelled the export.
    Cancelled,
    Io(String),
}

impl RenderError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            RenderError::ToolchainMissing(_) => "FFMPEG_NOT_FOUND",
            RenderError::MissingFilter(_) => "FFMPEG_FILTER_MISSING",
            RenderError::InvalidOptions(_) => "INVALID_RENDER_OPTIONS",
            RenderError::EmptyRange(_) => "EMPTY_RENDER_RANGE",
            RenderError::InvalidOutput(_) => "INVALID_OUTPUT_PATH",
            RenderError::MediaUnavailable { .. } => "MEDIA_UNAVAILABLE",
            RenderError::FontUnavailable(_) => "CAPTION_FONT_UNAVAILABLE",
            RenderError::Timeout(_) => "HELPER_TIMEOUT",
            RenderError::FfmpegFailed { .. } => "FFMPEG_FAILED",
            RenderError::Cancelled => "RENDER_CANCELLED",
            RenderError::Io(_) => "RENDER_IO_ERROR",
        }
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, RenderError::Cancelled)
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::ToolchainMissing(message)
            | RenderError::MissingFilter(message)
            | RenderError::InvalidOptions(message)
            | RenderError::EmptyRange(message)
            | RenderError::InvalidOutput(message)
            | RenderError::FontUnavailable(message)
            | RenderError::Timeout(message)
            | RenderError::Io(message) => write!(f, "{}: {message}", self.code()),
            RenderError::MediaUnavailable { media_id, source, reason } => {
                write!(f, "{}: media {media_id} ({source}): {reason}", self.code())
            }
            RenderError::FfmpegFailed { exit_code, stderr, .. } => {
                let code = exit_code.map_or_else(|| "signal".to_string(), |code| code.to_string());
                write!(f, "{}: ffmpeg exited with {code}", self.code())?;
                if !stderr.trim().is_empty() {
                    write!(f, "\n{}", stderr.trim())?;
                }
                Ok(())
            }
            RenderError::Cancelled => write!(f, "{}: the render was cancelled", self.code()),
        }
    }
}

impl std::error::Error for RenderError {}

impl From<std::io::Error> for RenderError {
    fn from(error: std::io::Error) -> Self {
        RenderError::Io(error.to_string())
    }
}

impl Serialize for RenderError {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("RenderError", 2)?;
        state.serialize_field("code", self.code())?;
        state.serialize_field("message", &self.to_string())?;
        state.end()
    }
}

pub type Result<T> = std::result::Result<T, RenderError>;
