//! Project invariants, checked after every edit and on load.
//!
//! An edit that would leave any `Error` issue is rejected by `ops::apply`, so a
//! saved project never contains one.

use crate::model::{ClipKind, MediaType, Project, TrackKind};
use crate::time::Time;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Validation {
    pub ok: bool,
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
    pub issues: Vec<Issue>,
}

pub const MIN_VOLUME_DB: f64 = -96.0;
pub const MAX_VOLUME_DB: f64 = 24.0;
pub const MIN_SPEED: f64 = 0.1;
pub const MAX_SPEED: f64 = 10.0;

struct Collector(Vec<Issue>);

impl Collector {
    fn push(&mut self, severity: Severity, code: &'static str, message: String, track: Option<&str>, clip: Option<&str>, media: Option<&str>) {
        self.0.push(Issue {
            severity,
            code,
            message,
            track_id: track.map(str::to_string),
            clip_id: clip.map(str::to_string),
            media_id: media.map(str::to_string),
        });
    }

    fn error(&mut self, code: &'static str, message: String, track: Option<&str>, clip: Option<&str>, media: Option<&str>) {
        self.push(Severity::Error, code, message, track, clip, media);
    }
}

/// Whether media of this type may sit on a track of this kind. A video file on an
/// audio track plays its sound only; that holds whether or not the clip is still
/// linked to its picture, so unlinking never makes a project invalid.
pub fn media_fits_track(media: MediaType, track: TrackKind) -> bool {
    match track {
        TrackKind::Video => matches!(media, MediaType::Video | MediaType::Image | MediaType::Sequence),
        TrackKind::Audio => matches!(media, MediaType::Audio | MediaType::Video),
        TrackKind::Caption => false,
    }
}

