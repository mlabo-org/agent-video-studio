//! The tool surface, driven through the JSON-RPC handler the stdio loop uses.

use avs_mcp::protocol::handle_input;
use avs_mcp::{Server, ToolHost};
use avs_session::Env;
use serde_json::{Value, json};
use std::process::Command;

struct Harness {
    server: Server,
    _home: tempfile::TempDir,
    home: std::path::PathBuf,
}

fn harness() -> Harness {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().canonicalize().unwrap();
    let mut env = Env::new(&path);
    env.path_dirs = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    Harness { server: Server::new(env), _home: home, home: path }
}

impl Harness {
    fn call(&self, name: &str, arguments: Value) -> Value {
        let response = handle_input(&self.server, &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": arguments } })).unwrap();
        response["result"]["structuredContent"].clone()
    }

    fn ok(&self, name: &str, arguments: Value) -> Value {
        let result = self.call(name, arguments);
        assert_eq!(result["ok"], true, "{name}: {result}");
        result
    }

    fn fail(&self, name: &str, arguments: Value) -> String {
        let result = self.call(name, arguments);
        assert_eq!(result["ok"], false, "{name} should fail: {result}");
        result["error"]["code"].as_str().unwrap().to_string()
    }

    /// A 4 s test video with sound, made with ffmpeg.
    fn sample_video(&self) -> String {
        let path = self.home.join("sample.mp4");
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x180:rate=30:duration=4", "-f", "lavfi", "-i", "sine=frequency=440:duration=4"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(&path)
            .status()
            .expect("ffmpeg is required for this test");
        assert!(status.success());
        path.display().to_string()
    }
}

#[test]
fn every_listed_tool_is_implemented_and_every_edit_is_listed() {
    let harness = harness();
    let tools = harness.server.tools();
    assert!(tools.len() >= 68, "{} tools", tools.len());
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        assert!(tool["description"].as_str().is_some_and(|text| text.len() > 20), "{name} needs a description");
        assert_eq!(tool["inputSchema"]["type"], "object", "{name}");
        // Calling with no arguments must reach the tool: any outcome except UNKNOWN_TOOL.
        let result = harness.call(name, json!({}));
        assert_ne!(result["error"]["code"], "UNKNOWN_TOOL", "{name} is listed but not implemented");
    }
    let listed: Vec<&str> = tools.iter().map(|tool| tool["name"].as_str().unwrap()).collect();
    // Every timeline edit the core knows is reachable as a tool of the same name.
    let sample = json!({ "tool": "set_track_muted", "args": { "trackId": "A1", "muted": true } });
    let operation: avs_core::Operation = serde_json::from_value(sample).unwrap();
    assert!(listed.contains(&operation.tool_name().as_str()));
    for name in ["add_clips", "set_project_settings", "extract_range", "import_handoff_manifest", "start_render_final_export"] {
        assert!(listed.contains(&name), "{name} missing from the catalog");
    }
    assert!(!listed.contains(&"migrate_legacy_project_session"));
}

#[test]
fn every_argument_in_the_published_schema_is_understood_by_the_edit() {
    // The catalog is the public contract; the core types are what actually run. A
    // property the core does not know must fail here, not be ignored at run time.
    let harness = harness();
    for tool in harness.server.tools() {
        let name = tool["name"].as_str().unwrap();
        let probe = json!({ "tool": name, "args": {} });
        if serde_json::from_value::<avs_core::Operation>(probe).is_err_and(|error| error.to_string().contains("unknown variant")) {
            continue; // not a timeline edit
        }
        for property in tool["inputSchema"]["properties"].as_object().unwrap().keys() {
            if ["expectedRevision", "confirm", "startTime"].contains(&property.as_str()) {
                continue; // handled by the server before the edit is built
            }
            let error = serde_json::from_value::<avs_core::Operation>(json!({ "tool": name, "args": { property: null } })).err().map(|error| error.to_string()).unwrap_or_default();
            assert!(!error.contains("unknown field"), "{name}.{property}: {error}");
        }
    }
}

#[test]
fn protocol_handshake_and_listing() {
    let harness = harness();
    let init = handle_input(&harness.server, &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } })).unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "agent-video-studio");
    assert!(handle_input(&harness.server, &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());
    let list = handle_input(&harness.server, &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).unwrap();
    assert!(list["result"]["tools"].as_array().unwrap().len() >= 68);
}

