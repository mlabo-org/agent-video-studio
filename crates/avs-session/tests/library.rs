//! Settings, workspaces, media import, folder listing and export job state.
//! Everything runs inside a temp directory; nothing reads the real user data.

mod common;

use avs_session::workspace::{ConfirmedInput, CreateInput, SwitchInput};
use avs_session::*;
use common::{Sandbox, add_track, op};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn create(sandbox: &Sandbox, folder: &str, title: &str) -> workspace::CreateResult {
    let input = CreateInput { target_path: sandbox.path(folder).display().to_string(), title: Some(title.into()), expected_settings_revision: None };
    create_workspace(&sandbox.env, &input).unwrap_or_else(|e| panic!("create {folder}: {e} {}", e.details))
}

fn confirmed(path: &Path) -> ConfirmedInput {
    ConfirmedInput { target_path: path.display().to_string(), confirm_data_directory: path.display().to_string(), expected_recent_revision: None }
}

// ---------------------------------------------------------------- settings

#[test]
fn reading_settings_creates_nothing_and_a_broken_file_is_repaired_by_reset() {
    let sandbox = Sandbox::new();
    let settings = get_settings(&sandbox.env).unwrap();
    assert_eq!(settings.source, config::SettingsSource::Default);
    assert!(settings.data_directory.ends_with("Agent Video Studio/Project Library"));
    assert!(recent_list(&sandbox.env).unwrap().entries.is_empty());
    assert!(!sandbox.path("app-config").exists() && std::fs::read_dir(sandbox.path("home")).unwrap().next().is_none());

    std::fs::create_dir_all(sandbox.path("app-config")).unwrap();
    std::fs::write(sandbox.env.settings_file(), b"{ broken").unwrap();
    let error = get_settings(&sandbox.env).unwrap_err();
    assert_eq!(error.code, "SETTINGS_INVALID_JSON");
    assert!(set_settings(&sandbox.env, &sandbox.path("x").display().to_string(), None).is_err(), "set does not paper over a broken file");

    let repaired = reset_settings(&sandbox.env, Some(99)).unwrap();
    assert_eq!(repaired.source, config::SettingsSource::Default);
    assert!(!sandbox.env.settings_file().exists());

    let set = set_settings(&sandbox.env, &sandbox.path("chosen").display().to_string(), Some(0)).unwrap();
    assert_eq!((set.revision, set.source), (1, config::SettingsSource::Config));
    assert!(set.data_directory.ends_with("chosen/Agent Video Studio/Project Library") && set.data_directory.is_dir());
    assert_eq!(set_settings(&sandbox.env, &sandbox.path("y").display().to_string(), Some(0)).unwrap_err().code, "SETTINGS_REVISION_MISMATCH");
}

#[test]
fn library_root_and_session_path_overrides_are_respected() {
    let sandbox = Sandbox::new();
    let mut env = sandbox.env.clone();
    env.library_root = Some(sandbox.path("exact").display().to_string());
    let resolved = resolve_library(&env).unwrap();
    assert_eq!((resolved.data_directory, resolved.source), (sandbox.path("exact"), config::SettingsSource::Env));

    env.session_path = Some(sandbox.path("single/file.json"));
    let session = Session::for_env(&env).unwrap();
    assert_eq!(session.paths().session_file, sandbox.path("single/file.json"));
    assert_eq!(session.paths().lock_file(), sandbox.path("single/.file.lock"));
    assert_eq!(set_settings(&env, "/tmp/never", None).unwrap_err().kind, ErrorKind::Blocked);
}

// ---------------------------------------------------------------- workspaces

