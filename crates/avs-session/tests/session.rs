//! Behaviour of the session file: revisions, atomic multi-edit, undo, limits, old files.

mod common;

use avs_session::{ACTIVITY_LIMIT, ErrorKind, HISTORY_LIMIT, Session};
use common::{Sandbox, add_track, op, write};
use serde_json::{Value, json};

fn session_in(sandbox: &Sandbox) -> Session {
    let session = Session::in_directory(sandbox.path("library"));
    session.create(Some("Test")).unwrap();
    session
}

#[test]
fn only_one_of_two_concurrent_writers_with_the_same_revision_wins() {
    for round in 0..5 {
        let sandbox = Sandbox::new();
        let session = session_in(&sandbox);
        let barrier = std::sync::Barrier::new(2);
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|n| {
                    let session = session.clone();
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        session.apply(&add_track(&format!("T{n}")), Some(1))
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let ok = results.iter().filter(|r| r.is_ok()).count();
        let mismatched = results.iter().filter(|r| matches!(r, Err(e) if e.code == "REVISION_MISMATCH")).count();
        assert_eq!((ok, mismatched), (1, 1), "round {round}: {results:?}");
        let state = session.load().unwrap();
        assert_eq!(state.revision, 2);
        assert_eq!(state.project.tracks.len(), 4);
    }
}

#[test]
fn stale_revision_is_rejected_and_changes_nothing() {
    let sandbox = Sandbox::new();
    let session = session_in(&sandbox);
    session.apply(&add_track("T1"), Some(1)).unwrap();
    let error = session.apply(&add_track("T2"), Some(1)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::RevisionMismatch);
    assert_eq!(error.details["expectedRevision"], 1);
    assert_eq!(error.details["actualRevision"], 2);
    assert_eq!(session.load().unwrap().project.tracks.len(), 4);
}

#[test]
fn apply_many_is_one_revision_one_undo_and_all_or_nothing() {
    let sandbox = Sandbox::new();
    let session = session_in(&sandbox);
    let applied = session.apply_many(&[add_track("T1"), add_track("T2")], Some(1)).unwrap();
    assert_eq!((applied.revision, applied.undo_depth, applied.outcomes.len()), (2, 1, 2));

    // The second operation fails (duplicate id): neither is stored.
    let error = session.apply_many(&[add_track("T3"), add_track("T1")], None).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Edit);
    assert_eq!(error.details["operationIndex"], 1);
    let state = session.load().unwrap();
    assert_eq!((state.revision, state.project.tracks.len()), (2, 5));

    let undone = session.undo(None).unwrap();
    assert!(undone.changed);
    assert_eq!(undone.project.tracks.len(), 3, "both tracks come off in one undo");
    let redone = session.redo(None).unwrap();
    assert_eq!(redone.project.tracks.len(), 5);
    assert!(!session.redo(None).unwrap().changed);
    assert_eq!(session.load().unwrap().revision, 4, "a no-op redo writes nothing");
}

