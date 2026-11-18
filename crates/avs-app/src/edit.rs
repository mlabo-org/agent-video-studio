//! Turning editor gestures and commands into `avs_core::Operation`s.
//!
//! Pure: everything takes the project copy and returns operations. The UI applies
//! them to its copy for the live preview of a drag and sends them to the session
//! when the gesture ends, so a gesture is always exactly one undo step.

use crate::geom;
use avs_core::ops::*;
use avs_core::placement::{self, Placement};
use avs_core::{Clip, ClipKind, ClipProperties, Edge, EditMode, MediaAsset, MediaType, Project, Time, TrackKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Select,
    Trim,
    Blade,
    Range,
}

/// Which part of a clip the pointer is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Body,
    Start,
    End,
}

/// What a drag on the timeline does.
#[derive(Debug, Clone, PartialEq)]
pub enum DragKind {
    /// Move the clips (already expanded with link companions). `anchor` is the clip
    /// under the pointer; it alone may change track.
    Move { clip_ids: Vec<String>, anchor: String },
    Trim { clip_id: String, edge: Edge, ripple: bool },
    Roll { left: String, right: String },
    Slip { clip_id: String },
    Slide { clip_id: String },
}

/// Decides the gesture from the tool, the zone under the pointer and ⌥.
///
/// - Select: body moves, an edge trims; ⌥ makes an edge trim ripple and a body drag slide.
/// - Trim: an edge ripples, or rolls when another clip touches that edge; the body
///   slips, ⌥ + body slides.
pub fn drag_kind(project: &Project, tool: Tool, alt: bool, clip_id: &str, zone: Zone, selection: &[String]) -> Option<DragKind> {
    let clip = project.clip(clip_id).ok()?;
    let edge = match zone {
        Zone::Start => Some(Edge::Start),
        Zone::End => Some(Edge::End),
        Zone::Body => None,
    };
    match (tool, edge) {
        (Tool::Select, Some(edge)) => Some(DragKind::Trim { clip_id: clip_id.to_string(), edge, ripple: alt }),
        (Tool::Select, None) if alt => Some(DragKind::Slide { clip_id: clip_id.to_string() }),
        (Tool::Select, None) => {
            let mut ids: Vec<String> = if selection.iter().any(|id| id == clip_id) { selection.to_vec() } else { vec![clip_id.to_string()] };
            ids.retain(|id| project.clip(id).is_ok());
            Some(DragKind::Move { clip_ids: project.with_companions(&ids), anchor: clip_id.to_string() })
        }
        (Tool::Trim, Some(edge)) => {
            let track = project.track(&clip.track_id).ok()?;
            let neighbour = match edge {
                Edge::End => track.clips.iter().find(|other| other.id != clip.id && other.start_seconds == clip.end()).map(|other| (clip.id.clone(), other.id.clone())),
                Edge::Start => track.clips.iter().find(|other| other.id != clip.id && other.end() == clip.start_seconds).map(|other| (other.id.clone(), clip.id.clone())),
            };
            match neighbour {
                Some((left, right)) => Some(DragKind::Roll { left, right }),
                None => Some(DragKind::Trim { clip_id: clip_id.to_string(), edge, ripple: true }),
            }
        }
        (Tool::Trim, None) if alt => Some(DragKind::Slide { clip_id: clip_id.to_string() }),
        (Tool::Trim, None) => Some(DragKind::Slip { clip_id: clip_id.to_string() }),
        (Tool::Blade | Tool::Range, _) => None,
    }
}

/// Times a moving edge sticks to: other clips' edges, the playhead, the range and 0.
pub fn snap_targets(project: &Project, exclude: &[String], playhead: Time, range: Option<(Time, Time)>) -> Vec<Time> {
    let mut targets = vec![Time::ZERO, playhead];
    if let Some((start, end)) = range {
        targets.push(start);
        targets.push(end);
    }
    for clip in project.clips().filter(|clip| !exclude.contains(&clip.id)) {
        targets.push(clip.start_seconds);
        targets.push(clip.end());
    }
    targets.sort();
    targets.dedup();
    targets
}

/// Adjusts `delta` so the moving edge closest to a target lands exactly on it.
/// Returns the adjusted delta and the target it snapped to.
pub fn snap_delta(edges: &[Time], delta: Time, targets: &[Time], threshold: Time) -> (Time, Option<Time>) {
    // (distance, adjusted delta, target)
    let mut best: Option<(i64, Time, Time)> = None;
    for edge in edges {
        let moved = *edge + delta;
        for target in targets {
            let distance = (*target - moved).micros().abs();
            if distance <= threshold.micros() && best.is_none_or(|(current, _, _)| distance < current) {
                best = Some((distance, *target - *edge, *target));
            }
        }
    }
    match best {
        Some((_, snapped, target)) => (snapped, Some(target)),
        None => (delta, None),
    }
}

/// The length of the media a clip plays, when it has one (a still or a caption
/// can be shown for any duration).
fn clip_media_length(project: &Project, clip: &Clip) -> Option<Time> {
    let media = project.media(clip.media_id.as_deref()?).ok()?;
    (!media.media_type.is_still()).then_some(media.duration_seconds)
}

/// The earliest and latest timeline time an edge of `clip` can be dragged to: the
/// clip keeps at least one frame, and never plays media before its first frame or
/// after its last. A drag stops there instead of failing when it is released.
pub fn edge_bounds(project: &Project, clip: &Clip, edge: Edge, fps: f64) -> (Time, Time) {
    let frame = geom::frame_time(1, fps);
    let speed = clip.speed().max(f64::EPSILON);
    let length = clip_media_length(project, clip);
    match edge {
        Edge::Start => {
            let earliest = match length {
                Some(_) => clip.start_seconds - clip.source_start().scale(1.0 / speed),
                None => Time::ZERO,
            };
            (earliest.max(Time::ZERO), clip.end() - frame)
        }
        Edge::End => {
            let latest = match length {
                Some(length) => clip.start_seconds + (length - clip.source_start()).scale(1.0 / speed),
                None => Time::from_micros(i64::MAX / 4),
            };
            (clip.start_seconds + frame, latest)
        }
    }
}