#[test]
fn workspaces_can_be_created_switched_deleted_and_restored() {
    let sandbox = Sandbox::new();
    let a = create(&sandbox, "a", "Alpha");
    let b = create(&sandbox, "b", "Beta");
    assert_ne!(a.created.id, b.created.id);
    let current = get_settings(&sandbox.env).unwrap();
    assert_eq!(current.data_directory, sandbox.path("b/Agent Video Studio/Project Library"));
    let a_dir = sandbox.path("a/Agent Video Studio/Project Library");
    let b_dir = sandbox.path("b/Agent Video Studio/Project Library");
    let recent = recent_list(&sandbox.env).unwrap();
    assert_eq!(recent.entries.iter().map(|e| e.data_directory.clone()).collect::<Vec<_>>(), vec![b_dir.display().to_string(), a_dir.display().to_string()]);
    assert!(recent.entries[0].current && recent.entries[0].reachable);

    // The workspace in use cannot be deleted.
    let plan = inspect_delete(&sandbox.env, &b_dir.display().to_string());
    assert!(plan.blockers.iter().any(|b| b.code == "PROJECT_IO_DELETE_CURRENT_WORKSPACE"));
    assert_eq!(delete_workspace(&sandbox.env, &confirmed(&b_dir)).unwrap_err().code, "PROJECT_IO_DELETE_CURRENT_WORKSPACE");

    // A wrong confirmation path changes nothing.
    let mut wrong = confirmed(&a_dir);
    wrong.confirm_data_directory = sandbox.path("elsewhere").display().to_string();
    assert_eq!(delete_workspace(&sandbox.env, &wrong).unwrap_err().code, "PROJECT_IO_DELETE_CONFIRMATION_MISMATCH");
    assert!(a_dir.join("project-session.json").is_file());

    let deleted = delete_workspace(&sandbox.env, &confirmed(&a_dir)).unwrap();
    assert!(!a_dir.exists() && deleted.moved_to.ends_with("Project Library.deleted") && deleted.moved_to.join("project-session.json").is_file());
    assert_eq!(deleted.summary.title, "Alpha");
    assert!(deleted.recent.entries.iter().all(|e| e.data_directory != a_dir.display().to_string()));

    let restore_plan = inspect_restore(&sandbox.env, &a_dir.display().to_string());
    assert!(restore_plan.blockers.is_empty(), "{:?}", restore_plan.blockers);
    let restored = restore_workspace(&sandbox.env, &confirmed(&a_dir)).unwrap();
    assert_eq!(restored.summary.id, a.created.id);
    assert!(a_dir.join("project-session.json").is_file() && !deleted.moved_to.exists());
    assert_eq!(get_settings(&sandbox.env).unwrap().data_directory, b_dir, "restore does not switch");

    let switched = switch_workspace(&sandbox.env, &SwitchInput { target_path: a_dir.display().to_string(), expected_settings_revision: Some(current.revision) }).unwrap();
    assert_eq!(switched.opened.title, "Alpha");
    assert_eq!(Session::for_env(&sandbox.env).unwrap().load().unwrap().project.title, "Alpha");
    assert_eq!(create_workspace(&sandbox.env, &CreateInput { target_path: a_dir.display().to_string(), title: None, expected_settings_revision: None }).unwrap_err().code, "PROJECT_IO_SESSION_EXISTS");
}

#[test]
fn delete_refuses_symbolic_links_and_a_forgotten_workspace_can_be_listed_as_missing() {
    let sandbox = Sandbox::new();
    create(&sandbox, "real", "Real");
    create(&sandbox, "current", "Current");
    let real_dir = sandbox.path("real/Agent Video Studio/Project Library");
    let link_parent = sandbox.path("linked/Agent Video Studio");
    std::fs::create_dir_all(&link_parent).unwrap();
    std::os::unix::fs::symlink(&real_dir, link_parent.join("Project Library")).unwrap();
    let link_dir = link_parent.join("Project Library");
    let plan = inspect_delete(&sandbox.env, &link_dir.display().to_string());
    assert!(plan.blockers.iter().any(|b| b.code == "PROJECT_IO_DATA_DIRECTORY_SYMLINK"), "{:?}", plan.blockers);
    assert!(delete_workspace(&sandbox.env, &confirmed(&link_dir)).is_err());
    assert!(real_dir.join("project-session.json").is_file());

    std::fs::remove_dir_all(&real_dir).unwrap();
    let recent = recent_list(&sandbox.env).unwrap();
    let missing = recent.entries.iter().find(|e| e.data_directory == real_dir.display().to_string()).unwrap();
    assert_eq!(missing.reachability, config::Reachability::DataDirectoryMissing);
    let forgotten = recent_forget(&sandbox.env, &real_dir.display().to_string(), Some(recent.revision)).unwrap();
    assert!(forgotten.forgotten && forgotten.list.entries.len() == 1);
    assert_eq!(recent_forget(&sandbox.env, &real_dir.display().to_string(), Some(recent.revision)).unwrap_err().code, "RECENT_REVISION_MISMATCH");
}

