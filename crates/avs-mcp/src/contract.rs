//! Generates the files other plugins read: `local-capability-manifest.json` and the
//! per-tool JSON schemas under `schemas/v1/leaves/`.
//!
//! Sources: `tools.json` (names, descriptions, input schemas: the same catalog
//! `tools/list` serves) and `contract.json` (authority requests and artifact ports).
//! The generated files are committed; a test fails when they drift from the sources.
//! Regenerate with `agent-video-studio-mcp --write-contract <repository root>`.

use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

const CATALOG: &str = include_str!("../tools.json");
const CONTRACT: &str = include_str!("../contract.json");
const SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";
const LEAVES: &str = "schemas/v1/leaves";

/// Tools whose success output carries the new session revision.
const SESSION_WRITERS: &[&str] = &["undo_project", "redo_project", "apply_auto_edit_plan", "import_media_file", "import_media_file_to_timeline", "import_handoff_manifest", "import_codexvideo_job"];

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_default();
    text.push('\n');
    text
}

fn schema_reference(leaf: &str, side: &str) -> Value {
    json!({
        "schemaVersion": "local-capability-schema/v1",
        "schemaId": format!("agent-video-studio/{leaf}-{side}/v1"),
        "path": format!("{LEAVES}/{leaf}-{side}.schema.json"),
    })
}

/// A tool is a timeline edit when its name is an `Operation` variant.
fn is_timeline_edit(tool: &str) -> bool {
    match serde_json::from_value::<avs_core::Operation>(json!({ "tool": tool, "args": {} })) {
        Ok(_) => true,
        Err(error) => !error.to_string().contains("unknown variant"),
    }
}

fn output_schema(tool: &str, leaf: &str) -> Value {
    let mut properties = Map::new();
    properties.insert("ok".into(), json!({ "const": true }));
    properties.insert("tool".into(), json!({ "const": tool }));
    let mut required = vec!["ok", "tool"];
    if is_timeline_edit(tool) || SESSION_WRITERS.contains(&tool) {
        properties.insert("revision".into(), json!({ "type": "integer", "minimum": 1, "description": "Project session revision after the call." }));
        required.push("revision");
    }
    json!({
        "$schema": SCHEMA_DIALECT,
        "$id": format!("agent-video-studio/{leaf}-output/v1"),
        "title": format!("{tool} output"),
        "description": format!("structuredContent of a successful agent_video_studio/{tool} call. A failed call returns {{ok: false, tool, error: {{code, message, details}}}} with isError: true instead."),
        "type": "object",
        "required": required,
        "properties": properties,
        "additionalProperties": true,
    })
}

/// Every generated file, keyed by its path relative to the repository root.
pub fn generate() -> BTreeMap<String, String> {
    let catalog: Vec<Value> = serde_json::from_str(CATALOG).expect("tools.json is valid JSON");
    let contract: Value = serde_json::from_str(CONTRACT).expect("contract.json is valid JSON");
    let server = contract["mcpServer"].as_str().expect("contract.mcpServer");
    let mut files = BTreeMap::new();
    let mut capabilities = Vec::new();

    for tool in &catalog {
        let name = tool["name"].as_str().expect("tool name");
        let leaf = name.replace('_', "-");
        let entry = contract["leaves"].get(name).unwrap_or_else(|| panic!("contract.json has no entry for {name}"));
        let mut input = tool["inputSchema"].as_object().cloned().unwrap_or_default();
        input.insert("$schema".into(), json!(SCHEMA_DIALECT));
        input.insert("$id".into(), json!(format!("agent-video-studio/{leaf}-input/v1")));
        input.insert("title".into(), json!(format!("{} input", tool["title"].as_str().unwrap_or(name))));
        input.insert("description".into(), json!(format!("Exact arguments accepted by {server}/{name}.")));
        files.insert(format!("{LEAVES}/{leaf}-input.schema.json"), pretty(&Value::Object(input)));
        files.insert(format!("{LEAVES}/{leaf}-output.schema.json"), pretty(&output_schema(name, &leaf)));
        capabilities.push(json!({
            "leafId": leaf,
            "title": tool["title"],
            "description": tool["description"],
            "executionClass": "server_stateful",
            "runtimeBinding": { "kind": "mcp_tool", "mcpServer": server, "tool": name },
            "authorityRequests": entry["authorityRequests"],
            "artifactRoots": entry["artifactRoots"],
            "inputSchema": schema_reference(&leaf, "input"),
            "outputSchema": schema_reference(&leaf, "output"),
            "inputPorts": entry["inputPorts"],
            "outputPorts": entry["outputPorts"],
        }));
    }

    let coordinator = &contract["coordinator"];
    capabilities.push(coordinator["capability"].clone());
    for side in ["input", "output"] {
        let leaf = coordinator["capability"]["leafId"].as_str().expect("coordinator leafId");
        files.insert(format!("{LEAVES}/{leaf}-{side}.schema.json"), pretty(&coordinator[format!("{side}Schema")]));
    }

    let mut manifest = contract["manifest"].as_object().cloned().expect("contract.manifest");
    manifest.insert("capabilities".into(), Value::Array(capabilities));
    files.insert("local-capability-manifest.json".into(), pretty(&Value::Object(manifest)));
    files
}

/// Writes the generated files under `root` and removes stale leaf schemas.
pub fn write(root: &std::path::Path) -> std::io::Result<usize> {
    let files = generate();
    let leaves = root.join(LEAVES);
    std::fs::create_dir_all(&leaves)?;
    for entry in std::fs::read_dir(&leaves)? {
        let path = entry?.path();
        let relative = format!("{LEAVES}/{}", path.file_name().and_then(|name| name.to_str()).unwrap_or_default());
        if path.extension().is_some_and(|extension| extension == "json") && !files.contains_key(&relative) {
            std::fs::remove_file(&path)?;
        }
    }
    for (relative, content) in &files {
        std::fs::write(root.join(relative), content)?;
    }
    Ok(files.len())
}