/// `at` held inside `bounds`; the snap line is dropped when the bound won.
fn hold(at: Time, snapped: Option<Time>, bounds: (Time, Time)) -> (Time, Option<Time>) {
    let held = at.max(bounds.0).min(bounds.1.max(bounds.0));
    (held, if held == at { snapped } else { None })
}

/// Inputs that turn a drag into an operation.
#[derive(Debug, Clone)]
pub struct DragInput<'a> {
    pub project: &'a Project,
    pub kind: &'a DragKind,
    /// Pointer movement in timeline time since the press.
    pub raw_delta: Time,
    /// Track under the pointer (for a move to another track).
    pub hover_track: Option<&'a str>,
    pub fps: f64,
    pub snap: bool,
    pub snap_threshold: Time,
    pub playhead: Time,
    pub range: Option<(Time, Time)>,
}

/// The operation for a drag at its current position, plus the snap line to draw.
pub fn drag_operation(input: &DragInput) -> Option<(Vec<Operation>, Option<Time>)> {
    let project = input.project;
    let fps = input.fps;
    match input.kind {
        DragKind::Move { clip_ids, anchor } => {
            let anchor_clip = project.clip(anchor).ok()?;
            let mut delta = geom::quantize(anchor_clip.start_seconds + input.raw_delta, fps) - anchor_clip.start_seconds;
            let mut snapped = None;
            if input.snap {
                let edges: Vec<Time> = clip_ids.iter().filter_map(|id| project.clip(id).ok()).flat_map(|clip| [clip.start_seconds, clip.end()]).collect();
                let targets = snap_targets(project, clip_ids, input.playhead, input.range);
                (delta, snapped) = snap_delta(&edges, delta, &targets, input.snap_threshold);
            }
            let earliest = clip_ids.iter().filter_map(|id| project.clip(id).ok()).map(|clip| clip.start_seconds).min()?;
            if earliest + delta < Time::ZERO {
                delta = -earliest;
            }
            let track_change = input.hover_track.filter(|track_id| *track_id != anchor_clip.track_id);
            if let Some(target) = track_change
                && let Some(operations) = track_moves(project, clip_ids, anchor, target, delta) {
                    return Some((operations, snapped));
                }
            if delta == Time::ZERO {
                return None;
            }
            Some((vec![Operation::MoveClips(MoveClips { clip_ids: clip_ids.clone(), start_seconds: None, delta_seconds: Some(delta), edit_mode: EditMode::Strict })], snapped))
        }
        DragKind::Trim { clip_id, edge, ripple } => {
            let clip = project.clip(clip_id).ok()?;
            let original = match edge {
                Edge::Start => clip.start_seconds,
                Edge::End => clip.end(),
            };
            let (at, snapped) = snapped_point(input, original, clip_id);
            let (at, snapped) = hold(at, snapped, edge_bounds(project, clip, *edge, fps));
            if at == original {
                return None;
            }
            let operation = match (edge, ripple) {
                (_, true) => Operation::RippleTrimClip(RippleTrimClip { clip_id: clip_id.clone(), edge: *edge, at_seconds: at }),
                (Edge::Start, false) => Operation::TrimClip(TrimClip { clip_id: clip_id.clone(), start_seconds: Some(at), end_seconds: None, duration_seconds: None, source_start_seconds: None }),
                (Edge::End, false) => Operation::TrimClip(TrimClip { clip_id: clip_id.clone(), start_seconds: None, end_seconds: Some(at), duration_seconds: None, source_start_seconds: None }),
            };
            Some((vec![operation], snapped))
        }
        DragKind::Roll { left, right } => {
            let left_clip = project.clip(left).ok()?;
            let right_clip = project.clip(right).ok()?;
            let (at, snapped) = snapped_point(input, left_clip.end(), left);
            // The edit point stops where either clip runs out of media or of length.
            let (left_bounds, right_bounds) = (edge_bounds(project, left_clip, Edge::End, fps), edge_bounds(project, right_clip, Edge::Start, fps));
            let (at, snapped) = hold(at, snapped, (left_bounds.0.max(right_bounds.0), left_bounds.1.min(right_bounds.1)));
            if at == left_clip.end() {
                return None;
            }
            Some((vec![Operation::RollEdit(RollEdit { left_clip_id: left.clone(), right_clip_id: right.clone(), edit_point_seconds: at })], snapped))
        }
        DragKind::Slip { clip_id } => {
            let clip = project.clip(clip_id).ok()?;
            let frames = geom::frame_index(input.raw_delta.max(Time::ZERO), fps, true) - geom::frame_index((-input.raw_delta).max(Time::ZERO), fps, true);
            if frames == 0 {
                return None;
            }
            // Dragging right shows earlier material, as if the media slid right under the clip.
            let delta_source = -geom::frame_time(frames, fps).scale(clip.speed());
            // The clip's window stays inside the media.
            let last_start = clip_media_length(project, clip).map_or(Time::from_micros(i64::MAX / 4), |length| length - clip.duration_seconds.scale(clip.speed()));
            let source_start = (clip.source_start() + delta_source).min(last_start).max(Time::ZERO);
            if source_start == clip.source_start() {
                return None;
            }
            Some((vec![Operation::SlipClip(SlipClip { clip_id: clip_id.clone(), source_start_seconds: Some(source_start), delta_source_seconds: None })], None))
        }
        DragKind::Slide { clip_id } => {
            let clip = project.clip(clip_id).ok()?;
            let mut delta = geom::quantize(clip.start_seconds + input.raw_delta, fps) - clip.start_seconds;
            let mut snapped = None;
            if input.snap {
                let ids = project.with_companions(std::slice::from_ref(clip_id));
                let targets = snap_targets(project, &ids, input.playhead, input.range);
                (delta, snapped) = snap_delta(&[clip.start_seconds, clip.end()], delta, &targets, input.snap_threshold);
            }
            if delta == Time::ZERO {
                return None;
            }
            Some((vec![Operation::SlideClip(SlideClip { clip_id: clip_id.clone(), start_seconds: None, delta_seconds: Some(delta), left_clip_id: None, right_clip_id: None })], snapped))
        }
    }
}

