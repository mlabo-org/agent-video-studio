//! Imports small lavfi-made media with hand-written manifests. Everything lives in a
//! temp directory; nothing reads or writes the real user data.

use avs_core::{LinkRole, TrackKind};
use avs_handoff::{ImportOptions, import_handoff};
use avs_session::{Env, Session};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

fn find_tool(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(name)).find(|p| p.is_file())
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    job: PathBuf,
    env: Env,
    session: Session,
}

impl Fixture {
    /// A job folder with five tiny media files, an empty project, and an `Env` that can probe.
    fn new() -> Option<Fixture> {
        let ffmpeg = find_tool("ffmpeg")?;
        let ffprobe = find_tool("ffprobe")?;
        let dir = tempfile::Builder::new().prefix("avs-handoff-").tempdir().ok()?;
        let root = dir.path().to_path_buf();
        let job = root.join("job");
        std::fs::create_dir_all(job.join("assets")).ok()?;
        std::fs::create_dir_all(job.join("avs")).ok()?;
        let make = |args: &[&str], name: &str| -> Option<()> {
            let ok = Command::new(&ffmpeg).args(["-v", "error", "-y"]).args(args).arg(job.join("assets").join(name)).status().ok()?.success();
            ok.then_some(())
        };
        let png = ["-f", "lavfi", "-i", "color=c=blue:s=64x36:d=1", "-frames:v", "1"];
        make(&png, "scene.png")?;
        make(&png, "character.png")?;
        make(&["-f", "lavfi", "-i", "sine=duration=3"], "narration.wav")?;
        make(&["-f", "lavfi", "-i", "sine=frequency=220:duration=3"], "bgm.wav")?;
        make(&["-f", "lavfi", "-i", "testsrc=duration=3:size=320x240:rate=10", "-f", "lavfi", "-i", "sine=duration=3", "-shortest", "-pix_fmt", "yuv420p"], "source.mp4")?;
        let mut env = Env::new(root.join("home"));
        env.ffprobe_path = Some(ffprobe);
        let session = Session::in_directory(root.join("library"));
        session.create(Some("Test")).ok()?;
        Some(Fixture { _dir: dir, root, job, env, session })
    }

    fn asset(&self, name: &str) -> String {
        self.job.join("assets").join(name).display().to_string()
    }

    fn manifest_path(&self) -> PathBuf {
        self.job.join("avs").join("codexvideo-handoff.json")
    }