#[test]
fn a_failed_recent_list_update_puts_a_deleted_workspace_back() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new();
    create(&sandbox, "a", "Alpha");
    create(&sandbox, "b", "Beta");
    let a_dir = sandbox.path("a/Agent Video Studio/Project Library");
    let config_dir = sandbox.path("app-config");
    std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let enforced = std::fs::write(config_dir.join(".probe"), b"").is_err();
    let result = enforced.then(|| delete_workspace(&sandbox.env, &confirmed(&a_dir)));
    std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let Some(result) = result else {
        eprintln!("SKIPPED: directory permissions are not enforced here");
        return;
    };
    assert_eq!(result.unwrap_err().kind, ErrorKind::Io);
    assert!(a_dir.join("project-session.json").is_file());
    assert!(!sandbox.path("a/Agent Video Studio/Project Library.deleted").exists());
    assert!(recent_list(&sandbox.env).unwrap().entries.iter().any(|e| e.data_directory == a_dir.display().to_string()));
}

// ---------------------------------------------------------------- media

fn find_tool(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(name)).find(|p| p.is_file())
}

fn small_video(dir: &Path) -> Option<(Env, PathBuf)> {
    let ffmpeg = find_tool("ffmpeg")?;
    let ffprobe = find_tool("ffprobe")?;
    let file = dir.join("clip.mp4");
    let status = std::process::Command::new(ffmpeg)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=1:size=320x240:rate=10", "-f", "lavfi", "-i", "sine=duration=1", "-shortest", "-pix_fmt", "yuv420p"])
        .arg(&file)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let mut env = Env::new(dir.join("home"));
    env.ffprobe_path = Some(ffprobe);
    Some((env, file))
}

#[test]
fn importing_a_video_copies_probes_registers_and_cleans_up_on_failure() {
    let sandbox = Sandbox::new();
    let Some((env, file)) = small_video(sandbox.dir.path()) else {
        eprintln!("SKIPPED: ffmpeg/ffprobe not found on PATH");
        return;
    };
    let session = Session::in_directory(sandbox.path("library"));
    session.create(None).unwrap();
    let request = ImportRequest { source_path: file.display().to_string(), ..ImportRequest::default() };

    let first = import_media(&env, &session, &request, Some(1)).unwrap();
    assert!(first.source.starts_with("/project-library/media/imports/") && first.source.ends_with("/clip.mp4"), "{}", first.source);
    let probe = first.probe.as_ref().unwrap();
    assert!(probe.has_video && probe.has_audio);
    assert_eq!((probe.display_width, probe.display_height), (Some(320), Some(240)));
    let state = session.load().unwrap();
    let media = state.project.media(&first.media_id).unwrap();
    assert!((media.duration_seconds.secs() - 1.0).abs() < 0.2);
    assert_eq!(media.metadata.as_ref().unwrap()["mediaProbe"]["audio"]["hasAudio"], true);
    assert_eq!(std::fs::read(session.resolve_media_source(&first.source).unwrap()).unwrap(), std::fs::read(&file).unwrap());
    assert_eq!(state.revision, 2);

    // Same name again: a sequence number, never an overwrite.
    let second = import_media(&env, &session, &request, None).unwrap();
    assert!(second.source.ends_with("/clip-2.mp4"), "{}", second.source);

    // A stale revision is rejected and the new copy is removed again.
    let imports = session.paths().media_root().join("imports");
    let count = |dir: &Path| walk(dir);
    let before = count(&imports);
    let error = import_media(&env, &session, &request, Some(1)).unwrap_err();
    assert_eq!(error.code, "REVISION_MISMATCH");
    assert_eq!(count(&imports), before);
}

