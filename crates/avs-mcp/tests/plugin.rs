//! The plugin's routing files agree with each other and with the server.

use avs_mcp::{Server, ToolHost};
use avs_session::Env;
use serde_json::Value;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(relative: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(root().join(relative)).unwrap_or_else(|error| panic!("{relative}: {error}"))).unwrap_or_else(|error| panic!("{relative}: {error}"))
}

#[test]
fn mcp_launch_definition_starts_the_installed_server_for_both_hosts() {
    let mcp = read_json(".mcp.json");
    let server = &mcp["mcpServers"]["agent_video_studio"];
    assert_eq!(server["command"], "/bin/sh");
    let script = server["args"][1].as_str().unwrap();
    assert_eq!(script, "exec \"$HOME/.local/bin/agent-video-studio-mcp\"");
    // The launch definition must not depend on this checkout, a build directory or a
    // host-specific variable.
    for forbidden in ["cargo", "target/", "CLAUDE_PLUGIN_ROOT", "/Users/"] {
        assert!(!mcp.to_string().contains(forbidden), ".mcp.json must not mention {forbidden}");
    }
    let install = std::fs::read_to_string(root().join("scripts/install.sh")).unwrap();
    assert!(install.contains("$bin_dir/agent-video-studio-mcp"), "install.sh installs the binary .mcp.json starts");
    assert!(install.contains("--package avs-mcp") && install.contains("--package avs-app"));
}

#[test]
fn plugin_metadata_is_consistent() {
    let codex = read_json(".codex-plugin/plugin.json");
    assert_eq!(codex["name"], "agent-video-studio");
    // Fixed compatibility value for local plugins; not a product version.
    assert_eq!(codex["version"], "0.1.0");
    assert_eq!(codex["mcpServers"], "./.mcp.json");
    assert_eq!(codex["skills"], "./skills/");
    assert!(root().join("skills/agent-video-studio/SKILL.md").is_file());
    assert!(!root().join(".codex-plugin/refresh.json").exists(), "nothing has to run after a Codex cache refresh");

    let marketplace = read_json(".claude-plugin/marketplace.json");
    let entry = &marketplace["plugins"][0];
    assert_eq!(entry["name"], "agent-video-studio");
    assert_eq!(entry["source"], "./");
    assert!(entry.get("version").is_none(), "a local marketplace entry carries no version");
    assert!(!root().join(".claude-plugin/plugin.json").exists());

    for removed in ["package.json", "vite.config.ts", "src", "tsconfig.json"] {
        assert!(!root().join(removed).exists(), "{removed} belongs to the removed TypeScript implementation");
    }
}

#[test]
fn the_readme_lists_every_tool_and_the_routing_metadata_names_only_real_ones() {
    let home = tempfile::tempdir().unwrap();
    let server = Server::new(Env::new(home.path()));
    let tools: Vec<String> = server.tools().iter().map(|tool| tool["name"].as_str().unwrap().to_string()).collect();

    let readme = std::fs::read_to_string(root().join("README.md")).unwrap();
    assert!(readme.contains(&format!("{} tools over stdio", tools.len())), "README tool count is stale ({} tools)", tools.len());
    for tool in &tools {
        assert!(readme.contains(&format!("`{tool}`")), "README does not list `{tool}`");
    }

    // Routing keywords an agent searches by must be real tool names when they look like one.
    let codex = read_json(".codex-plugin/plugin.json");
    for keyword in codex["keywords"].as_array().unwrap() {
        let keyword = keyword.as_str().unwrap();
        if keyword.contains('_') {
            assert!(tools.iter().any(|tool| tool == keyword), "plugin.json keyword `{keyword}` is not a tool");
        }
    }
    let skill = std::fs::read_to_string(root().join("skills/agent-video-studio/SKILL.md")).unwrap();
    for name in ["import_handoff_manifest", "import_codexvideo_job", "plan_silence_removal", "inspect_auto_edit_plan", "apply_auto_edit_plan", "validate_timeline", "render_preview", "render_final_export", "start_render_final_export", "import_media_file", "get_timeline"] {
        assert!(skill.contains(name), "SKILL.md no longer mentions {name}");
        assert!(tools.iter().any(|tool| tool == name), "SKILL.md names {name}, which is not a tool");
    }
    assert!(!skill.contains("npm") && !readme.contains("npm run"), "the documents still describe the removed Node tooling");
}
