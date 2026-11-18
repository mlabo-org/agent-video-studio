//! The project document: media, tracks, clips.
//!
//! The JSON shape (camelCase, seconds as floats) is the product's public timeline
//! schema. Session files written by the earlier TypeScript implementation load
//! directly; [`Project::adopt_legacy_fields`] moves the two things that changed
//! (clip links and the embedded tool history) to their new homes.

use crate::error::{EditError, Res};
use crate::time::Time;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub type Metadata = Map<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    Video,
    Audio,
    Image,
    Sequence,
}

impl MediaType {
    /// Still media has no intrinsic length: a clip may show it for any duration.
    pub fn is_still(self) -> bool {
        matches!(self, MediaType::Image | MediaType::Sequence)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
    Caption,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClipKind {
    Media,
    Caption,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditMode {
    /// Reject the edit when the target range is occupied.
    #[default]
    Strict,
    /// Open a gap at the target position, pushing later clips on that track right.
    Insert,
    /// Remove whatever occupies exactly the range each placed clip covers.
    Overwrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Start,
    End,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaAsset {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub media_type: MediaType,
    pub duration_seconds: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipProperties {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_degrees: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<Crop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_in_seconds: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_out_seconds: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption_style: Option<CaptionStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionText {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkRole {
    Video,
    Audio,
}

/// Ties a video clip to the clip that carries its embedded audio. The two members
/// of a group always share start, duration, source start and speed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipLink {
    pub group_id: String,
    pub role: LinkRole,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub id: String,
    pub track_id: String,
    pub kind: ClipKind,
    pub name: String,
    pub start_seconds: Time,
    pub duration_seconds: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_start_seconds: Option<Time>,
    #[serde(default)]
    pub properties: ClipProperties,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<CaptionText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<ClipLink>,
}

impl Clip {
    pub fn end(&self) -> Time {
        self.start_seconds + self.duration_seconds
    }

    pub fn source_start(&self) -> Time {
        self.source_start_seconds.unwrap_or(Time::ZERO)
    }

    /// Playback speed; anything missing or not a positive finite number means 1.
    pub fn speed(&self) -> f64 {
        match self.properties.speed {
            Some(speed) if speed.is_finite() && speed > 0.0 => speed,
            _ => 1.0,
        }
    }

    pub fn fade_in(&self) -> Time {
        self.properties.fade_in_seconds.unwrap_or(Time::ZERO)
    }

    pub fn fade_out(&self) -> Time {
        self.properties.fade_out_seconds.unwrap_or(Time::ZERO)
    }

    pub fn contains(&self, at: Time) -> bool {
        self.start_seconds < at && at < self.end()
    }

    pub fn intersects(&self, start: Time, end: Time) -> bool {
        self.start_seconds < end && start < self.end()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: String,
    pub name: String,
    pub kind: TrackKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
    #[serde(default)]
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn is_locked(&self) -> bool {
        self.locked.unwrap_or(false)
    }

    pub fn is_muted(&self) -> bool {
        self.muted.unwrap_or(false)
    }

    pub fn end(&self) -> Time {
        self.clips.iter().map(Clip::end).max().unwrap_or(Time::ZERO)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub title: String,
    pub fps: f64,
    pub resolution: Resolution,
    pub sample_rate: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playhead_seconds: Option<Time>,
    #[serde(default)]
    pub media: Vec<MediaAsset>,
    #[serde(default)]
    pub tracks: Vec<Track>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
    /// Read from files written by the TypeScript implementation and never written
    /// back: the activity log now lives beside the project, not inside it.
    #[serde(default, rename = "toolHistory", skip_serializing)]
    pub legacy_tool_history: Vec<Value>,
}

impl Project {
    /// An empty project with one video, one audio and one caption track.
    pub fn empty(id: impl Into<String>, title: impl Into<String>) -> Self {
        let track = |id: &str, name: &str, kind| Track {
            id: id.into(),
            name: name.into(),
            kind,
            locked: None,
            muted: None,
            clips: Vec::new(),
        };
        Project {
            id: id.into(),
            title: title.into(),
            fps: 30.0,
            resolution: Resolution { width: 1920, height: 1080 },
            sample_rate: 48_000,
            playhead_seconds: None,
            media: Vec::new(),
            tracks: vec![
                track("V1", "Video 1", TrackKind::Video),
                track("A1", "Audio 1", TrackKind::Audio),
                track("C1", "Captions 1", TrackKind::Caption),
            ],
            metadata: None,
            legacy_tool_history: Vec::new(),
        }
    }

    pub fn duration(&self) -> Time {
        self.tracks.iter().map(Track::end).max().unwrap_or(Time::ZERO)
    }

    pub fn track_index(&self, track_id: &str) -> Res<usize> {
        self.tracks
            .iter()
            .position(|track| track.id == track_id)
            .ok_or_else(|| EditError::new("TRACK_NOT_FOUND", format!("No track with id {track_id}.")).with("trackId", track_id))
    }

    pub fn track(&self, track_id: &str) -> Res<&Track> {
        Ok(&self.tracks[self.track_index(track_id)?])
    }

    /// Position of a clip as (track index, clip index).
    pub fn locate(&self, clip_id: &str) -> Res<(usize, usize)> {
        for (track_index, track) in self.tracks.iter().enumerate() {
            if let Some(clip_index) = track.clips.iter().position(|clip| clip.id == clip_id) {
                return Ok((track_index, clip_index));
            }
        }
        Err(EditError::new("CLIP_NOT_FOUND", format!("No clip with id {clip_id}.")).with("clipId", clip_id))
    }

    pub fn clip(&self, clip_id: &str) -> Res<&Clip> {
        let (track_index, clip_index) = self.locate(clip_id)?;
        Ok(&self.tracks[track_index].clips[clip_index])
    }

    pub fn clips(&self) -> impl Iterator<Item = &Clip> {
        self.tracks.iter().flat_map(|track| track.clips.iter())
    }

    pub fn media(&self, media_id: &str) -> Res<&MediaAsset> {
        self.media
            .iter()
            .find(|media| media.id == media_id)
            .ok_or_else(|| EditError::new("MEDIA_NOT_FOUND", format!("No media with id {media_id}.")).with("mediaId", media_id))
    }

    /// Media type of a clip, when it refers to registered media.
    pub fn media_type_of(&self, clip: &Clip) -> Option<MediaType> {
        let media_id = clip.media_id.as_deref()?;
        self.media.iter().find(|media| media.id == media_id).map(|media| media.media_type)
    }

    /// The other member of a clip's link group, if any.
    pub fn companion_id(&self, clip_id: &str) -> Option<String> {
        let group_id = &self.clip(clip_id).ok()?.link.as_ref()?.group_id;
        self.clips()
            .find(|other| other.id != clip_id && other.link.as_ref().is_some_and(|link| &link.group_id == group_id))
            .map(|other| other.id.clone())
    }

    /// The given clips plus their link companions, without duplicates, in input order.
    pub fn with_companions(&self, clip_ids: &[String]) -> Vec<String> {
        let mut all: Vec<String> = Vec::new();
        for clip_id in clip_ids {
            if !all.contains(clip_id) {
                all.push(clip_id.clone());
            }
            if let Some(companion) = self.companion_id(clip_id) {
                if !all.contains(&companion) {
                    all.push(companion);
                }
            }
        }
        all
    }

    /// First unused id of the form `<prefix><n>` (n counts from 1). Deterministic, so
    /// the same edits on the same project always produce the same ids.
    pub fn fresh_id(&self, prefix: &str, taken: impl Fn(&str) -> bool) -> String {
        let mut n = 1usize;
        loop {
            let candidate = format!("{prefix}{n}");
            if !taken(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    pub fn fresh_clip_id(&self) -> String {
        self.fresh_id("clip-", |id| self.clips().any(|clip| clip.id == id))
    }

    pub fn fresh_link_group_id(&self) -> String {
        self.fresh_id("link-", |id| self.clips().any(|clip| clip.link.as_ref().is_some_and(|link| link.group_id == id)))
    }

    /// Moves data from where the TypeScript implementation kept it to where this
    /// model keeps it. Idempotent; call once after loading a session file.
    pub fn adopt_legacy_fields(&mut self) {
        for track in &mut self.tracks {
            for clip in &mut track.clips {
                clip.track_id = track.id.clone();
                if clip.kind == ClipKind::Media && clip.source_start_seconds.is_none() {
                    clip.source_start_seconds = Some(Time::ZERO);
                }
                let Some(metadata) = clip.properties.metadata.as_mut() else { continue };
                let group = metadata.remove("linkedGroupId");
                let role = metadata.remove("linkedRole");
                // Ids of the other half, as the TypeScript version recorded them. The link
                // field is the only authority now, and these go stale on the first split.
                for stale in ["linkedClipId", "linkedAudioClipId", "linkedVideoClipId"] {
                    metadata.remove(stale);
                }
                if clip.link.is_none() {
                    if let (Some(Value::String(group_id)), Some(Value::String(role))) = (group, role) {
                        let role = if role == "audio" { LinkRole::Audio } else { LinkRole::Video };
                        clip.link = Some(ClipLink { group_id, role });
                    }
                }
                if metadata.is_empty() {
                    clip.properties.metadata = None;
                }
            }
        }
    }
}