    /// The manifest the tests start from: every kind of object, ids prefixed with `p`.
    fn manifest(&self, p: &str) -> Value {
        json!({
            "schema": "avs.codexvideo-handoff.v1",
            "producer": "codex-video",
            "createdAt": "2026-08-12T00:00:00.000Z",
            "handoff": { "role": "downstream-editable-handoff" },
            "job": { "jobId": format!("{p}-job"), "profile": "long", "jobRoot": self.job.display().to_string() },
            "project": { "title": "Manifest Title", "width": 1280, "height": 720, "fps": 24, "durationSec": 4 },
            "assets": [
                { "id": format!("{p}-scene"), "type": "image", "role": "visual", "path": self.asset("scene.png"), "durationSec": 2, "width": 64, "height": 36,
                  "hash": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "metadata": { "codexVideo": { "sceneId": "s1" } } },
                { "id": format!("{p}-char"), "type": "image", "role": "visual", "path": self.asset("character.png"), "durationSec": 1, "width": 941, "height": 1672,
                  "metadata": { "codexVideo": { "sourceField": "character" } } },
                { "id": format!("{p}-nar"), "type": "audio", "role": "narration", "path": self.asset("narration.wav"), "durationSec": 3 },
                { "id": format!("{p}-bgm"), "type": "audio", "role": "bgm", "path": self.asset("bgm.wav"),
                  "sha256": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
                { "id": format!("{p}-src"), "type": "video", "role": "visual", "path": self.asset("source.mp4") }
            ],
            "tracks": [
                { "id": "cv-visual", "kind": "video", "name": "CodexVideo visual" },
                { "id": "cv-character", "kind": "video", "name": "CodexVideo character" },
                { "id": "cv-narration", "kind": "audio" },
                { "id": "cv-bgm", "kind": "audio" },
                { "id": "cv-captions", "kind": "caption" }
            ],
            "clips": [
                { "id": format!("{p}-c-scene"), "trackId": "cv-visual", "assetId": format!("{p}-scene"), "startSec": 0, "durationSec": 2, "sourceStartSec": 0 },
                { "id": format!("{p}-c-src"), "trackId": "cv-visual", "assetId": format!("{p}-src"), "startSec": 2, "durationSec": 2,
                  "properties": { "volumeDb": -9, "fadeInSeconds": 0.5, "metadata": { "codexVideo": { "cue": "x" } } } },
                { "id": format!("{p}-c-char"), "trackId": "cv-character", "assetId": format!("{p}-char"), "startSec": 0, "durationSec": 4,
                  "properties": { "metadata": { "codexVideo": { "clipRole": "character", "character": { "slot": "right" } } } } },
                { "id": format!("{p}-c-nar"), "trackId": "cv-narration", "assetId": format!("{p}-nar"), "startSec": 0, "durationSec": 3, "properties": { "volumeDb": -3.5 } },
                { "id": format!("{p}-c-bgm"), "role": "bgm", "trackId": "cv-bgm", "assetId": format!("{p}-bgm"), "startSec": 0, "durationSec": 2.5, "properties": { "volumeDb": -20 } }
            ],
            "captions": [
                { "id": format!("{p}-cap1"), "trackId": "cv-captions", "startSec": 0.5, "durationSec": 1.5, "text": " Structured ", "speaker": "Narrator", "language": "en",
                  "metadata": { "codexVideo": { "sceneId": "s1", "sceneIndex": 0 } },
                  "style": { "placement": "lower-third", "color": "#fff", "fontSize": 32, "telopPreset": "x" } },
                { "id": format!("{p}-cap2"), "trackId": "cv-captions", "startSec": 2.5, "durationSec": 1, "text": "Telop", "style": "telop" }
            ],
            "audio": { "sourceAudioPreserved": true, "mutedIntervals": [ { "startSec": 0, "durationSec": 1, "target": "bgm" }, { "reason": "no times" } ] },
            "warnings": [ { "code": "FROM_PRODUCER" } ],
            "unresolved": [ { "type": "missing-visual-asset", "sceneId": "s2" } ]
        })
    }

    fn write(&self, manifest: &Value) -> PathBuf {
        let path = self.manifest_path();
        std::fs::write(&path, serde_json::to_vec_pretty(manifest).unwrap()).unwrap();
        path
    }

    fn import(&self, manifest: &Value, options: &ImportOptions) -> Result<avs_handoff::ImportReport, avs_handoff::HandoffError> {
        let path = self.write(manifest);
        import_handoff(&self.env, &self.session, &path, options)
    }

    /// Files under the library's media folder (copies made by imports).
    fn copied_files(&self) -> usize {
        fn walk(dir: &Path) -> usize {
            std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| if e.path().is_dir() { walk(&e.path()) } else { 1 }).sum()).unwrap_or(0)
        }
        walk(&self.session.paths().media_root())
    }
}

macro_rules! fixture {
    () => {
        match Fixture::new() {
            Some(fixture) => fixture,
            None => {
                eprintln!("SKIPPED: ffmpeg/ffprobe not found on PATH");
                return;
            }
        }
    };
}

type Change = Box<dyn Fn(&mut Value)>;

fn codes(report: &avs_handoff::ImportReport) -> Vec<&str> {
    report.warnings.iter().map(|w| w.code.as_str()).collect()
}