/// Moving clips up or down: every clip on a track of the anchor's kind moves by
/// the same number of tracks of that kind (and all by `delta` in time); linked
/// audio stays on its track and follows in time. One `MoveClip` per clip, sent
/// together as one revision. `None` when some clip would land outside the tracks
/// or on a locked one.
pub fn track_moves(project: &Project, clip_ids: &[String], anchor: &str, target: &str, delta: Time) -> Option<Vec<Operation>> {
    let anchor_track = project.track(&project.clip(anchor).ok()?.track_id).ok()?;
    let kind = anchor_track.kind;
    let lane: Vec<&str> = project.tracks.iter().filter(|track| track.kind == kind).map(|track| track.id.as_str()).collect();
    let from = lane.iter().position(|id| *id == anchor_track.id)? as i64;
    let to = lane.iter().position(|id| *id == target)? as i64;
    let offset = to - from;
    if offset == 0 {
        return None;
    }
    // The clip that names each move: the picture of a linked pair (its sound follows).
    let leads: Vec<&Clip> = clip_ids
        .iter()
        .filter_map(|id| project.clip(id).ok())
        .filter(|clip| !(clip.link.as_ref().is_some_and(|link| link.role == avs_core::LinkRole::Audio) && project.companion_id(&clip.id).is_some_and(|other| clip_ids.contains(&other))))
        .collect();
    let mut moves: Vec<(&Clip, Option<String>, usize)> = Vec::new();
    for clip in leads {
        let track_index = project.track_index(&clip.track_id).ok()?;
        let track = &project.tracks[track_index];
        if track.kind != kind {
            moves.push((clip, None, track_index));
            continue;
        }
        let position = lane.iter().position(|id| *id == track.id)? as i64 + offset;
        let new_track = project.track(lane.get(usize::try_from(position).ok()?)?).ok()?;
        if new_track.is_locked() {
            return None;
        }
        moves.push((clip, Some(new_track.id.clone()), track_index));
    }
    // Move in the direction of travel so no clip lands on one that has not moved yet.
    moves.sort_by_key(|(clip, _, track_index)| {
        let time = if delta.is_positive() { -clip.start_seconds.micros() } else { clip.start_seconds.micros() };
        let track = if offset > 0 { -(*track_index as i64) } else { *track_index as i64 };
        (time, track)
    });
    let operations: Vec<Operation> = moves
        .into_iter()
        .map(|(clip, track_id, _)| Operation::MoveClip(MoveClip { clip_id: clip.id.clone(), track_id, start_seconds: Some(clip.start_seconds + delta), edit_mode: EditMode::Strict }))
        .collect();
    (!operations.is_empty()).then_some(operations)
}

/// A move that lands on other clips of its own track: reorder instead, as a
/// magnetic timeline does. The moved clips are inserted at the clip edge nearest
/// to where they were dropped (later material moves right), and the hole they
/// left is closed. Returns the operations and the insertion point.
pub fn reorder_operations(project: &Project, clip_ids: &[String], anchor: &str, proposed_start: Time) -> Option<(Vec<Operation>, Time)> {
    // One clip or one linked pair: a larger selection spans tracks whose edges differ.
    let single = clip_ids.len() == 1 || (clip_ids.len() == 2 && project.companion_id(anchor).is_some_and(|other| clip_ids.contains(&other)));
    if !single {
        return None;
    }
    let anchor_clip = project.clip(anchor).ok()?;
    let track = project.track(&anchor_clip.track_id).ok()?;
    let moved: Vec<&Clip> = clip_ids.iter().filter_map(|id| project.clip(id).ok()).collect();
    let start = moved.iter().map(|clip| clip.start_seconds).min()?;
    let end = moved.iter().map(|clip| clip.end()).max()?;
    let length = end - start;
    let mut edges: Vec<Time> = track.clips.iter().filter(|clip| !clip_ids.contains(&clip.id)).flat_map(|clip| [clip.start_seconds, clip.end()]).collect();
    edges.push(Time::ZERO);
    let insert_at = edges.into_iter().filter(|edge| *edge != start).min_by_key(|edge| (*edge - proposed_start).micros().abs())?;
    // Dropping at its own place (just before the next clip) is no move.
    if insert_at == end && track.clips.iter().any(|clip| !clip_ids.contains(&clip.id) && clip.start_seconds == end) {
        return None;
    }
    let first = Operation::MoveClips(MoveClips { clip_ids: clip_ids.to_vec(), start_seconds: Some(insert_at), delta_seconds: None, edit_mode: EditMode::Insert });
    let mut working = avs_core::apply(project, &first).ok()?.0;
    let mut operations = vec![first];
    // Where the moved material used to be, after the insert pushed things right.
    let hole = if insert_at < start { start + length } else { start };
    let mut tracks: Vec<String> = moved.iter().map(|clip| clip.track_id.clone()).collect();
    tracks.dedup();
    for track_id in tracks {
        let close = Operation::CloseGapAtTime(CloseGapAtTime { at_seconds: hole + Time::from_micros(1), track_id });
        if let Ok((next, _)) = avs_core::apply(&working, &close) {
            working = next;
            operations.push(close);
        }
    }
    // A reorder never cuts anything; if it would (the other track is laid out
    // differently), it is not offered.
    if working.clips().count() != project.clips().count() {
        return None;
    }
    Some((operations, insert_at))
}

/// `original + raw_delta`, on a frame, snapped to the nearest target.
fn snapped_point(input: &DragInput, original: Time, clip_id: &str) -> (Time, Option<Time>) {
    let mut at = geom::quantize((original + input.raw_delta).max(Time::ZERO), input.fps);
    let mut snapped = None;
    if input.snap {
        let ids = input.project.with_companions(&[clip_id.to_string()]);
        let targets = snap_targets(input.project, &ids, input.playhead, input.range);
        let (delta, target) = snap_delta(&[original], at - original, &targets, input.snap_threshold);
        at = original + delta;
        snapped = target;
    }
    (at, snapped)
}

