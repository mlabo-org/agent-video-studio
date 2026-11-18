mod common;

use avs_core::{Time, secs};
use avs_render::{CancelToken, ExportOptions, GraphOptions, PreviewOptions, PreviewStream, RenderError, RenderGraph, Streams, VideoFormat, export, render_frame};
use common::*;
use serde_json::json;
use std::io::Read;

fn near(actual: u8, expected: u8, tolerance: u8) -> bool {
    actual.abs_diff(expected) <= tolerance
}

// ---- graph text: only the load-bearing facts ---------------------------------------

#[test]
fn graph_mixes_without_normalising_and_keeps_the_silent_base_first() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let project = project(
        640,
        360,
        vec![media("tone", "audio", 4.0, "tone.wav")],
        vec![track("A1", "audio", vec![clip("c1", "A1", "tone", 0.0, 2.0, json!({}))]), track("A2", "audio", vec![clip("c2", "A2", "tone", 1.0, 2.0, json!({}))])],
    );
    let mut options = GraphOptions::for_project(&project);
    options.streams = Streams::AudioOnly;
    let graph = RenderGraph::build(&project, &resolver(dir.path()), &options).unwrap();
    let text = &graph.filter_complex;
    assert!(text.contains("normalize=0"), "{text}");
    assert!(text.contains("[abase][a0][a1]amix=inputs=3:duration=first"), "{text}");
    // Audio-only graphs open no picture inputs and draw nothing.
    assert!(graph.video_label.is_none() && !text.contains("overlay"));
    assert_eq!(graph.duration, secs(3.0));
}

#[test]
fn a_single_clip_takes_the_same_mix_path_as_many() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let project = project(640, 360, vec![media("tone", "audio", 4.0, "tone.wav")], vec![track("A1", "audio", vec![clip("c1", "A1", "tone", 0.0, 2.0, json!({}))])]);
    let graph = RenderGraph::build(&project, &resolver(dir.path()), &GraphOptions::for_project(&project)).unwrap();
    assert!(graph.filter_complex.contains("[abase][a0]amix=inputs=2:duration=first:dropout_transition=0:normalize=0"), "{}", graph.filter_complex);
}

#[test]
fn speed_chains_atempo_and_moves_the_source_start_with_the_window() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let mut fast = clip("fast", "A1", "tone", 0.0, 1.0, json!({ "speed": 4.0 }));
    fast["sourceStartSeconds"] = json!(0.5);
    let mut slow = clip("slow", "A2", "tone", 0.0, 1.0, json!({ "speed": 0.25 }));
    slow["sourceStartSeconds"] = json!(0.0);
    let mut cut = clip("cut", "A3", "tone", 0.0, 2.0, json!({ "speed": 1.5 }));
    cut["sourceStartSeconds"] = json!(0.5);
    let project = project(
        640,
        360,
        vec![media("tone", "audio", 8.0, "tone.wav")],
        vec![track("A1", "audio", vec![fast]), track("A2", "audio", vec![slow]), track("A3", "audio", vec![cut])],
    );
    let mut options = GraphOptions::for_project(&project);
    options.streams = Streams::AudioOnly;
    let graph = RenderGraph::build(&project, &resolver(dir.path()), &options).unwrap();
    let text = &graph.filter_complex;
    assert!(text.contains("atempo=2,atempo=2"), "{text}");
    assert!(text.contains("atempo=0.5,atempo=0.5"), "{text}");
    // The window starts 0.5s into "cut", which plays at 1.5x: 0.5 + 0.5 * 1.5 = 1.25 into the file.
    options.start = secs(0.5);
    options.duration = Some(secs(1.0));
    let graph = RenderGraph::build(&project, &resolver(dir.path()), &options).unwrap();
    let cut_source = graph.sources.iter().find(|source| source.clip_id == "cut").unwrap();
    assert_eq!(cut_source.source_start_seconds, secs(1.25));
    let input = &graph.inputs[cut_source.input_index];
    assert_eq!(input.options, ["-ss", "1.25", "-t", "1.5"]);
    // The clip lasts 2s from 0, the window shows 0.5..1.5, so it plays from the window start.
    assert_eq!(cut_source.start_seconds, secs(0.5));
}

