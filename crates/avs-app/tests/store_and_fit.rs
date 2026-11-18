//! The editor's session link and the transform bootstrap, against real files.

use avs_app::store::{Reload, Store};
use avs_core::ops::{AddCaptions, CaptionInput, SetCaptionText};
use avs_core::{Operation, secs};
use avs_session::Session;
use std::path::PathBuf;

fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("avs-app-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn caption(text: &str, at: f64) -> Operation {
    Operation::AddCaptions(AddCaptions {
        track_id: None,
        captions: vec![CaptionInput { id: None, track_id: None, text: text.into(), start_seconds: secs(at), duration_seconds: Some(secs(1.0)), end_seconds: None, speaker: None, language: None, properties: None }],
    })
}

#[test]
fn another_writer_is_noticed_and_a_stale_edit_is_retried() {
    let root = temp_root("store");
    let env = avs_app::sandbox_env(&root).unwrap();
    let mut store = Store::open(env.clone());
    assert!(store.load_error.is_none(), "{:?}", store.load_error);
    let first = store.revision();

    // Another writer (an agent through MCP) edits the same file.
    let other = Session::for_env(&env).unwrap();
    other.apply(&caption("agent", 0.0), None).unwrap();

    // The editor has not polled yet; its edit carries the old revision and is retried.
    let applied = store.apply(&[caption("editor", 2.0)]).unwrap();
    assert_eq!(applied.revision, first + 2);
    let project = store.project().unwrap();
    let texts: Vec<String> = project.clips().filter_map(|clip| clip.caption.as_ref().map(|c| c.text.clone())).collect();
    assert_eq!(texts, vec!["agent".to_string(), "editor".to_string()]);
    assert!(store.is_own(applied.revision));
    assert!(!store.is_own(first + 1));

    // A later external edit is picked up by polling and reported as external.
    other.apply(&Operation::SetCaptionText(SetCaptionText { clip_id: "caption-1".into(), text: "changed".into() }), None).unwrap();
    assert_eq!(store.poll(true), Reload::Reloaded { external: true });
    assert_eq!(store.project().unwrap().clip("caption-1").unwrap().caption.as_ref().unwrap().text, "changed");

    // Undo goes through the session too.
    let undone = store.undo().unwrap();
    assert!(undone.changed);
    assert_eq!(store.project().unwrap().clip("caption-1").unwrap().caption.as_ref().unwrap().text, "agent");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_sandbox_never_points_outside_its_folder() {
    let root = temp_root("sandbox");
    let env = avs_app::sandbox_env(&root).unwrap();
    let settings = avs_session::get_settings(&env).unwrap();
    let root = root.canonicalize().unwrap();
    assert!(settings.data_directory.starts_with(&root), "{}", settings.data_directory.display());
    assert!(env.settings_file().starts_with(&root));
    assert!(env.default_data_directory().starts_with(&root));
    // A copy of the sandbox is repointed at the copy.
    let copy = root.with_file_name(format!("{}-copy", root.file_name().unwrap().to_string_lossy()));
    let _ = std::fs::remove_dir_all(&copy);
    let status = std::process::Command::new("/bin/cp").arg("-R").arg(&root).arg(&copy).status().unwrap();
    assert!(status.success());
    let copied = avs_app::sandbox_env(&copy).unwrap();
    assert!(avs_session::get_settings(&copied).unwrap().data_directory.starts_with(copy.canonicalize().unwrap()));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&copy);
}

/// The scale written when a transform is first edited reproduces the auto-fit
/// picture (checked by rendering both with the real renderer).
#[test]
fn bootstrap_scale_matches_the_auto_fit_render() {
    if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg not found; skipped");
        return;
    }
    let root = temp_root("fit");
    let source = root.join("wide.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=800x600:rate=30:duration=2", "-pix_fmt", "yuv420p"])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());
    let env = avs_app::sandbox_env(&root).unwrap();
    let session = Session::for_env(&env).unwrap();
    session.ensure(None).unwrap();
    let imported = avs_session::import_media(&env, &session, &avs_session::ImportRequest { source_path: source.display().to_string(), ..Default::default() }, None).unwrap();
    let state = session.load().unwrap();
    let media = state.project.media(&imported.media_id).unwrap().clone();
    let operations = avs_app::edit::place_media(&state.project, &media, avs_app::edit::PlaceMode::At, "V1", secs(0.0)).unwrap();
    let applied = session.apply_many(&operations, None).unwrap();
    let auto = applied.project.clone();
    let clip_id = auto.tracks[0].clips[0].id.clone();
    let (width, height) = avs_app::edit::media_dimensions(&media).unwrap();
    let scale = avs_render::auto_fit_scale(f64::from(width), f64::from(height), 1920.0, 1080.0, true);
    assert!((scale - 2.4).abs() < 1e-9, "800x600 covering 1920x1080 is max(1920/800, 1080/600) = 2.4, got {scale}");
    let explicit = avs_core::apply(
        &auto,
        &Operation::SetClipProperties(avs_core::ops::SetClipProperties {
            clip_id,
            properties: avs_core::ClipProperties { position: Some(avs_core::Position { x: 960.0, y: 540.0 }), scale: Some(scale), ..Default::default() },
        }),
    )
    .unwrap()
    .0;
    let media_root = session.paths().media_root();
    let resolver = move |source: &str| avs_session::resolve_media_source(&media_root, source).map_err(|error| error.message);
    let a = avs_render::render_frame(&auto, &resolver, secs(0.5), 320, 180).unwrap();
    let b = avs_render::render_frame(&explicit, &resolver, secs(0.5), 320, 180).unwrap();
    let differing = a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0.iter()).filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 40)).count();
    let share = differing as f64 / (320.0 * 180.0);
    assert!(share < 0.03, "{:.1}% of pixels differ", share * 100.0);
    let _ = std::fs::remove_dir_all(&root);
}

/// An agent's `switch_project_library` changes the settings, not the editor: the
/// store reports the new active library once, and nothing while it is unchanged.
#[test]
fn a_library_switched_by_another_process_is_noticed_once() {
    let root = temp_root("switch");
    let env = avs_app::sandbox_env(&root).unwrap();
    let mut store = Store::open(env.clone());
    assert!(store.load_error.is_none(), "{:?}", store.load_error);
    assert_eq!(store.library_switched_elsewhere(), None);

    // What the MCP server does for create_project_session / switch_project_library.
    let target = root.join("second-library");
    let input = avs_session::CreateInput { target_path: target.display().to_string(), title: Some("Second".into()), expected_settings_revision: None };
    let created = avs_session::create_workspace(&env, &input).unwrap();

    let active = store.library_switched_elsewhere().expect("the switch is noticed");
    assert_eq!(active, created.settings.data_directory);
    assert_ne!(active, store.data_directory());
    store.reopen();
    assert_eq!(store.data_directory(), active);
    assert_eq!(store.library_switched_elsewhere(), None);
    let _ = std::fs::remove_dir_all(&root);
}
