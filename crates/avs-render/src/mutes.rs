//! CodexVideo handoff audio metadata: which stretches of the background music
//! (BGM) must be silent. The meaning is the one the earlier implementation had:
//!
//! - Only BGM clips are ever muted; narration, source audio and the sound of a
//!   video file are untouched.
//! - `mutedIntervals` are read from the project metadata (`codexVideo.audio`,
//!   `codexVideo`, `audio`, the metadata root) and, for the clip itself, from the
//!   clip and media metadata (`codexVideo.playback`, `codexVideo.audio`, ...).
//! - A container that says `sourceAudioPreserved: false` (and no other container says
//!   `true`) cancels the intervals read from that group.
//! - An interval may name its target (`target`, `appliesTo`, ...); one that names
//!   something other than BGM is skipped, and so is one whose `role` is a
//!   non-BGM audio role. An interval with no target at all applies to BGM.

use avs_core::{Clip, MediaAsset, MediaType, Project, Time};
use serde_json::Value;

type Record = serde_json::Map<String, Value>;

/// A closed-open stretch of project time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuteInterval {
    pub start: Time,
    pub end: Time,
}

fn field<'a>(record: Option<&'a Record>, key: &str) -> Option<&'a Record> {
    record?.get(key)?.as_object()
}

fn text<'a>(record: &'a Record, key: &str) -> Option<&'a str> {
    record.get(key)?.as_str().map(str::trim).filter(|value| !value.is_empty())
}

fn number(record: &Record, key: &str) -> Option<f64> {
    record.get(key)?.as_f64().filter(|value| value.is_finite())
}

fn is_bgm_role(value: &str) -> bool {
    matches!(value.trim().to_lowercase().as_str(), "bgm" | "background-music" | "background_music" | "music-bed" | "music_bed")
}

fn is_non_bgm_role(value: &str) -> bool {
    matches!(
        value.trim().to_lowercase().as_str(),
        "dialogue" | "embedded-audio" | "linked-audio" | "narration" | "source-audio" | "voice-over" | "voiceover"
    )
}

/// Reads `sourceAudioPreserved` across a group of containers and, unless the group
/// is explicitly switched off, collects their BGM mute intervals.
fn read_intervals(containers: &[&Record]) -> Vec<MuteInterval> {
    let disabled = containers.iter().any(|c| c.get("sourceAudioPreserved") == Some(&Value::Bool(false)));
    let enabled = containers.iter().any(|c| c.get("sourceAudioPreserved") == Some(&Value::Bool(true)));
    if disabled && !enabled {
        return Vec::new();
    }
    containers
        .iter()
        .filter_map(|container| container.get("mutedIntervals")?.as_array())
        .flatten()
        .filter_map(|value| parse_interval(value.as_object()?))
        .collect()
}

fn parse_interval(interval: &Record) -> Option<MuteInterval> {
    let target = ["target", "appliesTo", "audioTarget", "targetRole", "audioRole", "mixRole", "sourceRole"]
        .iter()
        .find_map(|key| text(interval, key));
    let targets_bgm = match target {
        Some(target) => is_bgm_role(target),
        None => match text(interval, "role") {
            Some(role) if is_bgm_role(role) => true,
            Some(role) => !is_non_bgm_role(role),
            None => true,
        },
    };
    if !targets_bgm {
        return None;
    }
    let first = |keys: &[&str]| keys.iter().find_map(|key| number(interval, key));
    let start = first(&["startSeconds", "startSec", "start"])?;
    let end = first(&["endSeconds", "endSec", "end"])
        .or_else(|| first(&["durationSeconds", "durationSec", "duration"]).filter(|duration| *duration > 0.0).map(|duration| start + duration))?;
    if end <= start {
        return None;
    }
    Some(MuteInterval { start: Time::from_secs(start)?, end: Time::from_secs(end)? })
}

fn project_containers(project: &Project) -> Vec<&Record> {
    let metadata: Option<&Record> = project.metadata.as_ref();
    let codex_video = field(metadata, "codexVideo");
    [field(codex_video, "audio"), codex_video, field(metadata, "audio"), metadata].into_iter().flatten().collect()
}

fn source_containers<'a>(clip: &'a Clip, media: &'a MediaAsset) -> Vec<&'a Record> {
    let clip_meta: Option<&Record> = clip.properties.metadata.as_ref();
    let media_meta: Option<&Record> = media.metadata.as_ref();
    let clip_cv = field(clip_meta, "codexVideo");
    let media_cv = field(media_meta, "codexVideo");
    [
        field(clip_cv, "playback"),
        field(clip_cv, "audio"),
        clip_cv,
        field(clip_meta, "audio"),
        clip_meta,
        field(media_cv, "playback"),
        field(media_cv, "audio"),
        media_cv,
        field(media_meta, "audio"),
        media_meta,
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// True when this audio clip is background music. The sound of a video file is never
/// BGM, whatever its metadata says.
pub fn is_bgm_clip(clip: &Clip, media: &MediaAsset) -> bool {
    if media.media_type == MediaType::Video {
        return false;
    }
    let clip_meta: Option<&Record> = clip.properties.metadata.as_ref();
    let media_meta: Option<&Record> = media.metadata.as_ref();
    let candidates: [Option<&Record>; 6] = [
        clip_meta,
        field(clip_meta, "codexVideo"),
        field(clip_meta, "audio"),
        media_meta,
        field(media_meta, "codexVideo"),
        field(media_meta, "audio"),
    ];
    candidates.into_iter().flatten().any(|record| {
        ["audioRole", "mixRole", "sourceRole", "role"].iter().find_map(|key| text(record, key)).is_some_and(is_bgm_role)
    })
}

fn dedupe(intervals: Vec<MuteInterval>) -> Vec<MuteInterval> {
    let mut seen: Vec<MuteInterval> = Vec::new();
    for interval in intervals {
        if !seen.contains(&interval) {
            seen.push(interval);
        }
    }
    seen
}

/// The project-time stretches in which this clip must be silent (empty unless it is BGM).
pub fn mute_intervals_for(project: &Project, clip: &Clip, media: &MediaAsset) -> Vec<MuteInterval> {
    if !is_bgm_clip(clip, media) {
        return Vec::new();
    }
    let mut intervals = read_intervals(&project_containers(project));
    intervals.extend(read_intervals(&source_containers(clip, media)));
    dedupe(intervals)
}