#[test]
fn everything_lands_in_one_revision_and_one_undo_restores_the_project() {
    let fx = fixture!();
    let before = fx.session.load().unwrap();
    let report = fx.import(&fx.manifest("a"), &ImportOptions { expected_revision: Some(before.revision), ..ImportOptions::default() }).unwrap();

    // one revision, one undo step
    assert_eq!((report.revision, report.undo_depth), (before.revision + 1, 1));
    let state = fx.session.load().unwrap();
    assert_eq!(state.revision, report.revision);
    let project = &state.project;

    // project settings and metadata
    assert_eq!((project.title.as_str(), project.fps, project.resolution.width, project.resolution.height), ("Manifest Title", 24.0, 1280, 720));
    let codex = &project.metadata.as_ref().unwrap()["codexVideo"];
    assert_eq!(codex["jobId"], "a-job");
    assert_eq!(codex["jobRoot"], fx.job.display().to_string());
    assert_eq!(codex["sourceAudioPreserved"], true);
    let intervals = codex["mutedIntervals"].as_array().unwrap();
    assert_eq!(intervals.len(), 1, "the interval with no times is dropped");
    assert_eq!((intervals[0]["startSeconds"].as_f64(), intervals[0]["endSeconds"].as_f64()), (Some(0.0), Some(1.0)));
    assert_eq!(codex["audio"]["mutedIntervals"], codex["mutedIntervals"]);

    // tracks: the character track sits above cv-visual; declared names are kept
    let ids: Vec<&str> = project.tracks.iter().map(|t| t.id.as_str()).collect();
    let at = |id: &str| ids.iter().position(|i| *i == id).unwrap();
    assert!(at("cv-character") < at("cv-visual"), "{ids:?}");
    assert_eq!(project.track("cv-visual").unwrap().name, "CodexVideo visual");
    assert_eq!(project.track("cv-captions").unwrap().kind, TrackKind::Caption);
    assert_eq!(report.created.track_ids.len(), 5);
    assert!(report.tracks.iter().all(|t| t.status == avs_handoff::TrackStatus::Created && !t.inferred));

    // media: copied into the library, with provenance
    assert_eq!(report.media.len(), 5);
    assert_eq!(fx.copied_files(), 5);
    let scene = report.media.iter().find(|m| m.manifest_asset_id == "a-scene").unwrap();
    assert!(scene.source.starts_with("/project-library/media/imports/"));
    let media = project.media(&scene.media_id).unwrap();
    assert_eq!(media.metadata.as_ref().unwrap()["codexVideo"]["manifestAssetId"], "a-scene");
    assert_eq!(media.tags.as_deref(), Some(&["codex-video".to_string(), "visual".to_string()][..]));
    assert_eq!(fx.session.resolve_media_source(&scene.source).unwrap(), scene.library.absolute_path);

    // clips: ids are the manifest's; embedded audio is a linked companion on A1 and takes the audio properties
    let entry = report.clips.iter().find(|c| c.manifest_clip_id == "a-c-src").unwrap();
    assert_eq!((entry.linked_audio_clip_id.as_deref(), entry.linked_audio_track_id.as_deref()), (Some("a-c-src-audio"), Some("A1")));
    let video = project.clip("a-c-src").unwrap();
    let audio = project.clip("a-c-src-audio").unwrap();
    assert_eq!((video.link.as_ref().unwrap().role, audio.link.as_ref().unwrap().role), (LinkRole::Video, LinkRole::Audio));
    assert_eq!(entry.link_group_id.as_deref(), Some(video.link.as_ref().unwrap().group_id.as_str()));
    assert_eq!((video.properties.volume_db, audio.properties.volume_db, audio.properties.fade_in_seconds.map(|t| t.secs())), (None, Some(-9.0), Some(0.5)));
    assert_eq!(video.properties.metadata.as_ref().unwrap()["codexVideo"]["cue"], "x");
    assert_eq!(video.properties.metadata.as_ref().unwrap()["codexVideo"]["manifestClipId"], "a-c-src");
    assert_eq!(project.clip("a-c-bgm").unwrap().properties.metadata.as_ref().unwrap()["codexVideo"]["role"], "bgm");
    assert_eq!(project.clip("a-c-nar").unwrap().properties.volume_db, Some(-3.5));

    // character overlay: right slot of a 1280x720 frame, no transform given by the manifest
    let character = project.clip("a-c-char").unwrap();
    let (position, scale) = (character.properties.position.unwrap(), character.properties.scale.unwrap());
    assert!(position.x > 640.0 && position.x < 1280.0 && position.y > 360.0, "{position:?}");
    assert!(scale > 0.0 && scale < 1.0);
    assert_eq!(character.properties.opacity, Some(1.0));

    // captions: trimmed text, object style becomes captionStyle, string style stays in metadata only
    let cap1 = project.clip("a-cap1").unwrap();
    assert_eq!(cap1.caption.as_ref().unwrap().text, "Structured");
    let style = cap1.properties.caption_style.as_ref().unwrap();
    assert_eq!((style.placement.as_deref(), style.color.as_deref(), style.font_size), (Some("lower-third"), Some("#fff"), Some(32.0)));
    let cap_codex = &cap1.properties.metadata.as_ref().unwrap()["codexVideo"];
    assert_eq!((&cap_codex["sceneId"], &cap_codex["manifestCaptionId"]), (&json!("s1"), &json!("a-cap1")));
    let cap2 = project.clip("a-cap2").unwrap();
    assert!(cap2.properties.caption_style.is_none());
    assert_eq!(cap2.properties.metadata.as_ref().unwrap()["codexVideo"]["style"], "telop");

    // the report is enough to continue: maps, created ids, inspection, warnings
    assert_eq!(report.created.clip_ids.len(), 6);
    assert_eq!(report.created.caption_ids, ["a-cap1", "a-cap2"]);
    assert_eq!(report.captions[0].track_id, "cv-captions");
    assert_eq!((report.timeline.clip_count, report.timeline.caption_count, report.timeline.media_count), (8, 2, 5));
    assert!(report.validation.ok && !report.validation.import_complete && report.validation.unresolved_count == 1);
    assert_eq!(report.manifest.warnings.len(), 1);
    let found = codes(&report);
    assert!(found.contains(&"CODEXVIDEO_HANDOFF_MUTED_INTERVAL_DROPPED"), "{found:?}");
    assert_eq!(found.iter().filter(|c| **c == "CODEXVIDEO_HANDOFF_ASSET_HASH_MISMATCH").count(), 2, "both `hash` and `sha256` are checked: {found:?}");
    assert!(avs_core::validate(project).ok);
    assert!(serde_json::to_string(&report).unwrap().contains("\"mediaId\""));

    // one undo puts the project back exactly
    fx.session.undo(None).unwrap();
    assert_eq!(fx.session.load().unwrap().project, before.project);
}

