//! Every edit the product can make, as data.
//!
//! [`Operation`] is the single entry for the editor UI, the MCP server and auto-edit
//! plans. [`apply`] runs one operation on a copy of the project, re-pairs linked
//! clips, and rejects the result if it breaks any invariant in `validate`, so no
//! caller can produce or save an invalid project.
//!
//! Rules that hold for all operations:
//! - A locked track's clips are never changed; the operation fails with `TRACK_LOCKED`.
//! - A linked video/audio pair is edited together (move, trim, split, delete, speed).
//! - Input structs reject unknown fields, so a misspelled argument is an error rather
//!   than a silently ignored one.
//! - Overlap, media range and fade limits are enforced by the final validation, which
//!   reports the specific code (`CLIP_OVERLAP`, `MEDIA_RANGE_EXCEEDED`, ...).

use crate::error::{EditError, Res};
use crate::model::*;
use crate::time::Time;
use crate::validate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const DEFAULT_STILL_CLIP: Time = Time::from_micros(5_000_000);
const DEFAULT_CAPTION: Time = Time::from_micros(3_000_000);

// ---------------------------------------------------------------- inputs

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddTrack {
    #[serde(default)]
    pub id: Option<String>,
    pub kind: TrackKind,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub insert_before_track_id: Option<String>,
    #[serde(default)]
    pub insert_after_track_id: Option<String>,
    #[serde(default)]
    pub locked: Option<bool>,
    #[serde(default)]
    pub muted: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReorderTrack {
    pub track_id: String,
    #[serde(default)]
    pub insert_before_track_id: Option<String>,
    #[serde(default)]
    pub insert_after_track_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteTrack {
    pub track_id: String,
    #[serde(default)]
    pub delete_clips: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetTrackMuted {
    pub track_id: String,
    pub muted: bool,
}

/// Track header settings. Only the given fields change. Locking and unlocking are
/// allowed on a locked track; that is how it gets unlocked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetTrackProperties {
    pub track_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub locked: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterMedia {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(rename = "type")]
    pub media_type: MediaType,
    pub duration_seconds: Time,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub transcript: Option<String>,
    #[serde(default)]
    pub metadata: Option<Metadata>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteMedia {
    pub media_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkedAudio {
    #[serde(default)]
    pub id: Option<String>,
    pub track_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub properties: Option<ClipProperties>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddClip {
    #[serde(default)]
    pub id: Option<String>,
    pub track_id: String,
    pub media_id: String,
    pub start_seconds: Time,
    #[serde(default)]
    pub duration_seconds: Option<Time>,
    #[serde(default)]
    pub source_start_seconds: Option<Time>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub properties: Option<ClipProperties>,
    #[serde(default)]
    pub linked_audio: Option<LinkedAudio>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddClips {
    pub clips: Vec<AddClip>,
    #[serde(default)]
    pub edit_mode: EditMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveClip {
    pub clip_id: String,
    #[serde(default)]
    pub track_id: Option<String>,
    #[serde(default)]
    pub start_seconds: Option<Time>,
    #[serde(default)]
    pub edit_mode: EditMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveClips {
    pub clip_ids: Vec<String>,
    /// Where the earliest of the clips should start. Give this or `deltaSeconds`.
    #[serde(default)]
    pub start_seconds: Option<Time>,
    /// How far to move every clip. Give this or `startSeconds`.
    #[serde(default)]
    pub delta_seconds: Option<Time>,
    #[serde(default)]
    pub edit_mode: EditMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrimClip {
    pub clip_id: String,
    #[serde(default)]
    pub start_seconds: Option<Time>,
    #[serde(default)]
    pub end_seconds: Option<Time>,
    #[serde(default)]
    pub duration_seconds: Option<Time>,
    #[serde(default)]
    pub source_start_seconds: Option<Time>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RippleTrimClip {
    pub clip_id: String,
    pub edge: Edge,
    pub at_seconds: Time,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RollEdit {
    pub left_clip_id: String,
    pub right_clip_id: String,
    pub edit_point_seconds: Time,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SlipClip {
    pub clip_id: String,
    #[serde(default)]
    pub source_start_seconds: Option<Time>,
    #[serde(default)]
    pub delta_source_seconds: Option<Time>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SlideClip {
    pub clip_id: String,
    #[serde(default)]
    pub start_seconds: Option<Time>,
    #[serde(default)]
    pub delta_seconds: Option<Time>,
    #[serde(default)]
    pub left_clip_id: Option<String>,
    #[serde(default)]
    pub right_clip_id: Option<String>,
}

/// Names one clip either directly or as "the clip under this time on this track".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipAtTime {
    pub at_seconds: Time,
    #[serde(default)]
    pub clip_id: Option<String>,
    #[serde(default)]
    pub track_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipsAtTime {
    pub at_seconds: Time,
    pub clip_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrimClipToTime {
    pub at_seconds: Time,
    pub edge: Edge,
    #[serde(default)]
    pub clip_id: Option<String>,
    #[serde(default)]
    pub track_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrimClipsToTime {
    pub at_seconds: Time,
    pub edge: Edge,
    pub clip_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SplitClip {
    pub clip_id: String,
    /// Timeline time of the cut. `time` is the name in the published tool contract.
    #[serde(rename = "time", alias = "atSeconds")]
    pub at_seconds: Time,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BladeAtTime {
    pub at_seconds: Time,
    #[serde(default)]
    pub track_ids: Option<Vec<String>>,
    #[serde(default)]
    pub clip_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipRef {
    pub clip_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipRefs {
    pub clip_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloseGapAtTime {
    pub at_seconds: Time,
    pub track_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RangeEdit {
    pub start_seconds: Time,
    pub end_seconds: Time,
    #[serde(default)]
    pub track_ids: Option<Vec<String>>,
    #[serde(default)]
    pub include_linked_companions: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetClipProperties {
    pub clip_id: String,
    pub properties: ClipProperties,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetClipAudio {
    pub clip_id: String,
    #[serde(default)]
    pub volume_db: Option<f64>,
    #[serde(default)]
    pub muted: Option<bool>,
    #[serde(default)]
    pub fade_in_seconds: Option<Time>,
    #[serde(default)]
    pub fade_out_seconds: Option<Time>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetCaptionText {
    pub clip_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptionInput {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub track_id: Option<String>,
    pub text: String,
    pub start_seconds: Time,
    #[serde(default)]
    pub duration_seconds: Option<Time>,
    #[serde(default)]
    pub end_seconds: Option<Time>,
    #[serde(default)]
    pub speaker: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub properties: Option<ClipProperties>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddCaptions {
    #[serde(default)]
    pub track_id: Option<String>,
    pub captions: Vec<CaptionInput>,
}

/// Project-wide settings. Only the given fields change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetProjectSettings {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub fps: Option<f64>,
    #[serde(default)]
    pub resolution: Option<Resolution>,
    #[serde(default)]
    pub sample_rate: Option<u32>,
    /// Keys to set in the project metadata; a `null` value removes the key.
    #[serde(default)]
    pub metadata: Option<Metadata>,
}

/// One edit. The tag is the MCP tool name, so `{"tool": "split_clip", "args": {...}}`
/// is both the wire form and the auto-edit-plan step form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tool", content = "args", rename_all = "snake_case")]
pub enum Operation {
    AddTrack(AddTrack),
    ReorderTrack(ReorderTrack),
    DeleteTrack(DeleteTrack),
    SetTrackMuted(SetTrackMuted),
    SetTrackProperties(SetTrackProperties),
    RegisterMedia(RegisterMedia),
    DeleteMedia(DeleteMedia),
    AddClips(AddClips),
    MoveClip(MoveClip),
    MoveClips(MoveClips),
    TrimClip(TrimClip),
    RippleTrimClip(RippleTrimClip),
    RollEdit(RollEdit),
    SlipClip(SlipClip),
    SlideClip(SlideClip),
    TrimClipToTime(TrimClipToTime),
    TrimClipsToTime(TrimClipsToTime),
    DeleteLeftAtTime(ClipAtTime),
    DeleteRightAtTime(ClipAtTime),
    DeleteClipsLeftAtTime(ClipsAtTime),
    DeleteClipsRightAtTime(ClipsAtTime),
    SplitClip(SplitClip),
    BladeAtTime(BladeAtTime),
    DeleteClip(ClipRef),
    DeleteClips(ClipRefs),
    RippleDeleteClip(ClipRef),
    CloseGapAtTime(CloseGapAtTime),
    LiftRange(RangeEdit),
    ExtractRange(RangeEdit),
    SetClipProperties(SetClipProperties),
    SetClipAudio(SetClipAudio),
    SetCaptionText(SetCaptionText),
    AddCaptions(AddCaptions),
    SetProjectSettings(SetProjectSettings),
}

impl Operation {
    /// The MCP tool name of this operation.
    pub fn tool_name(&self) -> String {
        match serde_json::to_value(self) {
            Ok(Value::Object(map)) => map.get("tool").and_then(Value::as_str).unwrap_or_default().to_string(),
            _ => String::new(),
        }
    }
}

/// What an operation did, for tool output and the activity log.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Outcome {
    pub summary: String,
    pub details: Value,
}

fn outcome(summary: impl Into<String>, details: Value) -> Res<Outcome> {
    Ok(Outcome { summary: summary.into(), details })
}

// ---------------------------------------------------------------- entry

/// Applies one operation and returns the new project. The input is untouched.
pub fn apply(project: &Project, operation: &Operation) -> Res<(Project, Outcome)> {
    let mut draft = Draft { project: project.clone() };
    let result = draft.run(operation)?;
    draft.relink();
    draft.sort();
    if let Some(issue) = validate::issues(&draft.project).into_iter().find(|issue| issue.severity == validate::Severity::Error) {
        let mut error = EditError::new(issue.code, issue.message);
        if let Some(track_id) = issue.track_id {
            error = error.with("trackId", track_id);
        }
        if let Some(clip_id) = issue.clip_id {
            error = error.with("clipId", clip_id);
        }
        if let Some(media_id) = issue.media_id {
            error = error.with("mediaId", media_id);
        }
        return Err(error);
    }
    Ok((draft.project, result))
}

struct Draft {
    project: Project,
}

fn non_negative(name: &str, value: Time) -> Res<Time> {
    if value.is_negative() {
        return Err(EditError::new("INVALID_TIME", format!("{name} must not be negative, got {value}.")).with("value", value.secs()));
    }
    Ok(value)
}

fn positive(name: &str, value: Time) -> Res<Time> {
    if !value.is_positive() {
        return Err(EditError::new("INVALID_DURATION", format!("{name} must be positive, got {value}.")).with("value", value.secs()));
    }
    Ok(value)
}

/// Shrinks fades so they fit inside the clip after a trim or split.
fn clamp_fades(clip: &mut Clip) {
    let duration = clip.duration_seconds;
    if let Some(fade_in) = clip.properties.fade_in_seconds {
        clip.properties.fade_in_seconds = Some(fade_in.min(duration));
    }
    if let Some(fade_out) = clip.properties.fade_out_seconds {
        clip.properties.fade_out_seconds = Some(fade_out.min(duration - clip.fade_in()));
    }
}

fn merge_properties(target: &mut ClipProperties, patch: &ClipProperties) {
    macro_rules! take {
        ($($field:ident),*) => { $( if patch.$field.is_some() { target.$field = patch.$field.clone(); } )* };
    }
    take!(position, scale, rotation_degrees, opacity, crop, volume_db, muted, fade_in_seconds, fade_out_seconds, speed, caption_style);
    if let Some(patch_metadata) = &patch.metadata {
        let metadata = target.metadata.get_or_insert_with(Metadata::new);
        for (key, value) in patch_metadata {
            metadata.insert(key.clone(), value.clone());
        }
    }
}

impl Draft {
    // ------------------------------------------------------------ primitives

    fn unlocked(&self, track_index: usize) -> Res<()> {
        let track = &self.project.tracks[track_index];
        if track.is_locked() {
            return Err(EditError::new("TRACK_LOCKED", format!("Track {} is locked.", track.id)).with("trackId", track.id.clone()));
        }
        Ok(())
    }

    fn sort(&mut self) {
        for track in &mut self.project.tracks {
            track.clips.sort_by(|a, b| a.start_seconds.cmp(&b.start_seconds).then_with(|| a.id.cmp(&b.id)));
        }
    }

    fn clip_mut(&mut self, clip_id: &str) -> Res<&mut Clip> {
        let (track_index, clip_index) = self.project.locate(clip_id)?;
        self.unlocked(track_index)?;
        Ok(&mut self.project.tracks[track_index].clips[clip_index])
    }

    fn is_still(&self, clip: &Clip) -> bool {
        clip.kind == ClipKind::Caption || self.project.media_type_of(clip).is_some_and(MediaType::is_still)
    }

    /// Removes a clip from its track and returns it.
    fn take(&mut self, clip_id: &str) -> Res<Clip> {
        let (track_index, clip_index) = self.project.locate(clip_id)?;
        self.unlocked(track_index)?;
        Ok(self.project.tracks[track_index].clips.remove(clip_index))
    }

    /// Puts a clip on the track named by its `track_id`.
    fn put(&mut self, clip: Clip) -> Res<()> {
        let track_index = self.project.track_index(&clip.track_id)?;
        self.unlocked(track_index)?;
        self.project.tracks[track_index].clips.push(clip);
        Ok(())
    }

    /// Changes where a clip starts and ends on the timeline. Moving the start also
    /// moves the source start by the same amount of timeline time times the speed,
    /// so the frames that stay visible stay where they were.
    fn set_span(&mut self, clip_id: &str, start: Time, end: Time) -> Res<()> {
        if end <= start {
            return Err(EditError::new("INVALID_TRIM", "A trim must leave a positive duration.").with("clipId", clip_id));
        }
        let still = self.is_still(self.project.clip(clip_id)?);
        let clip = self.clip_mut(clip_id)?;
        if !still {
            let shift = (start - clip.start_seconds).scale(clip.speed());
            clip.source_start_seconds = Some(clip.source_start() + shift);
        }
        clip.start_seconds = start;
        clip.duration_seconds = end - start;
        clamp_fades(clip);
        Ok(())
    }

    /// `set_span` on a clip and on its link companion.
    fn set_span_linked(&mut self, clip_id: &str, start: Time, end: Time) -> Res<()> {
        let companion = self.project.companion_id(clip_id);
        self.set_span(clip_id, start, end)?;
        if let Some(companion) = companion {
            self.set_span(&companion, start, end)?;
        }
        Ok(())
    }

    /// Cuts one clip in two at a timeline time and returns the id of the right half.
    /// The cut removes the fade-out of the left half and the fade-in of the right.
    fn split(&mut self, clip_id: &str, at: Time) -> Res<String> {
        let (track_index, clip_index) = self.project.locate(clip_id)?;
        self.unlocked(track_index)?;
        let original = self.project.tracks[track_index].clips[clip_index].clone();
        if !original.contains(at) {
            return Err(EditError::new("SPLIT_OUTSIDE_CLIP", format!("{at}s is not inside clip {clip_id}."))
                .with("clipId", clip_id)
                .with("atSeconds", at.secs()));
        }
        let still = self.is_still(&original);
        let left_duration = at - original.start_seconds;
        let mut right = original.clone();
        right.id = self.project.fresh_clip_id();
        right.start_seconds = at;
        right.duration_seconds = original.end() - at;
        if !still {
            right.source_start_seconds = Some(original.source_start() + left_duration.scale(original.speed()));
        }
        right.properties.fade_in_seconds = None;
        clamp_fades(&mut right);
        let right_id = right.id.clone();

        let left = &mut self.project.tracks[track_index].clips[clip_index];
        left.duration_seconds = left_duration;
        left.properties.fade_out_seconds = None;
        clamp_fades(left);
        self.project.tracks[track_index].clips.insert(clip_index + 1, right);
        Ok(right_id)
    }

    /// Empties `[start, end)` on one track: clips inside are removed, clips crossing
    /// an edge are cut there. Returns the ids of removed clips.
    fn clear_range(&mut self, track_index: usize, start: Time, end: Time) -> Res<Vec<String>> {
        self.unlocked(track_index)?;
        for edge in [start, end] {
            let crossing = self.project.tracks[track_index].clips.iter().find(|clip| clip.contains(edge)).map(|clip| clip.id.clone());
            if let Some(clip_id) = crossing {
                self.split(&clip_id, edge)?;
            }
        }
        let track = &mut self.project.tracks[track_index];
        let removed: Vec<String> = track
            .clips
            .iter()
            .filter(|clip| clip.start_seconds >= start && clip.end() <= end)
            .map(|clip| clip.id.clone())
            .collect();
        track.clips.retain(|clip| !(clip.start_seconds >= start && clip.end() <= end));
        Ok(removed)
    }

    /// Moves every clip on a track that starts at or after `from` by `delta`.
    fn shift_from(&mut self, track_index: usize, from: Time, delta: Time) -> Res<()> {
        if delta == Time::ZERO || !self.project.tracks[track_index].clips.iter().any(|clip| clip.start_seconds >= from) {
            return Ok(());
        }
        self.unlocked(track_index)?;
        for clip in &mut self.project.tracks[track_index].clips {
            if clip.start_seconds >= from {
                clip.start_seconds += delta;
            }
        }
        Ok(())
    }

    /// Makes room of `length` at `at` on one track, cutting a clip that crosses `at`.
    fn open_gap(&mut self, track_index: usize, at: Time, length: Time) -> Res<()> {
        let crossing = self.project.tracks[track_index].clips.iter().find(|clip| clip.contains(at)).map(|clip| clip.id.clone());
        if let Some(clip_id) = crossing {
            self.split(&clip_id, at)?;
        }
        self.shift_from(track_index, at, length)
    }

    /// Places a clip according to the edit mode.
    fn place(&mut self, clip: Clip, mode: EditMode) -> Res<()> {
        let track_index = self.project.track_index(&clip.track_id)?;
        match mode {
            EditMode::Strict => {}
            EditMode::Insert => self.open_gap(track_index, clip.start_seconds, clip.duration_seconds)?,
            EditMode::Overwrite => {
                self.clear_range(track_index, clip.start_seconds, clip.end())?;
            }
        }
        self.put(clip)
    }

    /// Re-pairs linked clips after an edit. Within each link group, a video member and
    /// an audio member that still agree on timing, source and speed stay linked (one
    /// group id per pair); members left without a partner lose their link.
    fn relink(&mut self) {
        let mut group_ids: Vec<String> = Vec::new();
        for clip in self.project.clips() {
            if let Some(link) = &clip.link {
                if !group_ids.contains(&link.group_id) {
                    group_ids.push(link.group_id.clone());
                }
            }
        }
        for group_id in group_ids {
            let members: Vec<Clip> = self
                .project
                .clips()
                .filter(|clip| clip.link.as_ref().is_some_and(|link| link.group_id == group_id))
                .cloned()
                .collect();
            let key = |clip: &Clip| (clip.start_seconds, clip.duration_seconds, clip.source_start(), clip.speed().to_bits(), clip.media_id.clone());
            let role = |clip: &Clip| clip.link.as_ref().map(|link| link.role);
            let mut pairs: Vec<(String, String)> = Vec::new();
            let mut used: Vec<String> = Vec::new();
            for video in members.iter().filter(|clip| role(clip) == Some(LinkRole::Video)) {
                let partner = members
                    .iter()
                    .find(|audio| role(audio) == Some(LinkRole::Audio) && !used.contains(&audio.id) && key(audio) == key(video));
                if let Some(audio) = partner {
                    used.push(audio.id.clone());
                    used.push(video.id.clone());
                    pairs.push((video.id.clone(), audio.id.clone()));
                }
            }
            if pairs.len() == 1 && members.len() == 2 {
                continue;
            }
            for member in &members {
                if !used.contains(&member.id) {
                    if let Ok((track_index, clip_index)) = self.project.locate(&member.id) {
                        self.project.tracks[track_index].clips[clip_index].link = None;
                    }
                }
            }
            for (index, (video_id, audio_id)) in pairs.iter().enumerate() {
                if index == 0 {
                    continue;
                }
                let new_group = self.project.fresh_link_group_id();
                for clip_id in [video_id, audio_id] {
                    if let Ok((track_index, clip_index)) = self.project.locate(clip_id) {
                        if let Some(link) = &mut self.project.tracks[track_index].clips[clip_index].link {
                            link.group_id = new_group.clone();
                        }
                    }
                }
            }
        }
    }

    /// Resolves "this clip" or "the clip under `at` on this track".
    fn resolve_clip_at(&self, at: Time, clip_id: &Option<String>, track_id: &Option<String>) -> Res<String> {
        if let Some(clip_id) = clip_id {
            self.project.locate(clip_id)?;
            return Ok(clip_id.clone());
        }
        let Some(track_id) = track_id else {
            return Err(EditError::new("CLIP_TARGET_REQUIRED", "Give clipId, or trackId to pick the clip under atSeconds."));
        };
        let track = self.project.track(track_id)?;
        track
            .clips
            .iter()
            .find(|clip| clip.start_seconds <= at && at <= clip.end())
            .map(|clip| clip.id.clone())
            .ok_or_else(|| EditError::new("NO_CLIP_AT_TIME", format!("No clip on track {track_id} at {at}s.")).with("trackId", track_id.clone()).with("atSeconds", at.secs()))
    }

    /// Trims one edge of a clip (and its companion) to a time.
    fn trim_edge_to(&mut self, clip_id: &str, edge: Edge, at: Time) -> Res<()> {
        let clip = self.project.clip(clip_id)?;
        let (start, end) = match edge {
            Edge::Start => (at, clip.end()),
            Edge::End => (clip.start_seconds, at),
        };
        self.set_span_linked(clip_id, start, end)
    }

    // ------------------------------------------------------------ dispatch

    fn run(&mut self, operation: &Operation) -> Res<Outcome> {
        match operation {
            Operation::AddTrack(input) => self.add_track(input),
            Operation::ReorderTrack(input) => self.reorder_track(input),
            Operation::DeleteTrack(input) => self.delete_track(input),
            Operation::SetTrackMuted(input) => {
                let track_index = self.project.track_index(&input.track_id)?;
                self.project.tracks[track_index].muted = Some(input.muted);
                outcome(format!("Track {} {}.", input.track_id, if input.muted { "muted" } else { "unmuted" }), json!({ "trackId": input.track_id, "muted": input.muted }))
            }
            Operation::SetTrackProperties(input) => {
                let track_index = self.project.track_index(&input.track_id)?;
                let track = &mut self.project.tracks[track_index];
                if let Some(name) = &input.name {
                    if name.trim().is_empty() {
                        return Err(EditError::new("INVALID_TRACK_NAME", "A track name must not be blank."));
                    }
                    track.name = name.trim().to_string();
                }
                if let Some(locked) = input.locked {
                    track.locked = Some(locked);
                }
                outcome(format!("Updated track {}.", input.track_id), json!({ "trackId": input.track_id, "name": track.name, "locked": track.is_locked() }))
            }
            Operation::RegisterMedia(input) => self.register_media(input),
            Operation::DeleteMedia(input) => self.delete_media(input),
            Operation::AddClips(input) => self.add_clips(input),
            Operation::MoveClip(input) => self.move_clip(input),
            Operation::MoveClips(input) => self.move_clips(input),
            Operation::TrimClip(input) => self.trim_clip(input),
            Operation::RippleTrimClip(input) => self.ripple_trim_clip(input),
            Operation::RollEdit(input) => self.roll_edit(input),
            Operation::SlipClip(input) => self.slip_clip(input),
            Operation::SlideClip(input) => self.slide_clip(input),
            Operation::TrimClipToTime(input) => {
                let clip_id = self.resolve_clip_at(input.at_seconds, &input.clip_id, &input.track_id)?;
                self.trim_edges_to(&[clip_id], input.edge, input.at_seconds)
            }
            Operation::TrimClipsToTime(input) => self.trim_edges_to(&input.clip_ids, input.edge, input.at_seconds),
            Operation::DeleteLeftAtTime(input) => {
                let clip_id = self.resolve_clip_at(input.at_seconds, &input.clip_id, &input.track_id)?;
                self.trim_edges_to(&[clip_id], Edge::Start, input.at_seconds)
            }
            Operation::DeleteRightAtTime(input) => {
                let clip_id = self.resolve_clip_at(input.at_seconds, &input.clip_id, &input.track_id)?;
                self.trim_edges_to(&[clip_id], Edge::End, input.at_seconds)
            }
            Operation::DeleteClipsLeftAtTime(input) => self.trim_edges_to(&input.clip_ids, Edge::Start, input.at_seconds),
            Operation::DeleteClipsRightAtTime(input) => self.trim_edges_to(&input.clip_ids, Edge::End, input.at_seconds),
            Operation::SplitClip(input) => {
                let created = self.split_linked(std::slice::from_ref(&input.clip_id), input.at_seconds)?;
                outcome(format!("Split clip {} at {}s.", input.clip_id, input.at_seconds), json!({ "clipId": input.clip_id, "createdClipIds": created }))
            }
            Operation::BladeAtTime(input) => self.blade_at_time(input),
            Operation::DeleteClip(input) => self.delete_clips(std::slice::from_ref(&input.clip_id), false),
            Operation::DeleteClips(input) => self.delete_clips(&input.clip_ids, false),
            Operation::RippleDeleteClip(input) => self.delete_clips(std::slice::from_ref(&input.clip_id), true),
            Operation::CloseGapAtTime(input) => self.close_gap_at_time(input),
            Operation::LiftRange(input) => self.range_edit(input, false),
            Operation::ExtractRange(input) => self.range_edit(input, true),
            Operation::SetClipProperties(input) => self.set_clip_properties(&input.clip_id, &input.properties),
            Operation::SetClipAudio(input) => {
                let patch = ClipProperties {
                    volume_db: input.volume_db,
                    muted: input.muted,
                    fade_in_seconds: input.fade_in_seconds,
                    fade_out_seconds: input.fade_out_seconds,
                    ..ClipProperties::default()
                };
                self.set_clip_properties(&input.clip_id, &patch)
            }
            Operation::SetCaptionText(input) => {
                let clip = self.clip_mut(&input.clip_id)?;
                let Some(caption) = clip.caption.as_mut() else {
                    return Err(EditError::new("NOT_A_CAPTION", format!("Clip {} is not a caption.", input.clip_id)).with("clipId", input.clip_id.clone()));
                };
                caption.text = input.text.clone();
                outcome(format!("Updated caption {}.", input.clip_id), json!({ "clipId": input.clip_id }))
            }
            Operation::AddCaptions(input) => self.add_captions(input),
            Operation::SetProjectSettings(input) => {
                if let Some(title) = &input.title {
                    self.project.title = title.clone();
                }
                if let Some(fps) = input.fps {
                    self.project.fps = fps;
                }
                if let Some(resolution) = input.resolution {
                    self.project.resolution = resolution;
                }
                if let Some(sample_rate) = input.sample_rate {
                    if sample_rate == 0 {
                        return Err(EditError::new("INVALID_SAMPLE_RATE", "sampleRate must be positive."));
                    }
                    self.project.sample_rate = sample_rate;
                }
                if let Some(patch) = &input.metadata {
                    let metadata = self.project.metadata.get_or_insert_with(Metadata::new);
                    for (key, value) in patch {
                        if value.is_null() {
                            metadata.remove(key);
                        } else {
                            metadata.insert(key.clone(), value.clone());
                        }
                    }
                    if metadata.is_empty() {
                        self.project.metadata = None;
                    }
                }
                outcome("Updated project settings.", json!({ "title": self.project.title, "fps": self.project.fps, "resolution": self.project.resolution }))
            }
        }
    }

    // ------------------------------------------------------------ tracks and media

    fn add_track(&mut self, input: &AddTrack) -> Res<Outcome> {
        let (prefix, label) = match input.kind {
            TrackKind::Video => ("V", "Video"),
            TrackKind::Audio => ("A", "Audio"),
            TrackKind::Caption => ("C", "Captions"),
        };
        let id = match &input.id {
            Some(id) if self.project.tracks.iter().any(|track| &track.id == id) => {
                return Err(EditError::new("DUPLICATE_TRACK_ID", format!("Track id {id} already exists.")).with("trackId", id.clone()));
            }
            Some(id) => id.clone(),
            None => self.project.fresh_id(prefix, |candidate| self.project.tracks.iter().any(|track| track.id == candidate)),
        };
        let name = input.name.clone().unwrap_or_else(|| {
            let number = self.project.tracks.iter().filter(|track| track.kind == input.kind).count() + 1;
            format!("{label} {number}")
        });
        let position = if let Some(before) = &input.insert_before_track_id {
            self.project.track_index(before)?
        } else if let Some(after) = &input.insert_after_track_id {
            self.project.track_index(after)? + 1
        } else {
            match self.project.tracks.iter().rposition(|track| track.kind == input.kind) {
                Some(last_of_kind) => last_of_kind + 1,
                None => self.project.tracks.len(),
            }
        };
        self.project.tracks.insert(position, Track { id: id.clone(), name, kind: input.kind, locked: input.locked, muted: input.muted, clips: Vec::new() });
        outcome(format!("Added track {id}."), json!({ "trackId": id, "index": position }))
    }

    fn reorder_track(&mut self, input: &ReorderTrack) -> Res<Outcome> {
        let from = self.project.track_index(&input.track_id)?;
        let anchor = input.insert_before_track_id.as_ref().or(input.insert_after_track_id.as_ref());
        let Some(anchor) = anchor else {
            return Err(EditError::new("REORDER_TARGET_REQUIRED", "Give insertBeforeTrackId or insertAfterTrackId."));
        };
        if anchor == &input.track_id {
            return Err(EditError::new("REORDER_TARGET_INVALID", "A track cannot be placed relative to itself."));
        }
        self.project.track_index(anchor)?;
        let track = self.project.tracks.remove(from);
        let anchor_index = self.project.track_index(anchor)?;
        let to = if input.insert_before_track_id.is_some() { anchor_index } else { anchor_index + 1 };
        self.project.tracks.insert(to, track);
        outcome(format!("Moved track {}.", input.track_id), json!({ "trackId": input.track_id, "index": to }))
    }

    fn delete_track(&mut self, input: &DeleteTrack) -> Res<Outcome> {
        let track_index = self.project.track_index(&input.track_id)?;
        self.unlocked(track_index)?;
        let clip_ids: Vec<String> = self.project.tracks[track_index].clips.iter().map(|clip| clip.id.clone()).collect();
        if !clip_ids.is_empty() && !input.delete_clips {
            return Err(EditError::new("TRACK_NOT_EMPTY", format!("Track {} still has {} clip(s); pass deleteClips: true to delete them too.", input.track_id, clip_ids.len()))
                .with("trackId", input.track_id.clone())
                .with("clipCount", clip_ids.len()));
        }
        let all = self.project.with_companions(&clip_ids);
        for clip_id in &all {
            self.take(clip_id)?;
        }
        let track_index = self.project.track_index(&input.track_id)?;
        self.project.tracks.remove(track_index);
        outcome(format!("Deleted track {}.", input.track_id), json!({ "trackId": input.track_id, "deletedClipIds": all }))
    }

    fn register_media(&mut self, input: &RegisterMedia) -> Res<Outcome> {
        positive("durationSeconds", input.duration_seconds)?;
        let id = match &input.id {
            Some(id) if self.project.media.iter().any(|media| &media.id == id) => {
                return Err(EditError::new("DUPLICATE_MEDIA_ID", format!("Media id {id} already exists.")).with("mediaId", id.clone()));
            }
            Some(id) => id.clone(),
            None => self.project.fresh_id("media-", |candidate| self.project.media.iter().any(|media| media.id == candidate)),
        };
        self.project.media.push(MediaAsset {
            id: id.clone(),
            name: input.name.clone(),
            media_type: input.media_type,
            duration_seconds: input.duration_seconds,
            source: input.source.clone(),
            thumbnail: input.thumbnail.clone(),
            tags: input.tags.clone(),
            transcript: input.transcript.clone(),
            metadata: input.metadata.clone(),
        });
        outcome(format!("Registered media {id}."), json!({ "mediaId": id }))
    }

    fn delete_media(&mut self, input: &DeleteMedia) -> Res<Outcome> {
        self.project.media(&input.media_id)?;
        let users: Vec<String> = self.project.clips().filter(|clip| clip.media_id.as_deref() == Some(&input.media_id)).map(|clip| clip.id.clone()).collect();
        if !users.is_empty() {
            return Err(EditError::new("MEDIA_IN_USE", format!("Media {} is used by {} clip(s).", input.media_id, users.len()))
                .with("mediaId", input.media_id.clone())
                .with("clipIds", users));
        }
        self.project.media.retain(|media| media.id != input.media_id);
        outcome(format!("Deleted media {}.", input.media_id), json!({ "mediaId": input.media_id }))
    }

    // ------------------------------------------------------------ clips

    fn add_clips(&mut self, input: &AddClips) -> Res<Outcome> {
        if input.clips.is_empty() {
            return Err(EditError::new("NO_CLIPS", "clips must not be empty."));
        }
        let mut created: Vec<String> = Vec::new();
        for item in &input.clips {
            let media = self.project.media(&item.media_id)?.clone();
            let start = non_negative("startSeconds", item.start_seconds)?;
            let source_start = non_negative("sourceStartSeconds", item.source_start_seconds.unwrap_or(Time::ZERO))?;
            let properties = item.properties.clone().unwrap_or_default();
            let speed = match properties.speed {
                Some(speed) if speed.is_finite() && speed > 0.0 => speed,
                _ => 1.0,
            };
            let duration = match item.duration_seconds {
                Some(duration) => positive("durationSeconds", duration)?,
                None if media.media_type.is_still() => DEFAULT_STILL_CLIP,
                None => positive("durationSeconds", (media.duration_seconds - source_start).scale(1.0 / speed))?,
            };
            if let Some(id) = &item.id {
                if self.project.locate(id).is_ok() {
                    return Err(EditError::new("DUPLICATE_CLIP_ID", format!("Clip id {id} already exists.")).with("clipId", id.clone()));
                }
            }
            let id = item.id.clone().unwrap_or_else(|| self.project.fresh_clip_id());
            let link_group = item.linked_audio.as_ref().map(|_| self.project.fresh_link_group_id());
            if item.linked_audio.is_some() && media.media_type != MediaType::Video {
                return Err(EditError::new("LINKED_AUDIO_REQUIRES_VIDEO", format!("Only video media can carry linked audio; {} is {:?}.", media.id, media.media_type)).with("mediaId", media.id.clone()));
            }
            let clip = Clip {
                id: id.clone(),
                track_id: item.track_id.clone(),
                kind: ClipKind::Media,
                name: item.name.clone().unwrap_or_else(|| media.name.clone()),
                start_seconds: start,
                duration_seconds: duration,
                media_id: Some(media.id.clone()),
                source_start_seconds: Some(source_start),
                properties: properties.clone(),
                caption: None,
                link: link_group.clone().map(|group_id| ClipLink { group_id, role: LinkRole::Video }),
            };
            self.place(clip, input.edit_mode)?;
            created.push(id);

            if let (Some(linked), Some(group_id)) = (&item.linked_audio, link_group) {
                if let Some(audio_id) = &linked.id {
                    if self.project.locate(audio_id).is_ok() {
                        return Err(EditError::new("DUPLICATE_CLIP_ID", format!("Clip id {audio_id} already exists.")).with("clipId", audio_id.clone()));
                    }
                }
                let audio_id = linked.id.clone().unwrap_or_else(|| self.project.fresh_clip_id());
                let mut audio_properties = linked.properties.clone().unwrap_or_default();
                // A linked pair always plays at one speed: the video clip's.
                audio_properties.speed = properties.speed;
                let audio = Clip {
                    id: audio_id.clone(),
                    track_id: linked.track_id.clone(),
                    kind: ClipKind::Media,
                    name: linked.name.clone().unwrap_or_else(|| format!("{} (audio)", media.name)),
                    start_seconds: start,
                    duration_seconds: duration,
                    media_id: Some(media.id.clone()),
                    source_start_seconds: Some(source_start),
                    properties: audio_properties,
                    caption: None,
                    link: Some(ClipLink { group_id, role: LinkRole::Audio }),
                };
                self.place(audio, input.edit_mode)?;
                created.push(audio_id);
            }
        }
        outcome(format!("Added {} clip(s).", created.len()), json!({ "clipIds": created, "editMode": input.edit_mode }))
    }

    /// Moves a set of clips (already expanded with companions) by one delta. Clips are
    /// lifted off the timeline first, so they never collide with their own old place.
    fn relocate(&mut self, clip_ids: &[String], delta: Time, retarget: Option<(&str, &str)>, mode: EditMode) -> Res<()> {
        let mut lifted: Vec<Clip> = Vec::new();
        for clip_id in clip_ids {
            let mut clip = self.take(clip_id)?;
            clip.start_seconds += delta;
            non_negative("startSeconds", clip.start_seconds)?;
            if let Some((moved_id, track_id)) = retarget {
                if clip.id == moved_id {
                    clip.track_id = track_id.to_string();
                }
            }
            lifted.push(clip);
        }
        match mode {
            EditMode::Strict | EditMode::Overwrite => {
                // Overwrite clears exactly the range each clip will occupy, never the
                // space between two moved clips.
                for clip in lifted {
                    self.place(clip, mode)?;
                }
            }
            EditMode::Insert => {
                // One gap per track, as long as the moved material on that track spans,
                // so clips moved together keep their spacing.
                let mut track_ids: Vec<String> = Vec::new();
                for clip in &lifted {
                    if !track_ids.contains(&clip.track_id) {
                        track_ids.push(clip.track_id.clone());
                    }
                }
                for track_id in track_ids {
                    let track_index = self.project.track_index(&track_id)?;
                    let on_track: Vec<&Clip> = lifted.iter().filter(|clip| clip.track_id == track_id).collect();
                    let start = on_track.iter().map(|clip| clip.start_seconds).min().unwrap_or(Time::ZERO);
                    let end = on_track.iter().map(|clip| clip.end()).max().unwrap_or(Time::ZERO);
                    self.open_gap(track_index, start, end - start)?;
                }
                for clip in lifted {
                    self.put(clip)?;
                }
            }
        }
        Ok(())
    }

    fn move_clip(&mut self, input: &MoveClip) -> Res<Outcome> {
        let clip = self.project.clip(&input.clip_id)?.clone();
        let delta = input.start_seconds.map(|start| start - clip.start_seconds).unwrap_or(Time::ZERO);
        if let Some(track_id) = &input.track_id {
            self.project.track_index(track_id)?;
        }
        let ids = self.project.with_companions(std::slice::from_ref(&input.clip_id));
        let retarget = input.track_id.as_deref().map(|track_id| (input.clip_id.as_str(), track_id));
        self.relocate(&ids, delta, retarget, input.edit_mode)?;
        outcome(format!("Moved clip {}.", input.clip_id), json!({ "clipIds": ids, "deltaSeconds": delta.secs(), "editMode": input.edit_mode }))
    }

    fn move_clips(&mut self, input: &MoveClips) -> Res<Outcome> {
        if input.clip_ids.is_empty() {
            return Err(EditError::new("NO_CLIPS", "clipIds must not be empty."));
        }
        let ids = self.project.with_companions(&input.clip_ids);
        let mut earliest: Option<Time> = None;
        for clip_id in &ids {
            let start = self.project.clip(clip_id)?.start_seconds;
            earliest = Some(earliest.map_or(start, |current| current.min(start)));
        }
        let delta = match (input.start_seconds, input.delta_seconds) {
            (Some(_), Some(_)) => return Err(EditError::new("CONFLICTING_MOVE", "Give startSeconds or deltaSeconds, not both.")),
            (Some(start), None) => start - earliest.unwrap_or(Time::ZERO),
            (None, Some(delta)) => delta,
            (None, None) => return Err(EditError::new("MOVE_TARGET_REQUIRED", "Give startSeconds or deltaSeconds.")),
        };
        self.relocate(&ids, delta, None, input.edit_mode)?;
        outcome(format!("Moved {} clip(s).", ids.len()), json!({ "clipIds": ids, "deltaSeconds": delta.secs(), "editMode": input.edit_mode }))
    }

    fn trim_clip(&mut self, input: &TrimClip) -> Res<Outcome> {
        let clip = self.project.clip(&input.clip_id)?.clone();
        let start = input.start_seconds.unwrap_or(clip.start_seconds);
        let end = match (input.end_seconds, input.duration_seconds) {
            (Some(_), Some(_)) => return Err(EditError::new("CONFLICTING_TRIM", "Give endSeconds or durationSeconds, not both.")),
            (Some(end), None) => end,
            (None, Some(duration)) => start + positive("durationSeconds", duration)?,
            (None, None) => clip.end(),
        };
        non_negative("startSeconds", start)?;
        let ids = self.project.with_companions(std::slice::from_ref(&input.clip_id));
        self.set_span_linked(&input.clip_id, start, end)?;
        if let Some(source_start) = input.source_start_seconds {
            non_negative("sourceStartSeconds", source_start)?;
            for clip_id in &ids {
                self.clip_mut(clip_id)?.source_start_seconds = Some(source_start);
            }
        }
        outcome(format!("Trimmed clip {}.", input.clip_id), json!({ "clipIds": ids, "startSeconds": start.secs(), "endSeconds": end.secs() }))
    }

    fn trim_edges_to(&mut self, clip_ids: &[String], edge: Edge, at: Time) -> Res<Outcome> {
        if clip_ids.is_empty() {
            return Err(EditError::new("NO_CLIPS", "clipIds must not be empty."));
        }
        // A clip and its companion named together must be trimmed once, not twice.
        let mut done: Vec<String> = Vec::new();
        for clip_id in clip_ids {
            if done.contains(clip_id) {
                continue;
            }
            let pair = self.project.with_companions(std::slice::from_ref(clip_id));
            self.trim_edge_to(clip_id, edge, at)?;
            done.extend(pair);
        }
        outcome(format!("Trimmed {} clip(s) to {at}s.", done.len()), json!({ "clipIds": done, "edge": edge, "atSeconds": at.secs() }))
    }

    fn ripple_trim_clip(&mut self, input: &RippleTrimClip) -> Res<Outcome> {
        let ids = self.project.with_companions(std::slice::from_ref(&input.clip_id));
        let mut delta = Time::ZERO;
        for clip_id in &ids {
            let (track_index, _) = self.project.locate(clip_id)?;
            let clip = self.project.clip(clip_id)?.clone();
            let old_end = clip.end();
            match input.edge {
                Edge::End => {
                    delta = input.at_seconds - old_end;
                    self.set_span(clip_id, clip.start_seconds, input.at_seconds)?;
                }
                Edge::Start => {
                    // The head is cut (or restored) and the clip stays where it starts;
                    // everything after it follows.
                    delta = clip.start_seconds - input.at_seconds;
                    self.set_span(clip_id, input.at_seconds, old_end)?;
                    self.clip_mut(clip_id)?.start_seconds = clip.start_seconds;
                }
            }
            self.shift_from(track_index, old_end, delta)?;
        }
        outcome(format!("Ripple-trimmed clip {}.", input.clip_id), json!({ "clipIds": ids, "edge": input.edge, "rippleSeconds": delta.secs() }))
    }

    fn roll_edit(&mut self, input: &RollEdit) -> Res<Outcome> {
        let left = self.project.clip(&input.left_clip_id)?.clone();
        let right = self.project.clip(&input.right_clip_id)?.clone();
        if left.track_id != right.track_id || left.end() != right.start_seconds {
            return Err(EditError::new("CLIPS_NOT_ADJACENT", "A roll edit needs two clips that touch on the same track.")
                .with("leftClipId", input.left_clip_id.clone())
                .with("rightClipId", input.right_clip_id.clone()));
        }
        self.set_span_linked(&input.left_clip_id, left.start_seconds, input.edit_point_seconds)?;
        self.set_span_linked(&input.right_clip_id, input.edit_point_seconds, right.end())?;
        outcome(format!("Rolled the edit to {}s.", input.edit_point_seconds), json!({ "leftClipId": input.left_clip_id, "rightClipId": input.right_clip_id, "editPointSeconds": input.edit_point_seconds.secs() }))
    }

    fn slip_clip(&mut self, input: &SlipClip) -> Res<Outcome> {
        let clip = self.project.clip(&input.clip_id)?.clone();
        let source_start = match (input.source_start_seconds, input.delta_source_seconds) {
            (Some(_), Some(_)) => return Err(EditError::new("CONFLICTING_SLIP", "Give sourceStartSeconds or deltaSourceSeconds, not both.")),
            (Some(source_start), None) => source_start,
            (None, Some(delta)) => clip.source_start() + delta,
            (None, None) => return Err(EditError::new("SLIP_TARGET_REQUIRED", "Give sourceStartSeconds or deltaSourceSeconds.")),
        };
        let ids = self.project.with_companions(std::slice::from_ref(&input.clip_id));
        for clip_id in &ids {
            self.clip_mut(clip_id)?.source_start_seconds = Some(source_start);
        }
        outcome(format!("Slipped clip {}.", input.clip_id), json!({ "clipIds": ids, "sourceStartSeconds": source_start.secs() }))
    }

    fn slide_clip(&mut self, input: &SlideClip) -> Res<Outcome> {
        let clip = self.project.clip(&input.clip_id)?.clone();
        let delta = match (input.start_seconds, input.delta_seconds) {
            (Some(_), Some(_)) => return Err(EditError::new("CONFLICTING_SLIDE", "Give startSeconds or deltaSeconds, not both.")),
            (Some(start), None) => start - clip.start_seconds,
            (None, Some(delta)) => delta,
            (None, None) => return Err(EditError::new("SLIDE_TARGET_REQUIRED", "Give startSeconds or deltaSeconds.")),
        };
        let track = self.project.track(&clip.track_id)?;
        let left = track.clips.iter().find(|other| other.end() == clip.start_seconds).cloned();
        let right = track.clips.iter().find(|other| other.start_seconds == clip.end()).cloned();
        for (named, found, side) in [(&input.left_clip_id, &left, "left"), (&input.right_clip_id, &right, "right")] {
            if let Some(named) = named {
                if found.as_ref().map(|clip| &clip.id) != Some(named) {
                    return Err(EditError::new("CLIPS_NOT_ADJACENT", format!("Clip {named} does not touch the {side} edge of {}.", input.clip_id)).with("clipId", named.clone()));
                }
            }
        }
        let ids = self.project.with_companions(std::slice::from_ref(&input.clip_id));
        if let Some(left) = &left {
            self.set_span_linked(&left.id, left.start_seconds, left.end() + delta)?;
        }
        if let Some(right) = &right {
            self.set_span_linked(&right.id, right.start_seconds + delta, right.end())?;
        }
        for clip_id in &ids {
            let moved = self.clip_mut(clip_id)?;
            moved.start_seconds += delta;
        }
        outcome(
            format!("Slid clip {} by {delta}s.", input.clip_id),
            json!({ "clipIds": ids, "deltaSeconds": delta.secs(), "leftClipId": left.map(|clip| clip.id), "rightClipId": right.map(|clip| clip.id) }),
        )
    }

    /// Splits each named clip and its companion at one time. Returns the new clip ids.
    fn split_linked(&mut self, clip_ids: &[String], at: Time) -> Res<Vec<String>> {
        let mut created = Vec::new();
        for clip_id in self.project.with_companions(clip_ids) {
            created.push(self.split(&clip_id, at)?);
        }
        Ok(created)
    }

    fn blade_at_time(&mut self, input: &BladeAtTime) -> Res<Outcome> {
        let at = input.at_seconds;
        let targets: Vec<String> = if let Some(clip_ids) = &input.clip_ids {
            for clip_id in clip_ids {
                self.project.locate(clip_id)?;
            }
            clip_ids.clone()
        } else {
            let track_indexes: Vec<usize> = match &input.track_ids {
                Some(track_ids) => track_ids.iter().map(|track_id| self.project.track_index(track_id)).collect::<Res<_>>()?,
                None => (0..self.project.tracks.len()).filter(|index| !self.project.tracks[*index].is_locked()).collect(),
            };
            track_indexes
                .into_iter()
                .flat_map(|index| self.project.tracks[index].clips.iter().filter(|clip| clip.contains(at)).map(|clip| clip.id.clone()).collect::<Vec<_>>())
                .collect()
        };
        // With no explicit clip list, cut what is under the blade and nothing else.
        let explicit = input.clip_ids.is_some();
        let cut: Vec<String> = self
            .project
            .with_companions(&targets)
            .into_iter()
            .filter(|clip_id| explicit || self.project.clip(clip_id).is_ok_and(|clip| clip.contains(at)))
            .collect();
        let mut created = Vec::new();
        for clip_id in &cut {
            created.push(self.split(clip_id, at)?);
        }
        outcome(format!("Cut {} clip(s) at {at}s.", cut.len()), json!({ "atSeconds": at.secs(), "splitClipIds": cut, "createdClipIds": created }))
    }

    fn delete_clips(&mut self, clip_ids: &[String], ripple: bool) -> Res<Outcome> {
        if clip_ids.is_empty() {
            return Err(EditError::new("NO_CLIPS", "clipIds must not be empty."));
        }
        for clip_id in clip_ids {
            self.project.locate(clip_id)?;
        }
        let ids = self.project.with_companions(clip_ids);
        for clip_id in &ids {
            let (track_index, _) = self.project.locate(clip_id)?;
            let clip = self.take(clip_id)?;
            if ripple {
                self.shift_from(track_index, clip.end(), -clip.duration_seconds)?;
            }
        }
        outcome(format!("Deleted {} clip(s).", ids.len()), json!({ "deletedClipIds": ids, "ripple": ripple }))
    }

    fn close_gap_at_time(&mut self, input: &CloseGapAtTime) -> Res<Outcome> {
        let track_index = self.project.track_index(&input.track_id)?;
        let at = input.at_seconds;
        let track = &self.project.tracks[track_index];
        let no_gap = || EditError::new("NO_GAP_AT_TIME", format!("There is no gap at {at}s on track {}.", input.track_id)).with("trackId", input.track_id.clone()).with("atSeconds", at.secs());
        if track.clips.iter().any(|clip| clip.start_seconds <= at && at < clip.end()) {
            return Err(no_gap());
        }
        let gap_start = track.clips.iter().map(Clip::end).filter(|end| *end <= at).max().unwrap_or(Time::ZERO);
        let Some(gap_end) = track.clips.iter().map(|clip| clip.start_seconds).filter(|start| *start > at).min() else {
            return Err(no_gap());
        };
        self.shift_from(track_index, gap_end, gap_start - gap_end)?;
        outcome(format!("Closed a {}s gap on track {}.", gap_end - gap_start, input.track_id), json!({ "trackId": input.track_id, "gapStartSeconds": gap_start.secs(), "gapEndSeconds": gap_end.secs() }))
    }

    fn range_edit(&mut self, input: &RangeEdit, close: bool) -> Res<Outcome> {
        let (start, end) = (non_negative("startSeconds", input.start_seconds)?, input.end_seconds);
        if end <= start {
            return Err(EditError::new("INVALID_RANGE", "endSeconds must be after startSeconds."));
        }
        let mut track_indexes: Vec<usize> = match &input.track_ids {
            Some(track_ids) => track_ids.iter().map(|track_id| self.project.track_index(track_id)).collect::<Res<_>>()?,
            None => (0..self.project.tracks.len()).filter(|index| !self.project.tracks[*index].is_locked()).collect(),
        };
        if input.include_linked_companions.unwrap_or(true) {
            let touched: Vec<String> = track_indexes
                .iter()
                .flat_map(|index| self.project.tracks[*index].clips.iter().filter(|clip| clip.intersects(start, end)).map(|clip| clip.id.clone()).collect::<Vec<_>>())
                .collect();
            for clip_id in self.project.with_companions(&touched) {
                let (track_index, _) = self.project.locate(&clip_id)?;
                if !track_indexes.contains(&track_index) {
                    track_indexes.push(track_index);
                }
            }
        }
        let mut removed = Vec::new();
        for track_index in &track_indexes {
            removed.extend(self.clear_range(*track_index, start, end)?);
            if close {
                self.shift_from(*track_index, end, start - end)?;
            }
        }
        let track_ids: Vec<String> = track_indexes.iter().map(|index| self.project.tracks[*index].id.clone()).collect();
        outcome(
            format!("{} {}s..{}s on {} track(s).", if close { "Extracted" } else { "Lifted" }, start, end, track_ids.len()),
            json!({ "startSeconds": start.secs(), "endSeconds": end.secs(), "trackIds": track_ids, "removedClipIds": removed }),
        )
    }

    fn set_clip_properties(&mut self, clip_id: &str, patch: &ClipProperties) -> Res<Outcome> {
        let companion = self.project.companion_id(clip_id);
        merge_properties(&mut self.clip_mut(clip_id)?.properties, patch);
        // Speed is part of what keeps a linked pair in step, so it applies to both.
        if let (Some(speed), Some(companion)) = (patch.speed, &companion) {
            self.clip_mut(companion)?.properties.speed = Some(speed);
        }
        outcome(format!("Updated clip {clip_id}."), json!({ "clipId": clip_id, "linkedClipId": companion }))
    }

    fn add_captions(&mut self, input: &AddCaptions) -> Res<Outcome> {
        if input.captions.is_empty() {
            return Err(EditError::new("NO_CAPTIONS", "captions must not be empty."));
        }
        let default_track = match &input.track_id {
            Some(track_id) => Some(track_id.clone()),
            None => self.project.tracks.iter().find(|track| track.kind == TrackKind::Caption).map(|track| track.id.clone()),
        };
        let mut created = Vec::new();
        for item in &input.captions {
            let Some(track_id) = item.track_id.clone().or(default_track.clone()) else {
                return Err(EditError::new("NO_CAPTION_TRACK", "The project has no caption track; add one first."));
            };
            let start = non_negative("startSeconds", item.start_seconds)?;
            let duration = match (item.duration_seconds, item.end_seconds) {
                (Some(_), Some(_)) => return Err(EditError::new("CONFLICTING_CAPTION_TIMING", "Give durationSeconds or endSeconds, not both.")),
                (Some(duration), None) => duration,
                (None, Some(end)) => end - start,
                (None, None) => DEFAULT_CAPTION,
            };
            positive("durationSeconds", duration)?;
            if let Some(id) = &item.id {
                if self.project.locate(id).is_ok() {
                    return Err(EditError::new("DUPLICATE_CLIP_ID", format!("Clip id {id} already exists.")).with("clipId", id.clone()));
                }
            }
            let id = item.id.clone().unwrap_or_else(|| self.project.fresh_id("caption-", |candidate| self.project.clips().any(|clip| clip.id == candidate)));
            let name: String = item.text.chars().take(40).collect();
            self.put(Clip {
                id: id.clone(),
                track_id,
                kind: ClipKind::Caption,
                name,
                start_seconds: start,
                duration_seconds: duration,
                media_id: None,
                source_start_seconds: None,
                properties: item.properties.clone().unwrap_or_default(),
                caption: Some(CaptionText { text: item.text.clone(), speaker: item.speaker.clone(), language: item.language.clone() }),
                link: None,
            })?;
            created.push(id);
        }
        outcome(format!("Added {} caption(s).", created.len()), json!({ "clipIds": created }))
    }
}