fn walk(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |reader| {
        reader.flatten().map(|e| if e.path().is_dir() { walk(&e.path()) } else { 1 }).sum()
    })
}

#[test]
fn a_hung_ffprobe_is_killed_at_the_timeout() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new();
    let fake = sandbox.path("ffprobe");
    std::fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let source = sandbox.path("song.wav");
    std::fs::write(&source, b"not really audio").unwrap();
    let mut env = Env::new(sandbox.path("home"));
    env.ffprobe_path = Some(fake);
    let session = Session::in_directory(sandbox.path("library"));
    session.create(None).unwrap();

    let started = std::time::Instant::now();
    let request = ImportRequest { source_path: source.display().to_string(), probe_timeout_ms: Some(300), ..ImportRequest::default() };
    let error = import_media(&env, &session, &request, None).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
    assert_eq!(error.code, "IMPORT_MEDIA_DURATION_UNPROBEABLE");
    assert_eq!(error.details["attempts"][0]["timedOut"], true);
    assert_eq!(walk(&session.paths().media_root()), 0, "nothing was copied");

    // With a duration given, an unusable probe does not block the import.
    let request = ImportRequest { duration_seconds: Some(2.5), ..request };
    let imported = import_media(&env, &session, &request, None).unwrap();
    assert!(imported.probe.is_none());
    assert_eq!(session.load().unwrap().project.media(&imported.media_id).unwrap().duration_seconds.secs(), 2.5);
}

#[test]
fn import_and_media_source_paths_are_checked() {
    let sandbox = Sandbox::new();
    let env = Env::new(sandbox.path("home"));
    let session = Session::in_directory(sandbox.path("library"));
    session.create(None).unwrap();
    let try_import = |path: &str| {
        let request = ImportRequest { source_path: path.into(), duration_seconds: Some(1.0), ..ImportRequest::default() };
        import_media(&env, &session, &request, None).unwrap_err().code
    };
    let real = sandbox.path("a.mp4");
    std::fs::write(&real, b"x").unwrap();
    let link = sandbox.path("link.mp4");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    std::fs::write(sandbox.path("notes.txt"), b"x").unwrap();
    assert_eq!(try_import("relative/a.mp4"), "IMPORT_MEDIA_SOURCE_NOT_ABSOLUTE");
    assert_eq!(try_import("https://example.com/a.mp4"), "IMPORT_MEDIA_SOURCE_URL");
    assert_eq!(try_import(&link.display().to_string()), "IMPORT_MEDIA_SOURCE_SYMLINK");
    assert_eq!(try_import(&sandbox.path("notes.txt").display().to_string()), "IMPORT_MEDIA_UNSUPPORTED_EXTENSION");
    assert_eq!(try_import(&sandbox.path("missing.mp4").display().to_string()), "IMPORT_MEDIA_SOURCE_MISSING");
    assert_eq!(try_import(&sandbox.path("home").display().to_string()), "IMPORT_MEDIA_SOURCE_DIRECTORY");

    let media = session.paths().media_root();
    std::fs::create_dir_all(media.join("imports")).unwrap();
    std::fs::write(media.join("imports/ok.mp4"), b"x").unwrap();
    std::fs::write(sandbox.path("outside.mp4"), b"x").unwrap();
    std::os::unix::fs::symlink(sandbox.path("outside.mp4"), media.join("imports/out.mp4")).unwrap();
    std::os::unix::fs::symlink(sandbox.dir.path(), media.join("jump")).unwrap();
    let resolve = |source: &str| session.resolve_media_source(source);
    assert_eq!(resolve("/project-library/media/imports/ok.mp4").unwrap(), media.join("imports/ok.mp4"));
    for bad in [
        "/project-library/media/../outside.mp4",
        "/project-library/media/imports/../imports/ok.mp4",
        "/project-library/media//imports/ok.mp4",
        "/project-library/media/imports/ok.mp4/",
        "/project-library/media/",
        "/other/media/imports/ok.mp4",
        "/project-library/media/imports\\ok.mp4",
        "imports/ok.mp4",
    ] {
        assert_eq!(resolve(bad).unwrap_err().code, "MEDIA_SOURCE_INVALID", "{bad}");
    }
    assert_eq!(resolve("/project-library/media/imports/out.mp4").unwrap_err().code, "MEDIA_SOURCE_SYMLINK");
    assert_eq!(resolve("/project-library/media/jump/outside.mp4").unwrap_err().code, "MEDIA_SOURCE_SYMLINK");
    assert_eq!(resolve("/project-library/media/imports/none.mp4").unwrap_err().code, "MEDIA_SOURCE_NOT_FOUND");
}