pub fn issues(project: &Project) -> Vec<Issue> {
    let mut out = Collector(Vec::new());

    if !(project.fps.is_finite() && project.fps > 0.0) {
        out.error("INVALID_FPS", format!("Project fps must be positive, got {}.", project.fps), None, None, None);
    }
    if project.resolution.width == 0 || project.resolution.height == 0 {
        out.error("INVALID_RESOLUTION", "Project resolution must be non-zero.".into(), None, None, None);
    }

    let mut media_ids = HashSet::new();
    for media in &project.media {
        if !media_ids.insert(media.id.as_str()) {
            out.error("DUPLICATE_MEDIA_ID", format!("Media id {} is used more than once.", media.id), None, None, Some(&media.id));
        }
        if !media.duration_seconds.is_positive() {
            out.error("INVALID_MEDIA_DURATION", format!("Media {} must have a positive duration.", media.id), None, None, Some(&media.id));
        }
    }

    let mut track_ids = HashSet::new();
    let mut clip_ids = HashSet::new();
    let mut groups: HashMap<&str, Vec<(&str, &crate::model::Clip)>> = HashMap::new();

    for track in &project.tracks {
        let track_id = track.id.as_str();
        if !track_ids.insert(track_id) {
            out.error("DUPLICATE_TRACK_ID", format!("Track id {track_id} is used more than once."), Some(track_id), None, None);
        }

        let mut furthest_end: Option<(Time, &str)> = None;
        let mut previous_start: Option<Time> = None;
        for clip in &track.clips {
            let clip_id = clip.id.as_str();
            if !clip_ids.insert(clip_id) {
                out.error("DUPLICATE_CLIP_ID", format!("Clip id {clip_id} is used more than once."), Some(track_id), Some(clip_id), None);
            }
            if clip.track_id != track.id {
                out.error("CLIP_TRACK_MISMATCH", format!("Clip {clip_id} records track {} but sits on {track_id}.", clip.track_id), Some(track_id), Some(clip_id), None);
            }
            if clip.start_seconds.is_negative() {
                out.error("NEGATIVE_CLIP_START", format!("Clip {clip_id} starts before 0."), Some(track_id), Some(clip_id), None);
            }
            if !clip.duration_seconds.is_positive() {
                out.error("INVALID_CLIP_DURATION", format!("Clip {clip_id} must have a positive duration."), Some(track_id), Some(clip_id), None);
            }
            if previous_start.is_some_and(|previous| clip.start_seconds < previous) {
                out.error("CLIPS_NOT_SORTED", format!("Clip {clip_id} is out of start order on track {track_id}."), Some(track_id), Some(clip_id), None);
            }
            previous_start = Some(clip.start_seconds);
            // Compare against the furthest end seen so far, not just the neighbour, so a
            // long clip overlapping several later ones is reported for each of them.
            if let Some((end, other)) = furthest_end {
                if clip.start_seconds < end {
                    out.error("CLIP_OVERLAP", format!("Clip {clip_id} overlaps {other} on track {track_id}."), Some(track_id), Some(clip_id), None);
                }
            }
            if furthest_end.is_none_or(|(end, _)| clip.end() > end) {
                furthest_end = Some((clip.end(), clip_id));
            }

            if clip.fade_in().is_negative() || clip.fade_out().is_negative() {
                out.error("INVALID_FADE", format!("Clip {clip_id} has a negative fade."), Some(track_id), Some(clip_id), None);
            } else if clip.fade_in() + clip.fade_out() > clip.duration_seconds {
                out.error("CLIP_FADE_EXCEEDS_DURATION", format!("Fades of clip {clip_id} are longer than the clip."), Some(track_id), Some(clip_id), None);
            }
            if let Some(speed) = clip.properties.speed {
                if !(speed.is_finite() && (MIN_SPEED..=MAX_SPEED).contains(&speed)) {
                    out.error("INVALID_SPEED", format!("Clip {clip_id} speed must be between {MIN_SPEED} and {MAX_SPEED}."), Some(track_id), Some(clip_id), None);
                }
            }
            if let Some(volume) = clip.properties.volume_db {
                if !(volume.is_finite() && (MIN_VOLUME_DB..=MAX_VOLUME_DB).contains(&volume)) {
                    out.error("INVALID_AUDIO_VOLUME", format!("Clip {clip_id} volume must be between {MIN_VOLUME_DB} and {MAX_VOLUME_DB} dB."), Some(track_id), Some(clip_id), None);
                }
            }
            if let Some(opacity) = clip.properties.opacity {
                if !(opacity.is_finite() && (0.0..=1.0).contains(&opacity)) {
                    out.error("INVALID_OPACITY", format!("Clip {clip_id} opacity must be between 0 and 1."), Some(track_id), Some(clip_id), None);
                }
            }
            if let Some(scale) = clip.properties.scale {
                if !(scale.is_finite() && scale > 0.0) {
                    out.error("INVALID_SCALE", format!("Clip {clip_id} scale must be positive."), Some(track_id), Some(clip_id), None);
                }
            }
            if let Some(crop) = clip.properties.crop {
                let sides = [crop.left, crop.right, crop.top, crop.bottom];
                let in_range = sides.iter().all(|side| side.is_finite() && (0.0..1.0).contains(side));
                if !in_range || crop.left + crop.right >= 1.0 || crop.top + crop.bottom >= 1.0 {
                    out.error("INVALID_CROP", format!("Clip {clip_id} crop must leave a visible area."), Some(track_id), Some(clip_id), None);
                }
            }

            match clip.kind {
                ClipKind::Caption => {
                    if track.kind != TrackKind::Caption {
                        out.error("INCOMPATIBLE_CLIP_TRACK", format!("Caption clip {clip_id} is on {:?} track {track_id}.", track.kind), Some(track_id), Some(clip_id), None);
                    }
                    if clip.caption.is_none() {
                        out.error("CAPTION_TEXT_MISSING", format!("Caption clip {clip_id} has no text."), Some(track_id), Some(clip_id), None);
                    }
                }
                ClipKind::Media => {
                    let Some(media_id) = clip.media_id.as_deref() else {
                        out.error("CLIP_MEDIA_MISSING", format!("Media clip {clip_id} names no media."), Some(track_id), Some(clip_id), None);
                        continue;
                    };
                    let Some(media) = project.media.iter().find(|media| media.id == media_id) else {
                        out.error("MEDIA_NOT_FOUND", format!("Clip {clip_id} refers to unknown media {media_id}."), Some(track_id), Some(clip_id), Some(media_id));
                        continue;
                    };
                    if !media_fits_track(media.media_type, track.kind) {
                        out.error("INCOMPATIBLE_MEDIA_TRACK", format!("{:?} media {media_id} cannot sit on {:?} track {track_id}.", media.media_type, track.kind), Some(track_id), Some(clip_id), Some(media_id));
                    }
                    if !media.media_type.is_still() {
                        let source_start = clip.source_start();
                        let source_end = source_start + clip.duration_seconds.scale(clip.speed());
                        // One microsecond of slack absorbs the rounding of duration x speed.
                        if source_start.is_negative() || source_end > media.duration_seconds + Time::from_micros(1) {
                            out.error("MEDIA_RANGE_EXCEEDED", format!("Clip {clip_id} uses {source_start}..{source_end}s of media {media_id}, which is {}s long.", media.duration_seconds), Some(track_id), Some(clip_id), Some(media_id));
                        }
                    }
                }
            }

            if let Some(link) = &clip.link {
                groups.entry(link.group_id.as_str()).or_default().push((track_id, clip));
            }
        }
    }

    for (group_id, members) in groups {
        let first = members[0].1.id.as_str();
        if members.len() != 2 {
            out.error("BROKEN_LINKED_CLIP", format!("Link group {group_id} has {} member(s); it needs exactly two.", members.len()), Some(members[0].0), Some(first), None);
            continue;
        }
        let (a, b) = (members[0].1, members[1].1);
        let roles = (a.link.as_ref().map(|l| l.role), b.link.as_ref().map(|l| l.role));
        if roles.0 == roles.1 {
            out.error("BROKEN_LINKED_CLIP", format!("Link group {group_id} needs one video and one audio member."), Some(members[0].0), Some(first), None);
        }
        let in_step = a.start_seconds == b.start_seconds
            && a.duration_seconds == b.duration_seconds
            && a.source_start() == b.source_start()
            && a.speed() == b.speed()
            && a.media_id == b.media_id;
        if !in_step {
            out.error("LINKED_CLIPS_OUT_OF_SYNC", format!("Linked clips {} and {} differ in timing, speed or media.", a.id, b.id), Some(members[0].0), Some(first), None);
        }
    }

    out.0
}

pub fn validate(project: &Project) -> Validation {
    let issues = issues(project);
    let errors: Vec<Issue> = issues.iter().filter(|issue| issue.severity == Severity::Error).cloned().collect();
    let warnings: Vec<Issue> = issues.iter().filter(|issue| issue.severity == Severity::Warning).cloned().collect();
    Validation { ok: errors.is_empty(), errors, warnings, issues }
}