#[test]
fn the_first_video_track_is_the_top_layer() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let project = project(
        640,
        360,
        vec![media("red", "video", 4.0, "red.mp4"), media("blue", "video", 4.0, "blue.mp4")],
        vec![track("V1", "video", vec![clip("top", "V1", "red", 0.0, 2.0, json!({}))]), track("V2", "video", vec![clip("below", "V2", "blue", 0.0, 2.0, json!({}))])],
    );
    let mut options = GraphOptions::for_project(&project);
    options.streams = Streams::VideoOnly;
    let graph = RenderGraph::build(&project, &resolver(dir.path()), &options).unwrap();
    // The lower track is composited first, so the upper one lands on top of it.
    assert_eq!(graph.sources[0].clip_id, "below");
    assert_eq!(graph.sources[1].clip_id, "top");
    assert!(graph.filter_complex.contains("[base0][vis0]overlay") && graph.filter_complex.contains("[base1][vis1]overlay"), "{}", graph.filter_complex);
}

#[test]
fn sequence_media_is_reported_not_dropped_silently() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let project = project(
        640,
        360,
        vec![media("seq", "sequence", 4.0, "red.mp4"), media("red", "video", 4.0, "red.mp4")],
        vec![track("V1", "video", vec![clip("s", "V1", "seq", 0.0, 1.0, json!({})), clip("r", "V1", "red", 1.0, 1.0, json!({}))])],
    );
    let graph = RenderGraph::build(&project, &resolver(dir.path()), &GraphOptions::for_project(&project)).unwrap();
    assert!(graph.warnings.iter().any(|w| w.code == "SEQUENCE_MEDIA_UNSUPPORTED" && w.clip_id.as_deref() == Some("s")), "{:?}", graph.warnings);
}

// ---- real renders -----------------------------------------------------------------

fn full_project() -> avs_core::Project {
    project(
        640,
        360,
        vec![media("av", "video", 4.0, "av.mp4"), media("tone", "audio", 4.0, "tone.wav"), media("green", "image", 1.0, "green.png")],
        vec![
            track("V2", "video", vec![clip("still", "V2", "green", 1.0, 1.0, json!({ "scale": 0.25, "position": { "x": 320, "y": 180 } }))]),
            track("V1", "video", vec![clip("video", "V1", "av", 0.0, 3.0, json!({}))]),
            track("A1", "audio", vec![clip("video-audio", "A1", "av", 0.0, 3.0, json!({}))]),
            track("A2", "audio", vec![clip("music", "A2", "tone", 0.0, 3.0, json!({ "volumeDb": -6.0 }))]),
            track("C1", "caption", vec![caption("cap", "C1", 0.5, 2.0, "字幕のテスト\nsecond line", json!({ "placement": "lower-third", "background": "#111827" }))]),
        ],
    )
}

#[test]
fn exports_a_real_mp4_with_the_right_length_size_and_streams() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let output = dir.path().join("out.mp4");
    let ratios = std::sync::Arc::new(std::sync::Mutex::new(Vec::<f64>::new()));
    let seen = ratios.clone();
    let finished_with_file = std::sync::Arc::new(std::sync::Mutex::new(Vec::<bool>::new()));
    let finished_seen = finished_with_file.clone();
    let watched = output.clone();
    let mut options = ExportOptions::new(&output);
    options.on_progress = Some(Box::new(move |progress| {
        seen.lock().unwrap().push(progress.ratio);
        if progress.finished {
            finished_seen.lock().unwrap().push(watched.is_file());
        }
    }));
    let result = export(&full_project(), &resolver(dir.path()), options).expect("export");

    let probe = ffprobe_json(&output);
    let streams = probe["streams"].as_array().unwrap();
    let video = streams.iter().find(|s| s["codec_type"] == "video").expect("video stream");
    let audio = streams.iter().find(|s| s["codec_type"] == "audio").expect("audio stream");
    assert_eq!(video["codec_name"], "h264");
    assert_eq!((video["width"].as_u64(), video["height"].as_u64()), (Some(640), Some(360)));
    assert_eq!(video["pix_fmt"], "yuv420p");
    assert_eq!(audio["codec_name"], "aac");
    let duration: f64 = probe["format"]["duration"].as_str().unwrap().parse().unwrap();
    assert!((duration - 3.0).abs() < 0.1, "duration {duration}");

    assert_eq!(result.duration_seconds, secs(3.0));
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert_eq!(result.caption_clip_ids, ["cap"]);
    assert!(result.ffmpeg_command.contains("-filter_complex") && result.ffmpeg_command.contains("normalize=0"));
    assert!(!dir.path().join(".out.mp4.partial.mp4").exists(), "the partial file must be gone");
    assert_eq!(*finished_with_file.lock().unwrap(), [true], "`finished` is reported once, after the file is in place");
    let ratios = ratios.lock().unwrap();
    assert!(ratios.last().is_some_and(|last| (*last - 1.0).abs() < 1e-9), "{ratios:?}");
    // The result is plain JSON.
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["durationSeconds"], 3.0);
}

