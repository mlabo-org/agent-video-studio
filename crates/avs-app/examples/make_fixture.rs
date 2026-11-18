//! Builds an isolated test library for the editor and its self-test.
//!
//! `cargo run -p avs-app --example make_fixture -- <root> [--long]`
//!
//! Media are generated with ffmpeg's lavfi sources (no files from anywhere else),
//! imported through `avs_session` into the sandbox library that
//! `agent-video-studio --library <root>` opens, and placed with the same
//! placement code the editor uses. `--long` adds a 15-minute timeline of 36 clips.

use avs_app::edit::{self, PlaceMode};
use avs_core::Operation;
use avs_core::ops::{AddCaptions, AddTrack, CaptionInput};
use avs_core::{Time, TrackKind, secs};
use avs_session::{ImportRequest, Session};
use std::path::{Path, PathBuf};
use std::process::Command;

fn ffmpeg(args: &[&str]) {
    let status = Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).status().expect("ffmpeg must be installed");
    assert!(status.success(), "ffmpeg failed: {args:?}");
}

fn make_media(dir: &Path, long: bool) -> Vec<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    let file = |name: &str| dir.join(name);
    let a = file("clip-a.mp4");
    ffmpeg(&["-f", "lavfi", "-i", "testsrc=size=1280x720:rate=30:duration=10", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=10", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", a.to_str().unwrap()]);
    let b = file("clip-b.mp4");
    ffmpeg(&["-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30:duration=8", "-f", "lavfi", "-i", "aevalsrc=0.6*sin(2*PI*660*t)*abs(sin(PI*t)):s=48000:d=8", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", b.to_str().unwrap()]);
    let portrait = file("portrait.mp4");
    ffmpeg(&["-f", "lavfi", "-i", "smptebars=size=720x1280:rate=30:duration=6", "-c:v", "libx264", "-pix_fmt", "yuv420p", portrait.to_str().unwrap()]);
    let music = file("music.m4a");
    ffmpeg(&["-f", "lavfi", "-i", "aevalsrc=0.5*sin(2*PI*220*t)*abs(sin(PI*t/2)):s=48000:d=20", "-c:a", "aac", music.to_str().unwrap()]);
    let still = file("still.png");
    ffmpeg(&["-f", "lavfi", "-i", "color=c=0xE07020:size=1280x720,drawgrid=w=160:h=90:t=4:c=white@0.6", "-frames:v", "1", still.to_str().unwrap()]);
    let mut files = vec![a, b, portrait, music, still];
    if long {
        let long = file("long.mp4");
        ffmpeg(&["-f", "lavfi", "-i", "testsrc=size=1280x720:rate=30:duration=30", "-f", "lavfi", "-i", "sine=frequency=330:sample_rate=48000:duration=30", "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", long.to_str().unwrap()]);
        files.push(long);
    }
    files
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("usage: make_fixture <root> [--long]"));
    let long = args.any(|arg| arg == "--long");
    let env = avs_app::sandbox_env(&root).expect("sandbox");
    let session = Session::for_env(&env).expect("session");
    if session.paths().session_file.exists() {
        panic!("{} already has a session; use a fresh folder", session.data_directory().display());
    }
    session.ensure(Some(if long { "Long Fixture" } else { "Fixture" })).expect("create session");
    let files = make_media(&root.join("source-media"), long);
    let mut ids = Vec::new();
    for file in &files {
        let imported = avs_session::import_media(&env, &session, &ImportRequest { source_path: file.display().to_string(), ..ImportRequest::default() }, None).expect("import");
        ids.push(imported.media_id);
    }
    let place = |media_index: usize, mode: PlaceMode, track: &str, at: Time| {
        let state = session.load().unwrap();
        let media = state.project.media(&ids[media_index]).unwrap().clone();
        let operations = edit::place_media(&state.project, &media, mode, track, at).expect("placement");
        session.apply_many(&operations, Some(state.revision)).expect("apply placement");
    };
    if long {
        for index in 0..36 {
            let state = session.load().unwrap();
            let media = state.project.media(&ids[5]).unwrap().clone();
            let start = secs(index as f64 * 25.0);
            let mut operations = edit::place_media(&state.project, &media, PlaceMode::At, "V1", start).unwrap();
            if let Some(Operation::AddClips(add)) = operations.last_mut() {
                add.clips[0].duration_seconds = Some(secs(25.0));
                add.clips[0].source_start_seconds = Some(secs((index % 5) as f64));
            }
            session.apply_many(&operations, Some(state.revision)).expect("long clip");
        }
        place(3, PlaceMode::Connect, "A1", secs(0.0));
    } else {
        place(0, PlaceMode::At, "V1", secs(0.0));
        place(1, PlaceMode::At, "V1", secs(10.0));
        let state = session.load().unwrap();
        session
            .apply(&Operation::AddTrack(AddTrack { id: Some("V2".into()), kind: TrackKind::Video, name: Some("Overlay".into()), insert_before_track_id: Some("V1".into()), insert_after_track_id: None, locked: None, muted: None }), Some(state.revision))
            .unwrap();
        place(4, PlaceMode::At, "V2", secs(4.0));
        let state = session.load().unwrap();
        session.apply(&Operation::AddTrack(AddTrack { id: Some("A3".into()), kind: TrackKind::Audio, name: Some("Music".into()), insert_before_track_id: None, insert_after_track_id: None, locked: None, muted: None }), Some(state.revision)).unwrap();
        place(3, PlaceMode::At, "A3", secs(0.0));
        let state = session.load().unwrap();
        session
            .apply(
                &Operation::AddCaptions(AddCaptions {
                    track_id: Some("C1".into()),
                    captions: vec![CaptionInput { id: Some("caption-1".into()), track_id: None, text: "こんにちは、世界 Hello".into(), start_seconds: secs(2.0), duration_seconds: Some(secs(3.0)), end_seconds: None, speaker: None, language: None, properties: None }],
                }),
                Some(state.revision),
            )
            .unwrap();
    }
    let state = session.load().unwrap();
    println!("library: {}", session.data_directory().display());
    println!("revision: {}  clips: {}  media: {}  duration: {}s", state.revision, state.project.clips().count(), state.project.media.len(), state.project.duration().secs());
    for track in &state.project.tracks {
        let clips: Vec<String> = track.clips.iter().map(|clip| format!("{}[{}-{}]", clip.id, clip.start_seconds.secs(), clip.end().secs())).collect();
        println!("  {} {:?}: {}", track.id, track.kind, clips.join(" "));
    }
    for (index, id) in ids.iter().enumerate() {
        println!("  media {index}: {id}");
    }
}