// ---------------------------------------------------------------- commands

/// ⌘B: cut the selected clips under the playhead, or every unlocked track when
/// nothing is selected (or no selected clip is under the playhead).
pub fn blade(project: &Project, selection: &[String], at: Time) -> Option<Operation> {
    let under: Vec<String> = selection.iter().filter(|id| project.clip(id).is_ok_and(|clip| clip.contains(at))).cloned().collect();
    if !selection.is_empty() && !under.is_empty() {
        return Some(Operation::BladeAtTime(BladeAtTime { at_seconds: at, track_ids: None, clip_ids: Some(dedupe_pairs(project, &under)) }));
    }
    let any = project.tracks.iter().filter(|track| !track.is_locked()).any(|track| track.clips.iter().any(|clip| clip.contains(at)));
    any.then_some(Operation::BladeAtTime(BladeAtTime { at_seconds: at, track_ids: None, clip_ids: None }))
}

/// Keeps one member of each linked pair (operations act on the pair anyway).
pub fn dedupe_pairs(project: &Project, ids: &[String]) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for id in ids {
        let companion = project.companion_id(id);
        if kept.contains(id) || companion.as_ref().is_some_and(|other| kept.contains(other)) {
            continue;
        }
        kept.push(id.clone());
    }
    kept
}

/// Delete: with a range, extract (ripple) or lift it; else ripple-delete or delete
/// the selected clips.
pub fn delete(project: &Project, selection: &[String], range: Option<(Time, Time)>, ripple: bool) -> Vec<Operation> {
    if let Some((start, end)) = range.filter(|(start, end)| end > start) {
        let edit = RangeEdit { start_seconds: start, end_seconds: end, track_ids: None, include_linked_companions: Some(true) };
        return vec![if ripple { Operation::ExtractRange(edit) } else { Operation::LiftRange(edit) }];
    }
    let ids: Vec<String> = selection.iter().filter(|id| project.clip(id).is_ok()).cloned().collect();
    if ids.is_empty() {
        return Vec::new();
    }
    if !ripple {
        return vec![Operation::DeleteClips(ClipRefs { clip_ids: ids })];
    }
    // Ripple later clips first, so each delete leaves the earlier ones where they are.
    let mut kept = dedupe_pairs(project, &ids);
    kept.sort_by_key(|id| std::cmp::Reverse(project.clip(id).map(|clip| clip.start_seconds).unwrap_or(Time::ZERO)));
    kept.into_iter().map(|clip_id| Operation::RippleDeleteClip(ClipRef { clip_id })).collect()
}

/// ⌥[ / ⌥]: trim the selected clips under the playhead, or every clip under it.
pub fn trim_to_playhead(project: &Project, selection: &[String], at: Time, edge: Edge) -> Option<Operation> {
    let under = |clip: &Clip| clip.contains(at) && project.track(&clip.track_id).is_ok_and(|track| !track.is_locked());
    let mut ids: Vec<String> = selection.iter().filter(|id| project.clip(id).is_ok_and(under)).cloned().collect();
    if ids.is_empty() {
        ids = project.clips().filter(|clip| under(clip)).map(|clip| clip.id.clone()).collect();
    }
    if ids.is_empty() {
        return None;
    }
    Some(Operation::TrimClipsToTime(TrimClipsToTime { at_seconds: at, edge, clip_ids: dedupe_pairs(project, &ids) }))
}

/// V: mute or unmute the selection (both halves of a linked pair).
pub fn toggle_muted(project: &Project, selection: &[String]) -> Vec<Operation> {
    let ids = project.with_companions(selection);
    let clips: Vec<&Clip> = ids.iter().filter_map(|id| project.clip(id).ok()).collect();
    if clips.is_empty() {
        return Vec::new();
    }
    let mute = !clips.iter().all(|clip| clip.properties.muted == Some(true));
    clips
        .iter()
        .map(|clip| Operation::SetClipProperties(SetClipProperties { clip_id: clip.id.clone(), properties: ClipProperties { muted: Some(mute), ..ClipProperties::default() } }))
        .collect()
}

/// X: the range of the clip under the playhead (topmost track first).
pub fn clip_range_at(project: &Project, at: Time) -> Option<(Time, Time)> {
    project.tracks.iter().flat_map(|track| track.clips.iter()).find(|clip| clip.start_seconds <= at && at < clip.end()).map(|clip| (clip.start_seconds, clip.end()))
}

/// All edit points (clip starts and ends), sorted, without duplicates.
pub fn edit_points(project: &Project) -> Vec<Time> {
    let mut points: Vec<Time> = project.clips().flat_map(|clip| [clip.start_seconds, clip.end()]).collect();
    points.push(Time::ZERO);
    points.sort();
    points.dedup();
    points
}

pub fn prev_edit(project: &Project, at: Time) -> Time {
    edit_points(project).into_iter().rev().find(|point| *point < at).unwrap_or(Time::ZERO)
}

pub fn next_edit(project: &Project, at: Time) -> Time {
    edit_points(project).into_iter().find(|point| *point > at).unwrap_or(at)
}

/// Clips whose span intersects `[start, end)` on the given tracks.
pub fn clips_in_box(project: &Project, start: Time, end: Time, track_ids: &[String]) -> Vec<String> {
    project
        .tracks
        .iter()
        .filter(|track| track_ids.contains(&track.id))
        .flat_map(|track| track.clips.iter())
        .filter(|clip| clip.intersects(start, end))
        .map(|clip| clip.id.clone())
        .collect()
}

// ---------------------------------------------------------------- media to timeline

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceMode {
    /// E: at the end of the target track.
    Append,
    /// W: at the playhead, pushing later clips right.
    Insert,
    /// D: at the playhead, replacing what is there.
    Overwrite,
    /// Q: at the playhead on a free track of the same kind (a new one if needed).
    Connect,
    /// Drag and drop: at a given time on a given track, failing if occupied.
    At,
}