#[test]
fn volume_does_not_depend_on_how_many_clips_play() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let media_list = vec![media("tone", "audio", 4.0, "tone.wav"), media("silence", "audio", 4.0, "silence.wav")];
    let one = project(640, 360, media_list.clone(), vec![track("A1", "audio", vec![clip("t", "A1", "tone", 0.0, 3.0, json!({}))])]);
    let two = project(
        640,
        360,
        media_list.clone(),
        vec![track("A1", "audio", vec![clip("t", "A1", "tone", 0.0, 3.0, json!({}))]), track("A2", "audio", vec![clip("s", "A2", "silence", 0.0, 3.0, json!({}))])],
    );
    let many = project(
        640,
        360,
        media_list,
        vec![
            track("A1", "audio", vec![clip("t", "A1", "tone", 0.0, 3.0, json!({}))]),
            track("A2", "audio", vec![clip("s", "A2", "silence", 0.0, 3.0, json!({}))]),
            track("A3", "audio", vec![clip("s2", "A3", "silence", 0.0, 3.0, json!({}))]),
            track("A4", "audio", vec![clip("s3", "A4", "silence", 0.0, 3.0, json!({}))]),
        ],
    );
    let mut levels = Vec::new();
    for (name, project) in [("one", &one), ("two", &two), ("many", &many)] {
        let output = dir.path().join(format!("{name}.mp4"));
        export(project, &resolver(dir.path()), ExportOptions::new(&output)).expect("export");
        levels.push(mean_volume(&output, 0.5, 2.5));
    }
    eprintln!("mean_volume one={} two={} many={}", levels[0], levels[1], levels[2]);
    assert!(levels[0] > -40.0, "the tone must be audible: {levels:?}");
    assert!((levels[0] - levels[1]).abs() <= 1.0, "{levels:?}");
    assert!((levels[0] - levels[2]).abs() <= 1.0, "{levels:?}");
}

#[test]
fn audio_lasts_as_long_as_the_timeline_even_where_no_clip_plays() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    // The only sound stops at 1s but the picture runs to 3s.
    let project = project(
        640,
        360,
        vec![media("tone", "audio", 4.0, "tone.wav"), media("red", "video", 4.0, "red.mp4")],
        vec![track("V1", "video", vec![clip("v", "V1", "red", 0.0, 3.0, json!({}))]), track("A1", "audio", vec![clip("t", "A1", "tone", 0.0, 1.0, json!({}))])],
    );
    let output = dir.path().join("gap.mp4");
    export(&project, &resolver(dir.path()), ExportOptions::new(&output)).expect("export");
    let probe = ffprobe_json(&output);
    let audio = probe["streams"].as_array().unwrap().iter().find(|s| s["codec_type"] == "audio").unwrap().clone();
    let audio_duration: f64 = audio["duration"].as_str().unwrap().parse().unwrap();
    assert!((audio_duration - 3.0).abs() < 0.1, "audio lasts {audio_duration}s");
}

