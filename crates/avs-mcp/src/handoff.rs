//! `import_handoff_manifest` / `import_codexvideo_job`: bring a CodexVideo job into
//! the active project as one undoable revision.

use crate::protocol::ToolError;
use crate::server::{Server, ToolResult, expected_revision, invalid, to_json};
use avs_handoff::{ImportOptions, import_handoff};
use serde_json::Value;
use std::path::Path;

pub fn call(server: &Server, arguments: &Value) -> ToolResult {
    let fields = arguments.as_object().ok_or_else(|| invalid("Tool arguments must be an object."))?;
    let manifest_path = fields.get("manifestPath").and_then(Value::as_str).ok_or_else(|| invalid("manifestPath is required."))?;
    let options = ImportOptions {
        expected_revision: expected_revision(fields)?,
        project_title: fields.get("projectTitle").and_then(Value::as_str).map(str::to_string),
        ..ImportOptions::default()
    };
    let session = server.session()?;
    let report = import_handoff(server.env(), &session, Path::new(manifest_path), &options)
        .map_err(|error| ToolError { code: error.code.clone(), message: error.message.clone(), details: error.details.clone() })?;
    Ok(to_json(&report))
}