#[test]
fn an_agent_can_import_edit_undo_and_export() {
    let harness = harness();
    let video = harness.sample_video();

    let imported = harness.ok("import_media_file_to_timeline", json!({ "sourcePath": video, "targetTrackId": "V1", "startSeconds": 0 }));
    assert_eq!(imported["revision"], 2);
    let timeline = harness.ok("get_timeline", json!({}));
    let tracks = timeline["timeline"]["tracks"].as_array().unwrap();
    assert_eq!(tracks[0]["clips"].as_array().unwrap().len(), 1, "video clip on V1");
    assert_eq!(tracks[1]["clips"].as_array().unwrap().len(), 1, "linked sound on A1");
    let clip_id = tracks[0]["clips"][0]["id"].as_str().unwrap().to_string();

    // A stale revision is refused and changes nothing.
    assert_eq!(harness.fail("split_clip", json!({ "clipId": clip_id, "time": 2, "expectedRevision": 1 })), "REVISION_MISMATCH");
    let split = harness.ok("split_clip", json!({ "clipId": clip_id, "time": 2, "expectedRevision": 2 }));
    assert_eq!(split["revision"], 3);
    assert_eq!(harness.ok("inspect_timeline", json!({}))["inspection"]["clipCount"], 4);

    // Bulk removal needs an explicit confirm.
    assert_eq!(harness.fail("extract_range", json!({ "startSeconds": 1, "endSeconds": 2 })), "CONFIRMATION_REQUIRED");
    harness.ok("extract_range", json!({ "startSeconds": 1, "endSeconds": 2, "confirm": true }));
    assert_eq!(harness.ok("get_timeline", json!({}))["timeline"]["durationSeconds"], 3.0);

    harness.ok("undo_project", json!({}));
    assert_eq!(harness.ok("get_timeline", json!({}))["timeline"]["durationSeconds"], 4.0);
    let status = harness.ok("get_project_session_status", json!({}));
    assert_eq!(status["canRedo"], true);

    // Errors carry the core's code.
    assert_eq!(harness.fail("delete_clip", json!({ "clipId": "nope" })), "CLIP_NOT_FOUND");

    // Export: only the library's own folders are writable.
    assert_eq!(harness.fail("render_final_export", json!({ "outputPath": harness.home.join("elsewhere.mp4").display().to_string() })), "INVALID_OUTPUT_PATH");
    let rendered = harness.ok("render_final_export", json!({}));
    let output = rendered["render"]["output"].as_str().unwrap();
    assert!(std::fs::metadata(output).unwrap().len() > 1000);

    // Background job: visible through the status tool until it completes.
    let started = harness.ok("start_render_final_export", json!({}));
    let job_id = started["job"]["jobId"].as_str().unwrap().to_string();
    let mut state = String::new();
    for _ in 0..200 {
        state = harness.ok("get_render_final_export_status", json!({ "jobId": job_id }))["job"]["state"].as_str().unwrap().to_string();
        if state == "completed" || state == "failed" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(state, "completed");
}

#[test]
fn a_plan_is_inspected_then_applied_by_hash() {
    let harness = harness();
    let video = harness.sample_video();
    harness.ok("import_media_file_to_timeline", json!({ "sourcePath": video, "targetTrackId": "V1", "startSeconds": 0 }));
    let planned = harness.ok("plan_silence_removal", json!({ "planId": "p1", "producer": { "id": "test" }, "trackIds": ["V1"], "silenceRanges": [{ "id": "s", "startSeconds": 1, "endSeconds": 3 }] }));
    assert_eq!(planned["mutated"], false);
    let (plan, hash) = (planned["plan"].clone(), planned["planHash"].as_str().unwrap().to_string());
    let inspected = harness.ok("inspect_auto_edit_plan", json!({ "plan": plan }));
    assert_eq!(inspected["planHash"], hash);

    assert_eq!(harness.fail("apply_auto_edit_plan", json!({ "plan": plan, "expectedPlanHash": hash, "expectedRevision": 2 })), "CONFIRMATION_REQUIRED");
    assert_eq!(harness.fail("apply_auto_edit_plan", json!({ "plan": plan, "expectedPlanHash": "0000", "expectedRevision": 2, "confirm": true })), "AUTO_EDIT_PLAN_HASH_MISMATCH");
    let applied = harness.ok("apply_auto_edit_plan", json!({ "plan": plan, "expectedPlanHash": hash, "expectedRevision": 2, "confirm": true }));
    assert_eq!(applied["revision"], 3);
    assert_eq!(applied["durationSeconds"], 2.2);
    // The same plan cannot be applied twice: its base revision is now stale.
    assert_eq!(harness.fail("apply_auto_edit_plan", json!({ "plan": plan, "expectedPlanHash": hash, "expectedRevision": 2, "confirm": true })), "AUTO_EDIT_PLAN_STALE_REVISION");
}

#[test]
fn library_tools_report_settings_and_workspaces() {
    let harness = harness();
    let settings = harness.ok("get_project_library_settings", json!({}));
    assert!(settings["settings"]["dataDirectory"].as_str().unwrap().starts_with(harness.home.to_str().unwrap()));
    harness.ok("get_timeline", json!({}));
    assert_eq!(harness.ok("get_project_io_status", json!({}))["ok"], true);
    assert!(harness.ok("list_recent_project_workspaces", json!({}))["recent"]["entries"].is_array());
}

#[test]
fn the_committed_capability_manifest_and_schemas_match_the_catalog() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (relative, expected) in avs_mcp::contract::generate() {
        let actual = std::fs::read_to_string(root.join(&relative)).unwrap_or_default();
        assert!(actual == expected, "{relative} is stale; run `agent-video-studio-mcp --write-contract .` from the repository root");
    }
    let leaves = std::fs::read_dir(root.join("schemas/v1/leaves")).unwrap().count();
    assert_eq!(leaves, avs_mcp::contract::generate().len() - 1, "stale schema files remain");
}