#[test]
fn bgm_is_silenced_only_in_its_muted_intervals() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let mut bgm = media("bgm", "audio", 4.0, "tone.wav");
    bgm["metadata"] = json!({ "codexVideo": { "role": "bgm" } });
    let mut project = project(640, 360, vec![bgm], vec![track("A1", "audio", vec![clip("b", "A1", "bgm", 0.0, 4.0, json!({}))])]);
    project.metadata = Some(json!({ "codexVideo": { "audio": { "sourceAudioPreserved": true, "mutedIntervals": [{ "startSec": 1.0, "endSec": 2.0 }] } } }).as_object().unwrap().clone());
    let output = dir.path().join("bgm.mp4");
    export(&project, &resolver(dir.path()), ExportOptions::new(&output)).expect("export");
    assert!(mean_volume(&output, 0.2, 0.8) > -40.0);
    assert!(mean_volume(&output, 1.2, 1.8) < -80.0, "muted stretch: {}", mean_volume(&output, 1.2, 1.8));
    assert!(mean_volume(&output, 2.3, 3.7) > -40.0);

    // The same interval in a preview that starts at 1.5s lands at 0..0.5 of the output.
    let mut options = GraphOptions::for_project(&project);
    options.start = secs(1.5);
    options.streams = Streams::AudioOnly;
    let graph = RenderGraph::build(&project, &resolver(dir.path()), &options).unwrap();
    assert!(graph.filter_complex.contains("volume=0:enable='gte(t,0)*lt(t,0.5)'"), "{}", graph.filter_complex);

    // `sourceAudioPreserved: false` switches the project's intervals off.
    project.metadata = Some(json!({ "codexVideo": { "audio": { "sourceAudioPreserved": false, "mutedIntervals": [{ "startSec": 1.0, "endSec": 2.0 }] } } }).as_object().unwrap().clone());
    let graph = RenderGraph::build(&project, &resolver(dir.path()), &GraphOptions::for_project(&project)).unwrap();
    assert!(!graph.filter_complex.contains("volume=0"), "{}", graph.filter_complex);
}

#[test]
fn a_caption_is_drawn_into_the_frame() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let project = project(
        640,
        360,
        vec![media("red", "video", 4.0, "red.mp4")],
        vec![
            track("V1", "video", vec![clip("v", "V1", "red", 0.0, 3.0, json!({}))]),
            track("C1", "caption", vec![caption("cap", "C1", 1.0, 1.0, "こんにちは世界 Hello", json!({ "placement": "lower-third", "color": "#00ff00", "fontSize": 40 }))]),
        ],
    );
    let (w, h) = (640u32, 360u32);
    let without = render_frame(&project, &resolver(dir.path()), secs(0.5), w, h).unwrap();
    let with = render_frame(&project, &resolver(dir.path()), secs(1.5), w, h).unwrap();
    assert_eq!(with.len(), (w * h * 4) as usize);
    let red = [255u8, 0, 0];
    let off_background = |frame: &[u8], rows: std::ops::Range<u32>| {
        let mut count = 0;
        for y in rows {
            for x in 0..w {
                let p = pixel(frame, w, x, y);
                if !(near(p[0], red[0], 40) && near(p[1], red[1], 40) && near(p[2], red[2], 40)) {
                    count += 1;
                }
            }
        }
        count
    };
    // lower-third: the text sits between 0.55 and 0.85 of the height.
    assert_eq!(off_background(&without, 0..h), 0, "the frame without a caption is plain red");
    assert!(off_background(&with, (h * 55 / 100)..(h * 85 / 100)) > 200, "the caption area must contain text pixels");
    assert_eq!(off_background(&with, 0..(h * 40 / 100)), 0, "nothing is drawn above the caption");
    // Green text, so some pixel there is clearly green.
    let greenish = (h * 55 / 100..h * 85 / 100).any(|y| (0..w).any(|x| {
        let p = pixel(&with, w, x, y);
        p[1] > 200 && p[0] < 80
    }));
    assert!(greenish, "the caption colour must be used");
}