#[test]
fn a_failure_saves_nothing_and_removes_the_copies() {
    let fx = fixture!();
    let before = fx.session.load().unwrap();
    // two clips on one track at the same time: core rejects the whole list
    let mut manifest = fx.manifest("b");
    manifest["clips"].as_array_mut().unwrap().push(
        json!({ "id": "b-clash", "trackId": "cv-narration", "assetId": "b-nar", "startSec": 1, "durationSec": 1 }),
    );
    let error = fx.import(&manifest, &ImportOptions::default()).unwrap_err();
    assert_eq!(error.code, "CLIP_OVERLAP", "{error}");
    assert!(error.details["operationIndex"].is_number());
    let after = fx.session.load().unwrap();
    assert_eq!((after.revision, &after.project), (before.revision, &before.project));
    assert_eq!(fx.copied_files(), 0, "the five copies were deleted");

    // a stale expected revision is refused before anything is copied
    let error = fx.import(&fx.manifest("b"), &ImportOptions { expected_revision: Some(before.revision + 5), ..ImportOptions::default() }).unwrap_err();
    assert_eq!(error.code, "REVISION_MISMATCH");
    assert_eq!(fx.copied_files(), 0);
}

#[test]
fn bad_manifests_are_refused_before_any_copy() {
    let fx = fixture!();
    let cases: Vec<(&str, Change)> = vec![
        ("CODEXVIDEO_HANDOFF_UNSUPPORTED_SCHEMA", Box::new(|m| m["schema"] = json!("other.v1"))),
        ("CODEXVIDEO_HANDOFF_UNSUPPORTED_PRODUCER", Box::new(|m| m["producer"] = json!("someone"))),
        ("CODEXVIDEO_HANDOFF_UNSUPPORTED_ASSET_TYPE", Box::new(|m| m["assets"][0]["type"] = json!("sequence"))),
        ("CODEXVIDEO_HANDOFF_MISSING_ASSET", Box::new(|m| m["assets"][0]["path"] = json!("relative/scene.png"))),
        ("CODEXVIDEO_HANDOFF_MISSING_ASSET", Box::new(|m| m["assets"][0]["path"] = json!("/nonexistent/dir/scene.png"))),
        ("CODEXVIDEO_HANDOFF_INVALID_MANIFEST", Box::new(|m| m["tracks"][1]["id"] = m["tracks"][0]["id"].clone())),
        ("CODEXVIDEO_HANDOFF_INVALID_MANIFEST", Box::new(|m| m["captions"][0]["id"] = m["clips"][0]["id"].clone())),
        ("CODEXVIDEO_HANDOFF_UNKNOWN_MEDIA", Box::new(|m| m["clips"][0]["assetId"] = json!("nope"))),
        ("CODEXVIDEO_HANDOFF_INVALID_TIMING", Box::new(|m| m["clips"][0]["durationSec"] = json!(0))),
        ("CODEXVIDEO_HANDOFF_INVALID_TIMING", Box::new(|m| m["clips"][0]["endSec"] = json!(9))),
        ("CODEXVIDEO_HANDOFF_INVALID_PROJECT", Box::new(|m| m["project"]["height"] = Value::Null)),
        ("CODEXVIDEO_HANDOFF_INVALID_MANIFEST", Box::new(|m| m["clips"][0]["properties"] = json!({ "opacity": 2 }))),
        ("CODEXVIDEO_HANDOFF_UNKNOWN_TRACK", Box::new(|m| m["tracks"][1]["kind"] = json!("audio"))),
        ("CODEXVIDEO_HANDOFF_JOB_ROOT_NOT_ABSOLUTE", Box::new(|m| m["job"]["jobRoot"] = json!("job"))),
        ("CODEXVIDEO_HANDOFF_JOB_ROOT_MISSING", Box::new(|m| m["job"]["jobRoot"] = json!("/nonexistent/job-root"))),
    ];
    for (code, change) in cases {
        let mut manifest = fx.manifest("c");
        change(&mut manifest);
        let error = fx.import(&manifest, &ImportOptions::default()).unwrap_err();
        assert_eq!(error.code, code, "{error} {}", error.details);
        assert_eq!(fx.copied_files(), 0);
        assert_eq!(fx.session.load().unwrap().revision, 1);
    }

    // the manifest file itself
    let missing = import_handoff(&fx.env, &fx.session, &fx.root.join("none.json"), &ImportOptions::default()).unwrap_err();
    assert_eq!(missing.code, "CODEXVIDEO_HANDOFF_MANIFEST_MISSING");
    let relative = import_handoff(&fx.env, &fx.session, Path::new("handoff.json"), &ImportOptions::default()).unwrap_err();
    assert_eq!(relative.code, "CODEXVIDEO_HANDOFF_MANIFEST_NOT_ABSOLUTE");
    std::fs::write(fx.manifest_path(), b"{ not json").unwrap();
    let broken = import_handoff(&fx.env, &fx.session, &fx.manifest_path(), &ImportOptions::default()).unwrap_err();
    assert_eq!(broken.code, "CODEXVIDEO_HANDOFF_MANIFEST_JSON_INVALID");
    let link = fx.job.join("avs").join("link.json");
    std::os::unix::fs::symlink(fx.manifest_path(), &link).unwrap();
    assert_eq!(import_handoff(&fx.env, &fx.session, &link, &ImportOptions::default()).unwrap_err().code, "CODEXVIDEO_HANDOFF_MANIFEST_SYMLINK");
}