#[test]
fn history_and_activity_are_capped_and_the_log_is_not_copied_into_snapshots() {
    let sandbox = Sandbox::new();
    let session = session_in(&sandbox);
    for n in 0..HISTORY_LIMIT + 5 {
        session.apply(&add_track(&format!("T{n}")), None).unwrap();
    }
    assert_eq!(session.load().unwrap().undo_depth, HISTORY_LIMIT);

    let many: Vec<_> = (0..ACTIVITY_LIMIT + 20).map(|n| add_track(&format!("B{n}"))).collect();
    session.apply_many(&many, None).unwrap();
    let state = session.load().unwrap();
    assert_eq!(state.activity.len(), ACTIVITY_LIMIT);
    assert_eq!(state.activity.last().unwrap().tool, "add_track");
    assert!(state.activity.iter().all(|entry| entry.ok));

    let raw: Value = serde_json::from_slice(&std::fs::read(&session.paths().session_file).unwrap()).unwrap();
    assert_eq!(raw["schemaVersion"], 2);
    assert!(raw["libraryId"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(raw["history"]["past"][0].get("toolHistory").is_none() && raw["history"]["past"][0].get("activity").is_none());
}

#[test]
fn every_created_project_has_its_own_ids_and_load_never_creates_files() {
    let sandbox = Sandbox::new();
    let missing = Session::in_directory(sandbox.path("nowhere"));
    assert_eq!(missing.load().unwrap_err().code, "SESSION_NOT_FOUND");
    assert!(!sandbox.path("nowhere").exists());
    assert_eq!(missing.apply(&add_track("T1"), None).unwrap_err().code, "SESSION_NOT_FOUND");
    assert_eq!(missing.undo(None).unwrap_err().code, "SESSION_NOT_FOUND");
    assert!(!sandbox.path("nowhere").exists(), "a failed write on a missing library creates no folder or lock file");

    let a = Session::in_directory(sandbox.path("a")).create(None).unwrap();
    let b = Session::in_directory(sandbox.path("b")).create(None).unwrap();
    assert_ne!(a.project.id, b.project.id);
    assert_ne!(a.library_id, b.library_id);
    assert!(Session::in_directory(sandbox.path("a")).create(None).is_err_and(|e| e.code == "SESSION_EXISTS"));
}

fn legacy_clip(id: &str, track: &str, role: &str) -> Value {
    json!({
        "id": id, "trackId": track, "kind": "media", "name": "Intro", "startSeconds": 1.5, "durationSeconds": 4,
        "mediaId": "m1", "sourceStartSeconds": 0,
        "properties": { "volumeDb": -3, "metadata": { "linkedGroupId": "grp-1", "linkedRole": role, "linkedClipId": "other" } }
    })
}

fn legacy_project(clips: bool) -> Value {
    json!({
        "id": "project-empty-session", "title": "Old", "fps": 30, "resolution": { "width": 1920, "height": 1080 },
        "sampleRate": 48000, "playheadSeconds": 0,
        "media": [{ "id": "m1", "name": "intro.mp4", "type": "video", "durationSeconds": 10, "source": "/project-library/media/x.mp4" }],
        "tracks": [
            { "id": "V1", "name": "Video", "kind": "video", "clips": if clips { vec![legacy_clip("c1", "V1", "video")] } else { vec![] } },
            { "id": "A1", "name": "Audio", "kind": "audio", "clips": if clips { vec![legacy_clip("c2", "A1", "audio")] } else { vec![] } },
            { "id": "C1", "name": "Captions", "kind": "caption", "clips": [] }
        ],
        "toolHistory": [
            { "id": "t1", "toolName": "register_media", "status": "success", "summary": "Registered video media.", "createdAt": "2025-01-01T00:00:00.000Z", "input": { "big": true } },
            { "id": "t2", "toolName": "add_clips", "status": "error", "summary": "Rejected." }
        ]
    })
}

#[test]
fn an_old_v1_file_opens_keeps_good_history_drops_bad_history_and_upgrades_on_first_write() {
    let sandbox = Sandbox::new();
    let session = Session::in_directory(sandbox.path("library"));
    let mut broken = legacy_project(true);
    broken["tracks"][0]["clips"][0]["startSeconds"] = json!(-5); // fails validation
    write(
        &session.paths().session_file,
        &json!({
            "schemaVersion": 1, "revision": 7, "updatedAt": "2025-01-02T00:00:00.000Z",
            "project": legacy_project(true),
            "history": { "past": [legacy_project(false), broken, "not a project"], "future": [] }
        }),
    );

    let state = session.load().unwrap();
    assert_eq!((state.file_schema_version, state.revision, state.undo_depth), (1, 7, 1));
    assert!(state.library_id.is_none(), "an id is assigned at the first write, not on every read");
    assert_eq!(state.warnings.len(), 2);
    assert!(state.warnings.iter().all(|w| w.code == "HISTORY_ENTRY_DROPPED"));
    let video = state.project.clip("c1").unwrap();
    assert_eq!(video.link.as_ref().unwrap().group_id, "grp-1");
    assert!(video.properties.metadata.is_none(), "link bookkeeping moved out of metadata");
    assert_eq!(state.activity.len(), 2);
    assert_eq!((state.activity[0].tool.as_str(), state.activity[0].ok, state.activity[1].ok), ("register_media", true, false));
    assert!(session.load().unwrap().library_id.is_none());

    let applied = session.apply(&add_track("T9"), Some(7)).unwrap();
    assert_eq!(applied.revision, 8);
    let after = session.load().unwrap();
    assert_eq!(after.file_schema_version, 2);
    assert!(after.library_id.is_some());
    assert_eq!(after.undo_depth, 2);
    assert!(after.warnings.is_empty(), "the bad entries are gone once rewritten");
    assert_eq!(after.project.id, "project-empty-session", "old project ids are kept as they are");
}

#[test]
fn an_invalid_current_project_is_the_only_thing_that_stops_a_load() {
    let sandbox = Sandbox::new();
    let session = Session::in_directory(sandbox.path("library"));
    let mut project = legacy_project(true);
    project["tracks"][0]["clips"][0]["durationSeconds"] = json!(0);
    write(&session.paths().session_file, &json!({ "schemaVersion": 1, "revision": 1, "updatedAt": "x", "project": project }));
    let error = session.load().unwrap_err();
    assert_eq!(error.code, "SESSION_PROJECT_INVALID");
    assert_eq!(error.details["issues"][0]["code"], "INVALID_CLIP_DURATION");

    std::fs::write(&session.paths().session_file, b"{ not json").unwrap();
    assert_eq!(session.load().unwrap_err().code, "SESSION_INVALID_JSON");
    write(&session.paths().session_file, &json!({ "schemaVersion": 9, "revision": 1 }));
    assert_eq!(session.load().unwrap_err().code, "SESSION_UNSUPPORTED_SCHEMA");
}

#[test]
fn a_waiting_writer_times_out_with_a_distinct_error() {
    let sandbox = Sandbox::new();
    let session = session_in(&sandbox);
    let lock = avs_session::fsutil::Lock::acquire(&session.paths().lock_file(), std::time::Duration::from_secs(1)).unwrap();
    let waiter = session.clone().with_lock_timeout(std::time::Duration::from_millis(150));
    let error = waiter.apply(&op("add_track", json!({ "kind": "audio" })), None).unwrap_err();
    assert_eq!((error.kind, error.code.as_str()), (ErrorKind::LockTimeout, "LOCK_TIMEOUT"));
    drop(lock);
    assert!(session.apply(&add_track("T1"), None).is_ok());
}

/// `tests/fixtures/legacy-v1-session.json` was written by the TypeScript implementation
/// (register media, add a linked clip, split, set audio, add a caption).
#[test]
fn a_file_written_by_the_typescript_implementation_loads_and_stays_editable() {
    let sandbox = Sandbox::new();
    let session = Session::in_directory(sandbox.path("library"));
    std::fs::create_dir_all(sandbox.path("library")).unwrap();
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/legacy-v1-session.json"), &session.paths().session_file).unwrap();

    let state = session.load().unwrap();
    assert_eq!((state.revision, state.undo_depth, state.activity.len(), state.warnings.len()), (6, 5, 5, 0));
    assert_eq!(state.project.clips().count(), 5);
    assert_eq!(state.project.clip("c1a").unwrap().link.as_ref().unwrap().group_id, "linked-av-c1");
    assert!(state.project.companion_id("c1").is_some_and(|id| id == "c1a"));

    // A linked edit works on the loaded data: deleting the video clip takes its audio along.
    let applied = session.apply(&op("delete_clip", json!({ "clipId": "c1" })), Some(6)).unwrap();
    assert!(applied.project.clip("c1").is_err() && applied.project.clip("c1a").is_err());
    assert_eq!(session.load().unwrap().undo_depth, 6);
}