/// Whether a video file has sound, as recorded by the import probe.
pub fn media_has_audio(media: &MediaAsset) -> bool {
    media.metadata.as_ref().and_then(|metadata| metadata.get("mediaProbe")?.get("audio")?.get("hasAudio")?.as_bool()).unwrap_or(false)
}

/// Pixel size of the media as probed (after rotation).
pub fn media_dimensions(media: &MediaAsset) -> Option<(u32, u32)> {
    let video = media.metadata.as_ref()?.get("mediaProbe")?.get("video")?;
    let width = video.get("displayWidth")?.as_u64()?;
    let height = video.get("displayHeight")?.as_u64()?;
    (width > 0 && height > 0).then_some((width as u32, height as u32))
}

pub fn track_kind_for(media_type: MediaType) -> TrackKind {
    match media_type {
        MediaType::Audio => TrackKind::Audio,
        _ => TrackKind::Video,
    }
}

/// Default timeline length of a media item.
pub fn media_length(media: &MediaAsset) -> Time {
    if media.media_type.is_still() { Time::from_micros(5_000_000) } else { media.duration_seconds }
}

/// The operations that put `media` on the timeline. `target_track` is the track the
/// user targets for this kind (it must exist); `at` is used by Insert/Overwrite/
/// Connect/At. The result may start with an `add_track`.
pub fn place_media(project: &Project, media: &MediaAsset, mode: PlaceMode, target_track: &str, at: Time) -> Result<Vec<Operation>, avs_core::EditError> {
    let kind = track_kind_for(media.media_type);
    let duration = media_length(media);
    let mut operations = Vec::new();
    let mut working = project.clone();
    // A drop onto material that is already there goes to a free track instead (as a
    // connected clip), rather than failing.
    let occupied = |track_id: &str| project.track(track_id).is_ok_and(|track| track.clips.iter().any(|clip| clip.intersects(at, at + duration)));
    let mode = if mode == PlaceMode::At && occupied(target_track) { PlaceMode::Connect } else { mode };
    let (track_id, start, edit_mode) = match mode {
        PlaceMode::Append => (target_track.to_string(), working.track(target_track)?.end(), EditMode::Strict),
        PlaceMode::Insert => (target_track.to_string(), at, EditMode::Insert),
        PlaceMode::Overwrite => (target_track.to_string(), at, EditMode::Overwrite),
        PlaceMode::At => (target_track.to_string(), at, EditMode::Strict),
        PlaceMode::Connect => {
            let end = at + duration;
            let free = working.tracks.iter().find(|track| track.kind == kind && track.id != target_track && !track.is_locked() && !track.clips.iter().any(|clip| clip.intersects(at, end)));
            match free {
                Some(track) => (track.id.clone(), at, EditMode::Strict),
                None => {
                    let prefix = if kind == TrackKind::Audio { "A" } else { "V" };
                    let id = working.fresh_id(prefix, |candidate| working.tracks.iter().any(|track| track.id == candidate));
                    // A connected picture goes above the target (the first track is on top);
                    // connected sound goes below it.
                    let add = Operation::AddTrack(AddTrack {
                        id: Some(id.clone()),
                        kind,
                        name: None,
                        insert_before_track_id: (kind == TrackKind::Video).then(|| target_track.to_string()),
                        insert_after_track_id: (kind != TrackKind::Video).then(|| target_track.to_string()),
                        locked: None,
                        muted: None,
                    });
                    working = avs_core::apply(&working, &add)?.0;
                    operations.push(add);
                    (id, at, EditMode::Strict)
                }
            }
        }
    };
    let placement = Placement {
        media_id: media.id.clone(),
        media_type: media.media_type,
        has_audio: media.media_type == MediaType::Video && media_has_audio(media),
        track_id,
        start_seconds: start,
        duration_seconds: None,
        source_start_seconds: None,
        name: None,
        properties: None,
        linked_audio_track_id: None,
        edit_mode,
    };
    operations.extend(placement::operations(&working, &placement, duration)?);
    Ok(operations)
}

/// The track a media kind goes to when the user has not picked one: the bottom
/// video track (the main picture) or the first audio track.
pub fn default_target(project: &Project, kind: TrackKind) -> Option<String> {
    let mut tracks = project.tracks.iter().filter(|track| track.kind == kind && !track.is_locked());
    let track = if kind == TrackKind::Video { tracks.next_back() } else { tracks.next() };
    track.map(|track| track.id.clone())
}

// ---------------------------------------------------------------- clipboard

/// What ⌘C puts on the clipboard: enough to recreate the clips anywhere.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardClips {
    pub project_id: String,
    pub clips: Vec<Clip>,
}

pub const CLIPBOARD_PREFIX: &str = "agent-video-studio/clips:";

pub fn copy_clips(project: &Project, selection: &[String]) -> Option<String> {
    let ids = project.with_companions(selection);
    let clips: Vec<Clip> = ids.iter().filter_map(|id| project.clip(id).ok().cloned()).collect();
    if clips.is_empty() {
        return None;
    }
    let payload = ClipboardClips { project_id: project.id.clone(), clips };
    serde_json::to_string(&payload).ok().map(|json| format!("{CLIPBOARD_PREFIX}{json}"))
}

pub fn parse_clipboard(text: &str) -> Option<ClipboardClips> {
    serde_json::from_str(text.strip_prefix(CLIPBOARD_PREFIX)?).ok()
}