#[test]
fn importing_into_a_project_with_clips_reports_what_it_replaced_and_can_keep_it() {
    let fx = fixture!();
    fx.import(&fx.manifest("d"), &ImportOptions::default()).unwrap();

    // the same manifest again: its ids are taken, so it is refused without copying
    let copied = fx.copied_files();
    let again = fx.import(&fx.manifest("d"), &ImportOptions::default()).unwrap_err();
    assert_eq!(again.code, "CODEXVIDEO_HANDOFF_CLIP_ID_CONFLICT");
    assert_eq!(fx.copied_files(), copied);

    // another job's manifest: tracks of the same kind are reused, project settings follow the manifest and the change is recorded
    let mut second = fx.manifest("e");
    second["project"] = json!({ "title": "Second", "width": 1920, "height": 1080, "fps": 30 });
    second["job"]["jobId"] = json!("e-job");
    let report = fx.import(&second, &ImportOptions::default()).unwrap_err();
    // clips of both manifests would overlap on the shared tracks: strict placement is kept
    assert_eq!(report.code, "CLIP_OVERLAP");
    for clips in ["clips", "captions"] {
        for item in second[clips].as_array_mut().unwrap() {
            let start = item["startSec"].as_f64().unwrap();
            item["startSec"] = json!(start + 10.0);
        }
    }
    let report = fx.import(&second, &ImportOptions::default()).unwrap();
    assert!(report.tracks.iter().all(|t| t.status == avs_handoff::TrackStatus::Existing));
    assert!(report.created.track_ids.is_empty());
    assert_eq!(codes(&report).iter().filter(|c| **c == "CODEXVIDEO_HANDOFF_PROJECT_SETTINGS_OVERWRITTEN").count(), 1, "{:?}", codes(&report));
    assert_eq!(report.project.title, "Manifest Title", "the manifest's title does not replace a title the project already has clips under");
    assert_eq!(report.project.resolution.width, 1920);
    let kept = report.warnings.iter().find(|w| w.code == "CODEXVIDEO_HANDOFF_PROJECT_SETTINGS_KEPT").unwrap();
    assert_eq!(kept.details["kept"], json!(["title"]));
    assert!(codes(&report).contains(&"CODEXVIDEO_HANDOFF_PROJECT_JOB_REPLACED"));

    // keepProjectSettings leaves them alone; an explicit title still applies
    let mut third = fx.manifest("f");
    third["project"] = json!({ "title": "Third", "width": 640, "height": 360, "fps": 12 });
    for clips in ["clips", "captions"] {
        for item in third[clips].as_array_mut().unwrap() {
            item["startSec"] = json!(item["startSec"].as_f64().unwrap() + 30.0);
        }
    }
    let options = ImportOptions { keep_project_settings: true, project_title: Some("Mine".into()), ..ImportOptions::default() };
    let report = fx.import(&third, &options).unwrap();
    assert_eq!((report.project.title.as_str(), report.project.fps, report.project.resolution.width), ("Mine", 30.0, 1920));
    assert!(codes(&report).contains(&"CODEXVIDEO_HANDOFF_PROJECT_SETTINGS_KEPT"));
    assert!(!codes(&report).contains(&"CODEXVIDEO_HANDOFF_PROJECT_SETTINGS_OVERWRITTEN"));
}