#[test]
fn clips_are_placed_scaled_faded_and_fitted_like_the_preview_says() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let (w, h) = (640u32, 360u32);
    let frame_of = |tracks: Vec<serde_json::Value>, at: f64| {
        let project = project(
            w,
            h,
            vec![
                media("red", "video", 4.0, "red.mp4"),
                media("portrait", "video", 4.0, "portrait_red.mp4"),
                media("fourthree", "video", 4.0, "four_three_blue.mp4"),
                media("green", "image", 1.0, "green.png"),
            ],
            tracks,
        );
        render_frame(&project, &resolver(dir.path()), secs(at), w, h).unwrap()
    };
    let is = |p: [u8; 4], rgb: [u8; 3], tolerance: u8| near(p[0], rgb[0], tolerance) && near(p[1], rgb[1], tolerance) && near(p[2], rgb[2], tolerance);

    // Portrait video is shown whole in a landscape frame: red in the middle, black at the sides.
    let frame = frame_of(vec![track("V1", "video", vec![clip("c", "V1", "portrait", 0.0, 2.0, json!({}))])], 0.5);
    assert!(is(pixel(&frame, w, 320, 180), [255, 0, 0], 40));
    assert!(is(pixel(&frame, w, 10, 180), [0, 0, 0], 20), "contain leaves the sides black: {:?}", pixel(&frame, w, 10, 180));

    // A 4:3 video fills the frame (cover), corners included.
    let frame = frame_of(vec![track("V1", "video", vec![clip("c", "V1", "fourthree", 0.0, 2.0, json!({}))])], 0.5);
    for (x, y) in [(2, 2), (637, 2), (2, 357), (637, 357), (320, 180)] {
        assert!(is(pixel(&frame, w, x, y), [0, 0, 255], 40), "({x},{y}) {:?}", pixel(&frame, w, x, y));
    }

    // Position + scale: a 640x360 still at 0.25 is 160x90, centred on (160, 90).
    let frame = frame_of(
        vec![track("V1", "video", vec![clip("c", "V1", "green", 0.0, 2.0, json!({ "scale": 0.25, "position": { "x": 160, "y": 90 } }))])],
        0.5,
    );
    assert!(is(pixel(&frame, w, 160, 90), [0, 128, 0], 40));
    assert!(is(pixel(&frame, w, 100, 60), [0, 128, 0], 40));
    assert!(is(pixel(&frame, w, 400, 250), [0, 0, 0], 10));
    assert!(is(pixel(&frame, w, 160, 150), [0, 0, 0], 10), "below the 90px-high still");

    // Opacity 0.5 of red over black is about half red.
    let frame = frame_of(vec![track("V1", "video", vec![clip("c", "V1", "red", 0.0, 2.0, json!({ "opacity": 0.5 }))])], 0.5);
    assert!(near(pixel(&frame, w, 320, 180)[0], 128, 12), "{:?}", pixel(&frame, w, 320, 180));

    // The upper track covers the lower one.
    let frame = frame_of(
        vec![track("V1", "video", vec![clip("top", "V1", "red", 0.0, 2.0, json!({}))]), track("V2", "video", vec![clip("below", "V2", "fourthree", 0.0, 2.0, json!({}))])],
        0.5,
    );
    assert!(is(pixel(&frame, w, 320, 180), [255, 0, 0], 40));

    // A fade-in of 1s is half way at 0.5s, also when the window starts inside the fade.
    let frame = frame_of(vec![track("V1", "video", vec![clip("c", "V1", "red", 0.0, 2.0, json!({ "fadeInSeconds": 1.0 }))])], 0.5);
    assert!(near(pixel(&frame, w, 320, 180)[0], 128, 20), "fade at 0.5s: {:?}", pixel(&frame, w, 320, 180));
    let frame = frame_of(vec![track("V1", "video", vec![clip("c", "V1", "red", 0.0, 2.0, json!({ "fadeInSeconds": 1.0 }))])], 1.5);
    assert!(near(pixel(&frame, w, 320, 180)[0], 255, 8), "after the fade: {:?}", pixel(&frame, w, 320, 180));

    // An empty spot on the timeline is a black frame (and a frame past the end renders too).
    let frame = frame_of(vec![track("V1", "video", vec![clip("c", "V1", "red", 0.0, 1.0, json!({}))])], 2.5);
    assert!(is(pixel(&frame, w, 320, 180), [0, 0, 0], 5));
}