/// Operations that place copied clips so the earliest starts at `at`, on their own
/// tracks, pushing later material right (as a paste does in Final Cut Pro).
pub fn paste_clips(project: &Project, copied: &ClipboardClips, at: Time) -> Vec<Operation> {
    let Some(earliest) = copied.clips.iter().map(|clip| clip.start_seconds).min() else { return Vec::new() };
    let offset = at - earliest;
    let mut media_clips: Vec<AddClip> = Vec::new();
    let mut captions: Vec<CaptionInput> = Vec::new();
    let mut handled: Vec<String> = Vec::new();
    for clip in &copied.clips {
        if handled.contains(&clip.id) || project.track(&clip.track_id).is_err() {
            continue;
        }
        handled.push(clip.id.clone());
        match clip.kind {
            ClipKind::Caption => captions.push(CaptionInput {
                id: None,
                track_id: Some(clip.track_id.clone()),
                text: clip.caption.as_ref().map(|caption| caption.text.clone()).unwrap_or_default(),
                start_seconds: clip.start_seconds + offset,
                duration_seconds: Some(clip.duration_seconds),
                end_seconds: None,
                speaker: clip.caption.as_ref().and_then(|caption| caption.speaker.clone()),
                language: clip.caption.as_ref().and_then(|caption| caption.language.clone()),
                properties: Some(clip.properties.clone()),
            }),
            ClipKind::Media => {
                let Some(media_id) = clip.media_id.clone() else { continue };
                if project.media(&media_id).is_err() {
                    continue;
                }
                // The audio half of a copied pair rides along as linked audio of the video half.
                let partner = clip.link.as_ref().and_then(|link| {
                    copied.clips.iter().find(|other| other.id != clip.id && other.link.as_ref().is_some_and(|l| l.group_id == link.group_id))
                });
                let linked_audio = match (clip.link.as_ref().map(|link| link.role), partner) {
                    (Some(avs_core::LinkRole::Video), Some(audio)) if project.track(&audio.track_id).is_ok() => {
                        handled.push(audio.id.clone());
                        Some(LinkedAudio { id: None, track_id: audio.track_id.clone(), name: Some(audio.name.clone()), properties: Some(audio.properties.clone()) })
                    }
                    (Some(avs_core::LinkRole::Audio), Some(_)) => {
                        // Handled with its video half.
                        handled.pop();
                        continue;
                    }
                    _ => None,
                };
                media_clips.push(AddClip {
                    id: None,
                    track_id: clip.track_id.clone(),
                    media_id,
                    start_seconds: clip.start_seconds + offset,
                    duration_seconds: Some(clip.duration_seconds),
                    source_start_seconds: clip.source_start_seconds,
                    name: Some(clip.name.clone()),
                    properties: Some(clip.properties.clone()),
                    linked_audio,
                });
            }
        }
    }
    let mut operations = Vec::new();
    if !media_clips.is_empty() {
        operations.push(Operation::AddClips(AddClips { clips: media_clips, edit_mode: EditMode::Insert }));
    }
    if !captions.is_empty() {
        operations.push(Operation::AddCaptions(AddCaptions { track_id: None, captions }));
    }
    operations
}

