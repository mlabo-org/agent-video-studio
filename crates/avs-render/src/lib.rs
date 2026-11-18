//! Agent Video Studio's render layer: one composition graph that the export and the
//! live preview both use, so the picture you preview is the picture you export.
//!
//! No async runtime: ffmpeg runs as a child process and the crate talks to it with
//! threads and pipes.

mod caption;
mod error;
mod export;
mod fmt;
mod graph;
mod media;
mod mutes;
mod paths;
mod preview;
mod tool;

pub use caption::caption_font_path;
pub use error::{RenderError, Result};
pub use export::{CancelToken, ExportOptions, ExportResult, Progress, export};
pub use graph::{GraphInput, GraphOptions, RenderGraph, RenderWarning, SourceRole, Streams, UsedSource, VideoFormat, auto_fit_scale};
pub use media::MediaResolver;
pub use paths::{check_output_within, validate_output};
pub use preview::{AudioReader, Frame, PreviewOptions, PreviewStream, render_frame};
pub use tool::Toolchain;
