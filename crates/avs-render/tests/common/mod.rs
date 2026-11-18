//! Fixtures shared by the integration tests: media made with lavfi, projects built
//! from JSON, and small ffmpeg/ffprobe readers.
#![allow(dead_code)]

use avs_core::Project;
use avs_render::Toolchain;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The toolchain, or `None` (after saying why) so a test can skip itself.
pub fn toolchain() -> Option<&'static Toolchain> {
    match Toolchain::get() {
        Ok(toolchain) => Some(toolchain),
        Err(error) => {
            eprintln!("SKIPPED: ffmpeg is not usable here ({error})");
            None
        }
    }
}

pub fn ffmpeg(args: &[&str]) {
    let toolchain = Toolchain::get().expect("toolchain");
    let output = Command::new(&toolchain.ffmpeg).args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).output().expect("run ffmpeg");
    assert!(output.status.success(), "ffmpeg {args:?} failed: {}", String::from_utf8_lossy(&output.stderr));
}

/// Makes the media every test draws from, inside `dir`.
pub fn make_fixtures(dir: &Path) {
    let p = |name: &str| dir.join(name).to_string_lossy().into_owned();
    ffmpeg(&["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=4", "-ac", "2", &p("tone.wav")]);
    ffmpeg(&["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo", "-t", "4", &p("silence.wav")]);
    for (name, colour, size) in [("red.mp4", "red", "640x360"), ("blue.mp4", "blue", "640x360"), ("portrait_red.mp4", "red", "360x640"), ("four_three_blue.mp4", "blue", "320x240")] {
        ffmpeg(&["-f", "lavfi", "-i", &format!("color=c={colour}:s={size}:r=30:d=4"), "-pix_fmt", "yuv420p", &p(name)]);
    }
    ffmpeg(&["-f", "lavfi", "-i", "color=c=green:s=640x360:d=1", "-frames:v", "1", &p("green.png")]);
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc=s=640x360:r=30:d=4", "-f", "lavfi", "-i", "sine=frequency=330:sample_rate=48000:duration=4", "-pix_fmt", "yuv420p",
        "-shortest", &p("av.mp4"),
    ]);
}

pub fn resolver(dir: &Path) -> impl Fn(&str) -> Result<PathBuf, String> + use<> {
    let dir = dir.to_path_buf();
    move |source: &str| {
        let path = dir.join(source);
        if path.is_file() { Ok(path) } else { Err(format!("no fixture named {source}")) }
    }
}

pub fn media(id: &str, kind: &str, duration: f64, source: &str) -> Value {
    json!({ "id": id, "name": id, "type": kind, "durationSeconds": duration, "source": source })
}

pub fn clip(id: &str, track: &str, media: &str, start: f64, duration: f64, properties: Value) -> Value {
    json!({
        "id": id, "trackId": track, "kind": "media", "name": id, "startSeconds": start, "durationSeconds": duration,
        "mediaId": media, "sourceStartSeconds": 0, "properties": properties
    })
}

pub fn caption(id: &str, track: &str, start: f64, duration: f64, text: &str, style: Value) -> Value {
    json!({
        "id": id, "trackId": track, "kind": "caption", "name": id, "startSeconds": start, "durationSeconds": duration,
        "properties": { "captionStyle": style }, "caption": { "text": text }
    })
}

pub fn track(id: &str, kind: &str, clips: Vec<Value>) -> Value {
    json!({ "id": id, "name": id, "kind": kind, "clips": clips })
}

pub fn project(width: u32, height: u32, media: Vec<Value>, tracks: Vec<Value>) -> Project {
    serde_json::from_value(json!({
        "id": "p", "title": "test", "fps": 30, "resolution": { "width": width, "height": height }, "sampleRate": 48000,
        "media": media, "tracks": tracks
    }))
    .expect("project json")
}

pub fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let index = ((y * width + x) * 4) as usize;
    [rgba[index], rgba[index + 1], rgba[index + 2], rgba[index + 3]]
}

pub fn ffprobe_json(path: &Path) -> Value {
    let toolchain = Toolchain::get().expect("toolchain");
    let output = Command::new(&toolchain.ffprobe)
        .args(["-v", "error", "-show_format", "-show_streams", "-of", "json"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(output.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).expect("ffprobe json")
}

/// `mean_volume` in dB from ffmpeg's volumedetect, over `[from, to]` seconds of the audio.
pub fn mean_volume(path: &Path, from: f64, to: f64) -> f64 {
    let toolchain = Toolchain::get().expect("toolchain");
    let filter = format!("atrim={from}:{to},volumedetect");
    let output = Command::new(&toolchain.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(path)
        .args(["-vn", "-af", &filter, "-f", "null", "-"])
        .output()
        .expect("run ffmpeg");
    let text = String::from_utf8_lossy(&output.stderr);
    let line = text.lines().find(|line| line.contains("mean_volume")).unwrap_or_else(|| panic!("no mean_volume in:\n{text}"));
    let value = line.split("mean_volume:").nth(1).expect("value").trim().trim_end_matches(" dB");
    value.parse().expect("number")
}
