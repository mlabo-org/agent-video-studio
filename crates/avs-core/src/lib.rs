//! Agent Video Studio core: the timeline document and every edit on it.
//!
//! This crate has no I/O. The session store, renderer, MCP server and editor UI all
//! go through [`ops::apply`], so there is exactly one implementation of each edit.

pub mod error;
pub mod model;
pub mod ops;
pub mod placement;
pub mod plan;
pub mod query;
pub mod time;
pub mod validate;

pub use error::{EditError, Res};
pub use model::*;
pub use ops::{Operation, Outcome, apply};
pub use time::{Time, secs};
pub use validate::{Issue, Severity, Validation, validate};