#[test]
fn an_existing_output_survives_cancel_failure_and_an_interrupted_run() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let output = dir.path().join("keep.mp4");
    let original = b"original bytes that must not change".to_vec();
    std::fs::write(&output, &original).unwrap();
    let unchanged = || assert_eq!(std::fs::read(&output).unwrap(), original);
    let partial = dir.path().join(".keep.mp4.partial.mp4");

    // (a) cancelled before it starts.
    let token = CancelToken::new();
    token.cancel();
    let mut options = ExportOptions::new(&output);
    options.cancel = Some(token);
    let error = export(&full_project(), &resolver(dir.path()), options).unwrap_err();
    assert_eq!(error, RenderError::Cancelled);
    unchanged();
    assert!(!partial.exists());

    // (b) the media is missing.
    let missing = |_: &str| -> Result<std::path::PathBuf, String> { Err("gone".to_string()) };
    let error = export(&full_project(), &missing, ExportOptions::new(&output)).unwrap_err();
    assert!(matches!(error, RenderError::MediaUnavailable { .. }), "{error}");
    unchanged();

    // (c) ffmpeg itself fails: an unreadable file behind a valid-looking source.
    std::fs::write(dir.path().join("broken.mp4"), b"not a video").unwrap();
    let broken = project(640, 360, vec![media("v", "video", 4.0, "broken.mp4")], vec![track("V1", "video", vec![clip("c", "V1", "v", 0.0, 2.0, json!({}))])]);
    let error = export(&broken, &resolver(dir.path()), ExportOptions::new(&output)).unwrap_err();
    assert!(matches!(error, RenderError::FfmpegFailed { .. }), "{error}");
    unchanged();
    assert!(!partial.exists());

    // (d) cancelled while ffmpeg is running (the first progress report triggers it).
    let long = project(
        1280,
        720,
        vec![media("av", "video", 4.0, "av.mp4")],
        vec![track("V1", "video", (0..30).map(|i| clip(&format!("c{i}"), "V1", "av", f64::from(i) * 4.0, 4.0, json!({}))).collect())],
    );
    let token = CancelToken::new();
    let trigger = token.clone();
    let mut options = ExportOptions::new(&output);
    options.cancel = Some(token);
    options.on_progress = Some(Box::new(move |_| trigger.cancel()));
    let began = std::time::Instant::now();
    let error = export(&long, &resolver(dir.path()), options).unwrap_err();
    assert_eq!(error, RenderError::Cancelled);
    assert!(began.elapsed().as_secs() < 30, "cancel must stop ffmpeg promptly");
    unchanged();
    assert!(!partial.exists());
}

#[test]
fn export_checks_the_output_path_only() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let resolver = resolver(dir.path());
    let project = full_project();
    for bad in ["relative.mp4".into(), dir.path().join("x.mov"), dir.path().join("no-such-dir/x.mp4")] {
        let error = export(&project, &resolver, ExportOptions::new(bad)).unwrap_err();
        assert_eq!(error.code(), "INVALID_OUTPUT_PATH", "{error}");
    }
    let mut options = ExportOptions::new(dir.path().join("exists.mp4"));
    std::fs::write(&options.output, b"x").unwrap();
    options.overwrite = false;
    assert_eq!(export(&project, &resolver, options).unwrap_err().code(), "INVALID_OUTPUT_PATH");
}

#[test]
fn output_must_resolve_inside_an_allowed_root() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let allowed = vec![root.path().to_path_buf()];
    assert!(avs_render::check_output_within(&root.path().join("a.mp4"), &allowed).is_ok());
    let nested = root.path().join("sub");
    std::fs::create_dir(&nested).unwrap();
    assert!(avs_render::check_output_within(&nested.join("a.mp4"), &allowed).is_ok());
    // A folder inside the root that is really a link to somewhere else.
    std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
    assert!(avs_render::check_output_within(&root.path().join("link").join("a.mp4"), &allowed).is_err());
    // A file inside the root that is a link out.
    std::os::unix::fs::symlink(outside.path().join("target.mp4"), root.path().join("escape.mp4")).unwrap();
    std::fs::write(outside.path().join("target.mp4"), b"x").unwrap();
    assert!(avs_render::check_output_within(&root.path().join("escape.mp4"), &allowed).is_err());
    assert!(avs_render::check_output_within(&outside.path().join("a.mp4"), &allowed).is_err());
}