/// The end of the copied material relative to its start (for ⌘D: paste right after).
pub fn clipboard_end(copied: &ClipboardClips) -> Option<Time> {
    copied.clips.iter().map(Clip::end).max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use avs_core::secs;

    /// Two video clips with linked audio, back to back: clip-1 [0,4), clip-3 [4,8).
    fn project() -> Project {
        let mut project = Project::empty("p", "Test");
        let register = |id: &str| {
            Operation::RegisterMedia(RegisterMedia {
                id: Some(id.into()),
                name: format!("{id}.mp4"),
                media_type: MediaType::Video,
                duration_seconds: secs(20.0),
                source: None,
                thumbnail: None,
                tags: None,
                transcript: None,
                metadata: Some(serde_json::from_value(serde_json::json!({"mediaProbe": {"audio": {"hasAudio": true}, "video": {"displayWidth": 1280, "displayHeight": 720}}})).unwrap()),
            })
        };
        project = avs_core::apply(&project, &register("m1")).unwrap().0;
        project = avs_core::apply(&project, &register("m2")).unwrap().0;
        for (media, start, track) in [("m1", 0.0, "V1"), ("m2", 4.0, "V1")] {
            let operation = Operation::AddClips(AddClips {
                clips: vec![AddClip {
                    id: None,
                    track_id: track.into(),
                    media_id: media.into(),
                    start_seconds: secs(start),
                    duration_seconds: Some(secs(4.0)),
                    source_start_seconds: Some(secs(2.0)),
                    name: None,
                    properties: None,
                    linked_audio: Some(LinkedAudio { id: None, track_id: "A1".into(), name: None, properties: None }),
                }],
                edit_mode: EditMode::Strict,
            });
            project = avs_core::apply(&project, &operation).unwrap().0;
        }
        project
    }

    fn input<'a>(project: &'a Project, kind: &'a DragKind, delta: f64) -> DragInput<'a> {
        DragInput { project, kind, raw_delta: secs(delta), hover_track: None, fps: 30.0, snap: true, snap_threshold: secs(0.2), playhead: secs(10.0), range: None }
    }

    #[test]
    fn select_tool_body_drag_moves_the_linked_pair() {
        let project = project();
        let kind = drag_kind(&project, Tool::Select, false, "clip-3", Zone::Body, &[]).unwrap();
        let DragKind::Move { clip_ids, .. } = &kind else { panic!("{kind:?}") };
        assert_eq!(clip_ids.len(), 2);
        let (operations, _) = drag_operation(&input(&project, &kind, 3.0)).unwrap();
        let (moved, _) = avs_core::apply(&project, &operations[0]).unwrap();
        assert_eq!(moved.clip("clip-3").unwrap().start_seconds, secs(7.0));
        assert_eq!(moved.clip("clip-4").unwrap().start_seconds, secs(7.0), "the audio half follows");
    }

    /// clip-1 and clip-3 each play 2..6s of a 20s file. Dragging further than the
    /// media allows stops at the limit and still gives a valid project.
    #[test]
    fn trim_roll_and_slip_stop_where_the_media_ends() {
        let project = project();
        let apply_all = |operations: &[Operation]| operations.iter().fold(project.clone(), |project, operation| avs_core::apply(&project, operation).expect("the held drag is a valid edit").0);

        // Roll the cut at 4s left by 3s: clip-3 has only 2s of media before its start.
        let roll = drag_kind(&project, Tool::Trim, false, "clip-1", Zone::End, &[]).unwrap();
        assert!(matches!(roll, DragKind::Roll { .. }), "{roll:?}");
        let mut far_left = input(&project, &roll, -3.0);
        far_left.snap = false;
        let (operations, _) = drag_operation(&far_left).unwrap();
        let rolled = apply_all(&operations);
        assert_eq!(rolled.clip("clip-3").unwrap().start_seconds, secs(2.0));
        assert_eq!(rolled.clip("clip-3").unwrap().source_start(), secs(0.0));

        // Pull clip-3's start 5s earlier with the select tool: it stops 2s earlier.
        let trim = DragKind::Trim { clip_id: "clip-3".into(), edge: Edge::Start, ripple: false };
        let mut far = input(&project, &trim, -5.0);
        far.snap = false;
        // clip-1 is in the way of a plain trim, so check the bound itself.
        assert_eq!(edge_bounds(&project, project.clip("clip-3").unwrap(), Edge::Start, 30.0).0, secs(2.0));
        let DragKind::Trim { .. } = &trim else { unreachable!() };
        let (operations, _) = drag_operation(&far).unwrap();
        assert!(matches!(&operations[0], Operation::TrimClip(trim) if trim.start_seconds == Some(secs(2.0))));

        // Stretch clip-3's end 30s later: the file ends 14s after the clip's end.
        let end = DragKind::Trim { clip_id: "clip-3".into(), edge: Edge::End, ripple: false };
        let mut far = input(&project, &end, 30.0);
        far.snap = false;
        let (operations, _) = drag_operation(&far).unwrap();
        let stretched = apply_all(&operations);
        assert_eq!(stretched.clip("clip-3").unwrap().end(), secs(22.0));

        // Slip clip-3 far in both directions: its 4s window stays inside 0..20s.
        let slip = DragKind::Slip { clip_id: "clip-3".into() };
        let (operations, _) = drag_operation(&input(&project, &slip, 30.0)).unwrap();
        assert_eq!(apply_all(&operations).clip("clip-3").unwrap().source_start(), secs(0.0));
        let (operations, _) = drag_operation(&input(&project, &slip, -30.0)).unwrap();
        assert_eq!(apply_all(&operations).clip("clip-3").unwrap().source_start(), secs(16.0));
    }

    #[test]
    fn move_snaps_to_the_playhead() {
        let project = project();
        let kind = drag_kind(&project, Tool::Select, false, "clip-3", Zone::Body, &[]).unwrap();
        // Proposed end 4 + 4 + 1.9 = 9.9; the playhead at 10 is within 0.2s.
        let (operations, snapped) = drag_operation(&input(&project, &kind, 1.9)).unwrap();
        assert_eq!(snapped, Some(secs(10.0)));
        let (moved, _) = avs_core::apply(&project, &operations[0]).unwrap();
        assert_eq!(moved.clip("clip-3").unwrap().end(), secs(10.0));
    }

    fn starts(project: &Project, track: &str) -> Vec<(String, f64)> {
        project.track(track).unwrap().clips.iter().map(|clip| (clip.id.clone(), clip.start_seconds.secs())).collect()
    }

    #[test]
    fn moving_onto_packed_clips_reorders() {
        let project = project();
        let kind = drag_kind(&project, Tool::Select, false, "clip-3", Zone::Body, &[]).unwrap();
        // clip-3 [4,8) dragged left by 3s lands on clip-1 [0,4): strict move is refused.
        let (strict, _) = drag_operation(&input(&project, &kind, -3.0)).unwrap();
        assert_eq!(avs_core::apply(&project, &strict[0]).unwrap_err().code, "CLIP_OVERLAP");
        let DragKind::Move { clip_ids, anchor } = &kind else { unreachable!() };
        let (operations, at) = reorder_operations(&project, clip_ids, anchor, secs(1.0)).unwrap();
        assert_eq!(at, Time::ZERO);
        let mut current = project.clone();
        for operation in &operations {
            current = avs_core::apply(&current, operation).unwrap().0;
        }
        assert_eq!(starts(&current, "V1"), vec![("clip-3".into(), 0.0), ("clip-1".into(), 4.0)]);
        assert_eq!(starts(&current, "A1"), vec![("clip-4".into(), 0.0), ("clip-2".into(), 4.0)]);
        assert_eq!(current.duration(), secs(8.0), "no hole is left");
        // Back to the right: drop past the middle of clip-1, after it.
        let (operations, at) = reorder_operations(&current, &["clip-3".into(), "clip-4".into()], "clip-3", secs(6.5)).unwrap();
        assert_eq!(at, secs(8.0));
        for operation in &operations {
            current = avs_core::apply(&current, operation).unwrap().0;
        }
        assert_eq!(starts(&current, "V1"), vec![("clip-1".into(), 0.0), ("clip-3".into(), 4.0)]);
    }

    #[test]
    fn several_clips_move_to_another_track_together() {
        let mut project = project();
        let add = Operation::AddTrack(AddTrack { id: Some("V2".into()), kind: TrackKind::Video, name: None, insert_before_track_id: None, insert_after_track_id: Some("V1".into()), locked: None, muted: None });
        project = avs_core::apply(&project, &add).unwrap().0;
        let selection: Vec<String> = ["clip-1", "clip-2", "clip-3", "clip-4"].map(String::from).to_vec();
        let kind = drag_kind(&project, Tool::Select, false, "clip-1", Zone::Body, &selection).unwrap();
        let mut drag = input(&project, &kind, 1.0);
        drag.hover_track = Some("V2");
        drag.snap = false;
        let (operations, _) = drag_operation(&drag).unwrap();
        assert_eq!(operations.len(), 2, "one move per picture clip; sound follows");
        let mut current = project.clone();
        for operation in &operations {
            current = avs_core::apply(&current, operation).unwrap().0;
        }
        assert_eq!(starts(&current, "V2"), vec![("clip-1".into(), 1.0), ("clip-3".into(), 5.0)]);
        assert_eq!(starts(&current, "A1"), vec![("clip-2".into(), 1.0), ("clip-4".into(), 5.0)]);
        assert!(current.track("V1").unwrap().clips.is_empty());
        // Past the last video track there is nowhere to go.
        assert!(track_moves(&project, &selection, "clip-1", "A1", Time::ZERO).is_none());
    }

    #[test]
    fn trim_tool_on_a_shared_edge_rolls() {
        let project = project();
        let kind = drag_kind(&project, Tool::Trim, false, "clip-1", Zone::End, &[]).unwrap();
        assert_eq!(kind, DragKind::Roll { left: "clip-1".into(), right: "clip-3".into() });
        let (operations, _) = drag_operation(&input(&project, &kind, -1.0)).unwrap();
        let (rolled, _) = avs_core::apply(&project, &operations[0]).unwrap();
        assert_eq!(rolled.clip("clip-1").unwrap().end(), secs(3.0));
        assert_eq!(rolled.clip("clip-3").unwrap().start_seconds, secs(3.0));
    }

    #[test]
    fn option_edge_drag_ripples_and_plain_edge_drag_trims() {
        let project = project();
        let ripple = drag_kind(&project, Tool::Select, true, "clip-1", Zone::End, &[]).unwrap();
        let (operations, _) = drag_operation(&input(&project, &ripple, -1.0)).unwrap();
        let (rippled, _) = avs_core::apply(&project, &operations[0]).unwrap();
        assert_eq!(rippled.clip("clip-3").unwrap().start_seconds, secs(3.0), "later clip follows");
        let trim = drag_kind(&project, Tool::Select, false, "clip-1", Zone::End, &[]).unwrap();
        let (operations, _) = drag_operation(&input(&project, &trim, -1.0)).unwrap();
        let (trimmed, _) = avs_core::apply(&project, &operations[0]).unwrap();
        assert_eq!(trimmed.clip("clip-3").unwrap().start_seconds, secs(4.0), "a plain trim leaves a gap");
    }

    #[test]
    fn slip_moves_source_and_keeps_the_span() {
        let project = project();
        let kind = drag_kind(&project, Tool::Trim, false, "clip-1", Zone::Body, &[]).unwrap();
        let (operations, _) = drag_operation(&input(&project, &kind, 1.0)).unwrap();
        let (slipped, _) = avs_core::apply(&project, &operations[0]).unwrap();
        let clip = slipped.clip("clip-1").unwrap();
        assert_eq!((clip.start_seconds, clip.source_start()), (Time::ZERO, secs(1.0)));
    }

    #[test]
    fn blade_with_selection_outside_the_playhead_cuts_all_tracks() {
        let project = project();
        let operation = blade(&project, &["clip-1".into()], secs(6.0)).unwrap();
        let (cut, _) = avs_core::apply(&project, &operation).unwrap();
        assert_eq!(cut.clips().count(), 6);
    }

    #[test]
    fn ripple_delete_of_both_halves_runs_once_per_pair() {
        let project = project();
        let operations = delete(&project, &["clip-1".into(), "clip-2".into(), "clip-3".into()], None, true);
        assert_eq!(operations.len(), 2);
        let mut current = project.clone();
        for operation in &operations {
            current = avs_core::apply(&current, operation).unwrap().0;
        }
        assert_eq!(current.clips().count(), 0);
    }

    #[test]
    fn range_delete_extracts() {
        let project = project();
        let operations = delete(&project, &[], Some((secs(1.0), secs(2.0))), true);
        let (extracted, _) = avs_core::apply(&project, &operations[0]).unwrap();
        assert_eq!(extracted.duration(), secs(7.0));
    }

    #[test]
    fn connect_adds_a_track_when_none_is_free() {
        let project = project();
        let media = project.media("m2").unwrap().clone();
        let operations = place_media(&project, &media, PlaceMode::Connect, "V1", secs(1.0)).unwrap();
        assert!(matches!(operations[0], Operation::AddTrack(_)));
        let mut current = project.clone();
        for operation in &operations {
            current = avs_core::apply(&current, operation).unwrap().0;
        }
        assert_eq!(current.tracks[0].clips.len(), 1, "the new track is above V1");
    }

    #[test]
    fn a_drop_on_an_occupied_spot_connects() {
        let project = project();
        let media = project.media("m1").unwrap().clone();
        let operations = place_media(&project, &media, PlaceMode::At, "V1", secs(2.0)).unwrap();
        let mut current = project.clone();
        for operation in &operations {
            current = avs_core::apply(&current, operation).unwrap().0;
        }
        assert_eq!(current.tracks.iter().filter(|track| track.kind == TrackKind::Video).count(), 2);
    }

    #[test]
    fn copy_and_paste_keeps_links() {
        let project = project();
        let text = copy_clips(&project, &["clip-1".into()]).unwrap();
        let copied = parse_clipboard(&text).unwrap();
        assert_eq!(copied.clips.len(), 2);
        let operations = paste_clips(&project, &copied, secs(8.0));
        let (pasted, _) = avs_core::apply(&project, &operations[0]).unwrap();
        assert_eq!(pasted.clips().count(), 6);
        let new = pasted.clips().find(|clip| clip.start_seconds == secs(8.0) && clip.track_id == "V1").unwrap();
        assert!(pasted.companion_id(&new.id).is_some());
    }

    #[test]
    fn mute_toggle_covers_the_pair() {
        let project = project();
        let operations = toggle_muted(&project, &["clip-1".into()]);
        assert_eq!(operations.len(), 2);
    }

    #[test]
    fn edit_point_navigation() {
        let project = project();
        assert_eq!(next_edit(&project, secs(1.0)), secs(4.0));
        assert_eq!(prev_edit(&project, secs(4.0)), Time::ZERO);
        assert_eq!(next_edit(&project, secs(8.0)), secs(8.0));
    }
}
