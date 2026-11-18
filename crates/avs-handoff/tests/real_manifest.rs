//! Imports a real CodexVideo job into a temp library. Run by hand:
//!
//! ```text
//! AVS_REAL_HANDOFF_MANIFEST=/path/to/codexvideo-handoff.json \
//!   cargo test -p avs-handoff --test real_manifest -- --ignored --nocapture
//! ```
//!
//! The manifest and its media are only read; the copies go into a temp directory.

use avs_handoff::{ImportOptions, import_handoff};
use avs_session::{Env, Session};
use std::path::PathBuf;

fn find_tool(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(name)).find(|p| p.is_file())
}

#[test]
#[ignore = "imports a real job (copies hundreds of MB); run by hand"]
fn a_real_job_imports_into_a_temp_library() {
    let manifest = PathBuf::from(std::env::var_os("AVS_REAL_HANDOFF_MANIFEST").expect("set AVS_REAL_HANDOFF_MANIFEST"));
    let dir = tempfile::Builder::new().prefix("avs-real-handoff-").tempdir().unwrap();
    let mut env = Env::new(dir.path().join("home"));
    env.ffprobe_path = Some(find_tool("ffprobe").expect("ffprobe on PATH"));
    let session = Session::in_directory(dir.path().join("library"));
    session.create(None).unwrap();

    let started = std::time::Instant::now();
    let report = import_handoff(&env, &session, &manifest, &ImportOptions::default()).unwrap_or_else(|e| panic!("{e} {}", e.details));
    let elapsed = started.elapsed();

    let project = session.load().unwrap().project;
    assert!(avs_core::validate(&project).ok);
    println!("elapsed: {elapsed:?}");
    println!("revision {} undoDepth {}", report.revision, report.undo_depth);
    println!("title {:?} fps {} resolution {}x{}", project.title, project.fps, project.resolution.width, project.resolution.height);
    println!("duration {}s", project.duration().secs());
    println!(
        "media {} tracks {:?} clips {} (captions {}) linked {}",
        project.media.len(),
        project.tracks.iter().map(|t| format!("{}:{}", t.id, t.clips.len())).collect::<Vec<_>>(),
        report.timeline.clip_count,
        report.timeline.caption_count,
        project.clips().filter(|c| c.link.is_some()).count()
    );
    println!("warnings: {:?}", report.warnings.iter().map(|w| &w.code).collect::<Vec<_>>());
    println!("unresolved: {} importComplete: {}", report.unresolved.len(), report.validation.import_complete);
    println!("jobRoot: {:?}", project.metadata.as_ref().and_then(|m| m["codexVideo"].get("jobRoot")));
    println!("{}", serde_json::to_string(&report.created).unwrap());
    session.undo(None).unwrap();
}