#[test]
fn preview_lines_picture_and_sound_up_with_the_timeline() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    // Playback starts at 1.0s; the red picture and the tone both begin at 1.5s.
    let project = project(
        640,
        360,
        vec![media("red", "video", 4.0, "red.mp4"), media("tone", "audio", 4.0, "tone.wav")],
        vec![
            track("V1", "video", vec![clip("v", "V1", "red", 1.5, 1.0, json!({}))]),
            track("A1", "audio", vec![clip("t", "A1", "tone", 1.5, 1.0, json!({}))]),
        ],
    );
    let from = secs(1.0);
    let stream = PreviewStream::start(&project, &resolver(dir.path()), PreviewOptions { from, width: 320, height: 180, fps: 30.0, sample_rate: 48_000, channels: 2 }).expect("start");
    // The audio reader goes to its own thread, as a player would do it.
    let PreviewStream { video, mut audio, .. } = stream;
    let audio_thread = std::thread::spawn(move || {
        let mut samples = vec![0f32; 48_000 + 9_600];
        let mut got = 0;
        while got < samples.len() {
            let read = audio.read_samples(&mut samples[got..]).expect("audio");
            assert!(read > 0, "audio ended early after {got} samples");
            got += read;
        }
        samples
    });

    let mut previous: Option<Time> = None;
    for index in 0..22u32 {
        let frame = video.recv_timeout(std::time::Duration::from_secs(20)).unwrap_or_else(|_| panic!("frame {index} never came"));
        assert_eq!(frame.rgba.len(), 320 * 180 * 4);
        let expected = from + Time::from_micros((f64::from(index) * 1_000_000.0 / 30.0).round() as i64);
        assert_eq!(frame.pts, expected);
        if let Some(previous) = previous {
            let step = (frame.pts - previous).micros();
            assert!((step - 33_333).abs() <= 1, "step {step}");
        }
        previous = Some(frame.pts);
        // 15 frames (0.5s) of black, then the red picture.
        let centre = pixel(&frame.rgba, 320, 160, 90);
        if index < 15 {
            assert!(centre[0] < 10, "frame {index} (t={}) should still be black: {centre:?}", frame.pts);
        } else {
            assert!(centre[0] > 240 && centre[1] < 20, "frame {index} (t={}) should be red: {centre:?}", frame.pts);
        }
    }

    let samples = audio_thread.join().expect("audio thread");
    let peak = |range: std::ops::Range<usize>| samples[range].iter().fold(0f32, |peak, sample| peak.max(sample.abs()));
    // 0.5s of silence (stereo: two values per frame), then the tone.
    assert!(peak(0..47_000) < 1e-3, "silent until 1.5s: {}", peak(0..47_000));
    let tone = peak(49_000..57_600);
    assert!(tone > 0.05 && tone <= 1.0, "tone peak {tone}");
    // Dropping what is left of the stream stops both processes without hanging.
    let began = std::time::Instant::now();
    drop(video);
    assert!(began.elapsed().as_secs() < 5);
}

#[test]
fn preview_ends_cleanly_and_a_start_past_the_end_is_an_error() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let project = project(640, 360, vec![media("red", "video", 4.0, "red.mp4")], vec![track("V1", "video", vec![clip("v", "V1", "red", 0.0, 1.0, json!({}))])]);
    let mut stream = PreviewStream::start(&project, &resolver(dir.path()), PreviewOptions { from: secs(0.5), width: 160, height: 90, fps: 30.0, sample_rate: 48_000, channels: 1 }).unwrap();
    let frames = std::iter::from_fn(|| stream.video.recv_timeout(std::time::Duration::from_secs(20)).ok()).count();
    assert_eq!(frames, 15, "0.5s at 30 fps");
    let mut rest = Vec::new();
    stream.audio.read_to_end(&mut rest).unwrap();
    assert_eq!(rest.len(), 24_000 * 4, "0.5s of mono 48 kHz f32");
    drop(stream);
    let error = PreviewStream::start(&project, &resolver(dir.path()), PreviewOptions { from: secs(2.0), width: 160, height: 90, fps: 30.0, sample_rate: 48_000, channels: 2 }).err().expect("an error");
    assert_eq!(error.code(), "EMPTY_RENDER_RANGE");
}

#[test]
fn preview_scales_the_same_composition_down() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    // The 90px-high still sits at the centre of its 160x90 slot at both sizes.
    let project = project(
        640,
        360,
        vec![media("green", "image", 1.0, "green.png")],
        vec![track("V1", "video", vec![clip("c", "V1", "green", 0.0, 2.0, json!({ "scale": 0.25, "position": { "x": 160, "y": 90 } }))])],
    );
    let small = render_frame(&project, &resolver(dir.path()), secs(0.5), 320, 180).unwrap();
    assert!(near(pixel(&small, 320, 80, 45)[1], 128, 30));
    assert!(pixel(&small, 320, 200, 120)[..3] == [0, 0, 0]);
    assert!(pixel(&small, 320, 80, 80)[..3] == [0, 0, 0], "the still is 45px high at this size");
    // A different aspect ratio letterboxes instead of stretching.
    let wide = render_frame(&project, &resolver(dir.path()), secs(0.5), 400, 180).unwrap();
    assert_eq!(wide.len(), 400 * 180 * 4);
    let _ = VideoFormat::Rgba;
}

