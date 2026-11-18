//! Turning "put this file on the timeline" into operations.
//!
//! The editor's Add/drop and the MCP import tool both call this, so a file lands the
//! same way whoever places it.

use crate::error::{EditError, Res};
use crate::model::{ClipProperties, EditMode, MediaType, Project, TrackKind};
use crate::ops::{AddClip, AddClips, AddTrack, LinkedAudio, Operation};
use crate::time::Time;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    pub media_id: String,
    pub media_type: MediaType,
    /// Whether a video file carries sound; such a clip gets a linked audio clip.
    pub has_audio: bool,
    pub track_id: String,
    pub start_seconds: Time,
    #[serde(default)]
    pub duration_seconds: Option<Time>,
    #[serde(default)]
    pub source_start_seconds: Option<Time>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub properties: Option<ClipProperties>,
    /// Audio track for the linked sound. When omitted, the first unlocked audio track
    /// that is free over the clip's range is used, and a new one is added if none is.
    #[serde(default)]
    pub linked_audio_track_id: Option<String>,
    #[serde(default)]
    pub edit_mode: EditMode,
}

/// The operations that place one media item. `duration` is the timeline length the
/// clip will have (the caller knows the media length; it is needed to find a free
/// audio track).
pub fn operations(project: &Project, placement: &Placement, duration: Time) -> Res<Vec<Operation>> {
    let mut operations = Vec::new();
    let mut linked_audio = None;
    if placement.media_type == MediaType::Video && placement.has_audio {
        let (start, end) = (placement.start_seconds, placement.start_seconds + duration);
        let track_id = match &placement.linked_audio_track_id {
            Some(track_id) => {
                let track = project.track(track_id)?;
                if track.kind != TrackKind::Audio {
                    return Err(EditError::new("INCOMPATIBLE_MEDIA_TRACK", format!("Track {track_id} is not an audio track.")).with("trackId", track_id.clone()));
                }
                track_id.clone()
            }
            None => {
                let free = project.tracks.iter().find(|track| {
                    track.kind == TrackKind::Audio
                        && !track.is_locked()
                        // With insert or overwrite the edit makes its own room.
                        && (placement.edit_mode != EditMode::Strict || !track.clips.iter().any(|clip| clip.intersects(start, end)))
                });
                match free {
                    Some(track) => track.id.clone(),
                    None => {
                        let id = project.fresh_id("A", |candidate| project.tracks.iter().any(|track| track.id == candidate));
                        operations.push(Operation::AddTrack(AddTrack {
                            id: Some(id.clone()),
                            kind: TrackKind::Audio,
                            name: None,
                            insert_before_track_id: None,
                            insert_after_track_id: None,
                            locked: None,
                            muted: None,
                        }));
                        id
                    }
                }
            }
        };
        linked_audio = Some(LinkedAudio { id: None, track_id, name: None, properties: None });
    }
    operations.push(Operation::AddClips(AddClips {
        clips: vec![AddClip {
            id: None,
            track_id: placement.track_id.clone(),
            media_id: placement.media_id.clone(),
            start_seconds: placement.start_seconds,
            duration_seconds: placement.duration_seconds,
            source_start_seconds: placement.source_start_seconds,
            name: placement.name.clone(),
            properties: placement.properties.clone(),
            linked_audio,
        }],
        edit_mode: placement.edit_mode,
    }));
    Ok(operations)
}