// ---------------------------------------------------------------- folders and jobs

#[test]
fn folder_listing_hides_dot_folders_and_reports_unreadable_ones_in_the_result() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(sandbox.path("home/Beta")).unwrap();
    std::fs::create_dir_all(sandbox.path("home/alpha")).unwrap();
    std::fs::create_dir_all(sandbox.path("home/.hidden")).unwrap();
    std::fs::write(sandbox.path("home/file.txt"), b"x").unwrap();
    let listing = browse_directories(&sandbox.env, None, BrowseMode::Filesystem).unwrap();
    assert_eq!(listing.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["alpha", "Beta"]);
    let missing = browse_directories(&sandbox.env, Some(&sandbox.path("nope").display().to_string()), BrowseMode::Filesystem).unwrap();
    assert_eq!(missing.error.unwrap().code, "NOT_FOUND");
    assert!(browse_directories(&sandbox.env, Some("relative"), BrowseMode::ProjectLibrary).is_err());
}

#[test]
fn export_job_state_survives_concurrent_progress_and_cancel_and_finds_dead_processes() {
    let sandbox = Sandbox::new();
    let jobs = ExportJobs::new(sandbox.path("library/.export-jobs"));
    let job = jobs.create(&NewExportJob { output_path: Some("/tmp/out.mp4".into()), ..NewExportJob::default() }).unwrap();
    assert_eq!(job.state, JobState::Starting);
    assert_eq!(jobs.create(&NewExportJob { job_id: Some("../evil".into()), ..NewExportJob::default() }).unwrap_err().code, "JOB_ID_INVALID");

    jobs.mark_running(&job.job_id, std::process::id()).unwrap();
    jobs.request_cancel(&job.job_id).unwrap();
    let after = jobs.report_progress(&job.job_id, 0.4, Some("encoding")).unwrap();
    assert_eq!((after.state, after.cancel_requested, after.progress), (JobState::Cancelling, true, 0.4), "progress must not undo a cancel request");
    assert_eq!(jobs.get(&job.job_id).unwrap().process_alive, Some(true));
    jobs.finish(&job.job_id, &JobResult::Cancelled).unwrap();
    assert_eq!(jobs.finish(&job.job_id, &JobResult::Completed { output_path: None }).unwrap_err().code, "JOB_FINISHED");
    assert_eq!(jobs.request_cancel(&job.job_id).unwrap().state, JobState::Cancelled);

    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = child.id();
    child.wait().unwrap();
    let orphan = jobs.create(&NewExportJob::default()).unwrap();
    jobs.mark_running(&orphan.job_id, dead_pid).unwrap();
    assert!(jobs.get(&orphan.job_id).unwrap().stale);
    let reaped = jobs.reap_stale().unwrap();
    assert_eq!((reaped.len(), reaped[0].state), (1, JobState::Failed));
    assert_eq!(jobs.list().unwrap().len(), 2);
}

#[test]
fn operations_reach_the_disk_only_through_apply() {
    // A smoke check that the workspace session accepts a core operation end to end.
    let sandbox = Sandbox::new();
    create(&sandbox, "w", "W");
    let session = Session::for_env(&sandbox.env).unwrap();
    session.apply(&add_track("T1"), Some(1)).unwrap();
    session.apply(&op("add_track", json!({ "kind": "caption" })), Some(2)).unwrap();
    assert_eq!(session.load().unwrap().revision, 3);
}