#[test]
fn every_clip_property_yields_a_runnable_graph_and_fades_survive_a_cut_window() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let busy = json!({
        "speed": 1.5, "fadeInSeconds": 0.3, "fadeOutSeconds": 0.3, "opacity": 0.8, "rotationDegrees": 12.5, "scale": 0.5,
        "position": { "x": 400, "y": 200 }, "crop": { "left": 0.1, "right": 0.1, "top": 0.05, "bottom": 0.05 }
    });
    let project = project(
        640,
        360,
        vec![media("av", "video", 4.0, "av.mp4"), media("tone", "audio", 4.0, "tone.wav"), media("green", "image", 1.0, "green.png")],
        vec![
            track("V2", "video", vec![clip("still", "V2", "green", 0.5, 1.0, json!({ "rotationDegrees": -20, "scale": 0.3, "fadeInSeconds": 0.2 }))]),
            track("V1", "video", vec![clip("busy", "V1", "av", 0.0, 2.0, busy)]),
            track("A1", "audio", vec![clip("slow", "A1", "tone", 0.0, 2.0, json!({ "speed": 0.4, "volumeDb": -3.0, "fadeInSeconds": 0.2, "fadeOutSeconds": 0.2 }))]),
        ],
    );
    let output = dir.path().join("busy.mp4");
    export(&project, &resolver(dir.path()), ExportOptions::new(&output)).expect("export with every property");
    let duration: f64 = ffprobe_json(&output)["format"]["duration"].as_str().unwrap().parse().unwrap();
    assert!((duration - 2.0).abs() < 0.1, "{duration}");
    // The same project exported from the middle (the window cuts into the fades).
    let mut options = ExportOptions::new(dir.path().join("cut.mp4"));
    options.start = secs(0.1);
    options.duration = Some(secs(1.0));
    export(&project, &resolver(dir.path()), options).expect("export from the middle");

    // A 2s audio fade-in, cut so the output starts 1s into it, begins half way up.
    let tone = |fade: f64| {
        common::project(
            640,
            360,
            vec![media("tone", "audio", 4.0, "tone.wav")],
            vec![track("A1", "audio", vec![clip("t", "A1", "tone", 0.0, 3.0, json!({ "fadeInSeconds": fade }))])],
        )
    };
    let render = |name: &str, fade: f64, start: f64| {
        let mut options = ExportOptions::new(dir.path().join(name));
        options.start = secs(start);
        options.duration = Some(secs(1.0));
        export(&tone(fade), &resolver(dir.path()), options).unwrap();
        dir.path().join(name)
    };
    let full = mean_volume(&render("full.mp4", 0.0, 1.0), 0.05, 0.25);
    let ramp = mean_volume(&render("ramp.mp4", 2.0, 1.0), 0.05, 0.25);
    eprintln!("full {full} dB, 2s fade-in cut at 1s {ramp} dB");
    // Gain between 1.05s and 1.25s of a 2s ramp is 0.525..0.625, i.e. about -5 dB.
    assert!((full - ramp - 4.8).abs() < 2.0, "full {full} ramp {ramp}");
}

#[test]
fn clip_mute_is_about_sound_and_track_mute_hides_the_track() {
    let Some(_) = toolchain() else { return };
    let dir = tempfile::tempdir().unwrap();
    make_fixtures(dir.path());
    let mut hidden = track("V2", "video", vec![clip("hidden", "V2", "red", 0.0, 1.0, json!({}))]);
    hidden["muted"] = json!(true);
    let project = project(
        640,
        360,
        vec![media("red", "video", 4.0, "red.mp4"), media("tone", "audio", 4.0, "tone.wav")],
        vec![
            track("V1", "video", vec![clip("shown", "V1", "red", 0.0, 1.0, json!({ "muted": true }))]),
            hidden,
            track("A1", "audio", vec![clip("silent", "A1", "tone", 0.0, 1.0, json!({ "muted": true }))]),
        ],
    );
    let graph = RenderGraph::build(&project, &resolver(dir.path()), &GraphOptions::for_project(&project)).unwrap();
    let used: Vec<&str> = graph.sources.iter().map(|source| source.clip_id.as_str()).collect();
    assert_eq!(used, ["shown"], "{used:?}");
}
