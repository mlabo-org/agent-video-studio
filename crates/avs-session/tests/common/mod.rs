#![allow(dead_code)]

use avs_core::Operation;
use avs_session::Env;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// A temp directory with an `Env` whose home and settings live inside it.
pub struct Sandbox {
    pub dir: TempDir,
    pub env: Env,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let dir = tempfile::Builder::new().prefix("avs-").tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut env = Env::new(&home);
        env.settings_path = Some(dir.path().join("app-config").join("project-library-settings.json"));
        Sandbox { dir, env }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

pub fn op(tool: &str, args: Value) -> Operation {
    serde_json::from_value(json!({ "tool": tool, "args": args })).unwrap_or_else(|e| panic!("{tool}: {e}"))
}

pub fn add_track(id: &str) -> Operation {
    op("add_track", json!({ "id": id, "kind": "video" }))
}

pub fn write(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