#[test]
fn a_clip_slightly_past_its_media_is_cut_to_the_file_but_a_real_overrun_is_refused() {
    let fx = fixture!();
    // bgm.wav is 3 s and the manifest gives no duration for it, so ffprobe's value is the length
    let mut manifest = fx.manifest("g");
    manifest["clips"][4]["durationSec"] = json!(3.05);
    let report = fx.import(&manifest, &ImportOptions::default()).unwrap();
    let warning = report.warnings.iter().find(|w| w.code == "CODEXVIDEO_HANDOFF_CLIP_TRIMMED_TO_MEDIA").expect("warned");
    assert_eq!(warning.details["clipId"], "g-c-bgm");
    let clip = fx.session.load().unwrap().project.clip("g-c-bgm").unwrap().clone();
    assert!(clip.duration_seconds.secs() < 3.05 && clip.duration_seconds.secs() > 2.9, "{}", clip.duration_seconds);
    assert_eq!(report.clips.iter().find(|c| c.clip_id == "g-c-bgm").unwrap().duration_seconds, clip.duration_seconds.secs());

    let fx = fixture!();
    let mut manifest = fx.manifest("g");
    manifest["clips"][4]["durationSec"] = json!(3.5);
    assert_eq!(fx.import(&manifest, &ImportOptions::default()).unwrap_err().code, "MEDIA_RANGE_EXCEEDED");
    assert_eq!(fx.copied_files(), 0);
}

#[test]
fn a_manifest_exported_outside_its_job_root_imports_without_recording_the_root() {
    let fx = fixture!();
    let elsewhere = fx.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let manifest = fx.manifest("h");
    let path = elsewhere.join("handoff.json");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let report = import_handoff(&fx.env, &fx.session, &path, &ImportOptions::default()).unwrap();
    assert!(codes(&report).contains(&"CODEXVIDEO_HANDOFF_JOB_ROOT_NOT_RECORDED"));
    let project = fx.session.load().unwrap().project;
    let codex = &project.metadata.as_ref().unwrap()["codexVideo"];
    assert!(codex.get("jobRoot").is_none() && codex["jobId"] == "h-job");
}
