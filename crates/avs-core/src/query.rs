//! Read-only views of a project for tools and the editor.

use crate::error::{EditError, Res};
use crate::model::{Clip, ClipKind, MediaAsset, MediaType, Project, Resolution, Track, TrackKind};
use crate::time::Time;
use crate::validate::{self, Issue};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineSnapshot {
    pub project_id: String,
    pub title: String,
    pub fps: f64,
    pub resolution: Resolution,
    pub sample_rate: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playhead_seconds: Option<Time>,
    pub duration_seconds: Time,
    pub tracks: Vec<Track>,
}

pub fn snapshot(project: &Project) -> TimelineSnapshot {
    TimelineSnapshot {
        project_id: project.id.clone(),
        title: project.title.clone(),
        fps: project.fps,
        resolution: project.resolution,
        sample_rate: project.sample_rate,
        playhead_seconds: project.playhead_seconds,
        duration_seconds: project.duration(),
        tracks: project.tracks.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackSummary {
    pub id: String,
    pub name: String,
    pub kind: TrackKind,
    pub locked: bool,
    pub muted: bool,
    pub clip_count: usize,
    pub first_start_seconds: Option<Time>,
    pub last_end_seconds: Option<Time>,
    /// Time covered by clips on this track (gaps excluded).
    pub duration_seconds: Time,
}

pub fn track_summaries(project: &Project) -> Vec<TrackSummary> {
    project
        .tracks
        .iter()
        .map(|track| TrackSummary {
            id: track.id.clone(),
            name: track.name.clone(),
            kind: track.kind,
            locked: track.is_locked(),
            muted: track.is_muted(),
            clip_count: track.clips.len(),
            first_start_seconds: track.clips.iter().map(|clip| clip.start_seconds).min(),
            last_end_seconds: track.clips.iter().map(Clip::end).max(),
            duration_seconds: track.clips.iter().fold(Time::ZERO, |total, clip| total + clip.duration_seconds),
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaUsage {
    pub media_id: String,
    pub clip_ids: Vec<String>,
    pub total_duration_seconds: Time,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub project_id: String,
    pub title: String,
    pub fps: f64,
    pub resolution: Resolution,
    pub duration_seconds: Time,
    pub media_count: usize,
    pub track_count: usize,
    pub clip_count: usize,
    pub caption_count: usize,
    pub tracks: Vec<TrackSummary>,
    pub media_usage: Vec<MediaUsage>,
    pub issues: Vec<Issue>,
}

pub fn inspect(project: &Project) -> Inspection {
    let media_usage = project
        .media
        .iter()
        .map(|media| {
            let users: Vec<&Clip> = project.clips().filter(|clip| clip.media_id.as_deref() == Some(&media.id)).collect();
            MediaUsage {
                media_id: media.id.clone(),
                clip_ids: users.iter().map(|clip| clip.id.clone()).collect(),
                total_duration_seconds: users.iter().fold(Time::ZERO, |total, clip| total + clip.duration_seconds),
            }
        })
        .collect();
    Inspection {
        project_id: project.id.clone(),
        title: project.title.clone(),
        fps: project.fps,
        resolution: project.resolution,
        duration_seconds: project.duration(),
        media_count: project.media.len(),
        track_count: project.tracks.len(),
        clip_count: project.clips().count(),
        caption_count: project.clips().filter(|clip| clip.kind == ClipKind::Caption).count(),
        tracks: track_summaries(project),
        media_usage,
        issues: validate::issues(project),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaSearch {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub types: Option<Vec<MediaType>>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub max_results: Option<usize>,
}

/// Case-insensitive search over name, tags, transcript and id. Every word of `text`
/// must appear somewhere; every tag in `tags` must be present.
pub fn search_media(project: &Project, query: &MediaSearch) -> Vec<MediaAsset> {
    let words: Vec<String> = query.text.as_deref().unwrap_or("").split_whitespace().map(str::to_lowercase).collect();
    let wanted_tags: Vec<String> = query.tags.clone().unwrap_or_default().iter().map(|tag| tag.to_lowercase()).collect();
    project
        .media
        .iter()
        .filter(|media| query.types.as_ref().is_none_or(|types| types.contains(&media.media_type)))
        .filter(|media| {
            let tags: Vec<String> = media.tags.clone().unwrap_or_default().iter().map(|tag| tag.to_lowercase()).collect();
            wanted_tags.iter().all(|tag| tags.contains(tag))
        })
        .filter(|media| {
            let haystack = format!("{} {} {} {}", media.id, media.name, media.tags.clone().unwrap_or_default().join(" "), media.transcript.as_deref().unwrap_or("")).to_lowercase();
            words.iter().all(|word| haystack.contains(word))
        })
        .take(query.max_results.unwrap_or(usize::MAX))
        .cloned()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectClipsInRange {
    pub start_seconds: Time,
    pub end_seconds: Time,
    #[serde(default)]
    pub track_ids: Option<Vec<String>>,
    #[serde(default)]
    pub include_linked_companions: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedPair {
    pub clip_id: String,
    pub linked_clip_id: String,
    pub linked_group_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectedClips {
    pub range_start_seconds: Time,
    pub range_end_seconds: Time,
    pub range_duration_seconds: Time,
    pub track_ids: Vec<String>,
    pub include_linked_companions: bool,
    /// Clips on the chosen tracks that intersect the range.
    pub direct_clip_ids: Vec<String>,
    /// Link companions of those clips that sit on other tracks.
    pub linked_companion_clip_ids: Vec<String>,
    pub clip_ids: Vec<String>,
    pub linked_pairs: Vec<LinkedPair>,
}

pub fn collect_clips_in_range(project: &Project, input: &CollectClipsInRange) -> Res<CollectedClips> {
    if input.start_seconds.is_negative() || input.end_seconds <= input.start_seconds {
        return Err(EditError::new("INVALID_RANGE", "endSeconds must be after a non-negative startSeconds."));
    }
    let track_ids: Vec<String> = match &input.track_ids {
        Some(track_ids) => {
            for track_id in track_ids {
                project.track_index(track_id)?;
            }
            track_ids.clone()
        }
        None => project.tracks.iter().map(|track| track.id.clone()).collect(),
    };
    let include = input.include_linked_companions.unwrap_or(true);
    let direct: Vec<&Clip> = project
        .tracks
        .iter()
        .filter(|track| track_ids.contains(&track.id))
        .flat_map(|track| track.clips.iter())
        .filter(|clip| clip.intersects(input.start_seconds, input.end_seconds))
        .collect();
    let direct_clip_ids: Vec<String> = direct.iter().map(|clip| clip.id.clone()).collect();
    let mut linked_companion_clip_ids = Vec::new();
    let mut linked_pairs = Vec::new();
    for clip in &direct {
        let (Some(link), Some(companion)) = (&clip.link, project.companion_id(&clip.id)) else { continue };
        linked_pairs.push(LinkedPair { clip_id: clip.id.clone(), linked_clip_id: companion.clone(), linked_group_id: link.group_id.clone() });
        if include && !direct_clip_ids.contains(&companion) && !linked_companion_clip_ids.contains(&companion) {
            linked_companion_clip_ids.push(companion);
        }
    }
    let mut clip_ids = direct_clip_ids.clone();
    clip_ids.extend(linked_companion_clip_ids.iter().cloned());
    Ok(CollectedClips {
        range_start_seconds: input.start_seconds,
        range_end_seconds: input.end_seconds,
        range_duration_seconds: input.end_seconds - input.start_seconds,
        track_ids,
        include_linked_companions: include,
        direct_clip_ids,
        linked_companion_clip_ids,
        clip_ids,
        linked_pairs,
    })
}
