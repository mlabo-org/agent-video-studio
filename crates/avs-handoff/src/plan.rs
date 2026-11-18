//! Turning a checked manifest into the list of `avs_core::Operation`s that makes the
//! import. Nothing here writes: the list is applied by `Session::apply_many` in one go.
//!
//! Order of the list: project settings and `metadata.codexVideo`, tracks, media,
//! clips (with linked audio), captions.

use crate::error::{HandoffError, Result};
use crate::manifest::{Asset, CaptionSpec, ClipProps, ClipSpec, Manifest, Obj};
use crate::{CaptionEntry, ClipEntry, ImportOptions, ImportWarning, TrackEntry, TrackStatus};
use avs_core::ops::{AddCaptions, AddClip, AddClips, AddTrack, CaptionInput, LinkedAudio, SetProjectSettings};
use avs_core::{CaptionStyle, ClipProperties, MediaType, Metadata, Operation, Position, Project, Resolution, Time, TrackKind};
use avs_session::PreparedImport;
use serde_json::{Value, json};

/// A track the manifest needs: declared in `tracks[]`, or implied by a clip or caption.
#[derive(Debug, Clone)]
pub struct TrackNeed {
    pub id: String,
    pub kind: TrackKind,
    pub name: Option<String>,
    pub inferred: bool,
}

pub struct Plan {
    pub operations: Vec<Operation>,
    pub tracks: Vec<TrackEntry>,
    pub clips: Vec<ClipEntry>,
    pub captions: Vec<CaptionEntry>,
    pub warnings: Vec<ImportWarning>,
    pub created_track_ids: Vec<String>,
    /// Whether the project's title, frame rate or size changed.
    pub settings_changed: bool,
}

fn kind_name(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "video",
        TrackKind::Audio => "audio",
        TrackKind::Caption => "caption",
    }
}

// CodexVideo keeps a single character on cv-character and gives extra characters sharing the screen a track per
// slot (cv-character-center, cv-character-right); all of them sit above cv-visual.
fn is_character_track(id: &str) -> bool {
    let id = id.trim().to_lowercase();
    id == "cv-character" || id.starts_with("cv-character-")
}

fn is_visual_track(id: &str) -> bool {
    id.trim().eq_ignore_ascii_case("cv-visual")
}

fn time(seconds: f64, field: &str) -> Result<Time> {
    Time::from_secs(seconds).ok_or_else(|| HandoffError::invalid("CODEXVIDEO_HANDOFF_INVALID_TIMING", format!("{field} is out of range.")).with("value", seconds))
}

/// The tracks the manifest needs, character tracks first (so they can be placed above `cv-visual`).
pub fn derive_tracks(manifest: &Manifest) -> Result<(Vec<TrackNeed>, Vec<ImportWarning>)> {
    let mut needs: Vec<TrackNeed> =
        manifest.tracks.iter().map(|track| TrackNeed { id: track.id.clone(), kind: track.kind, name: track.name.clone(), inferred: false }).collect();
    let mut warnings = Vec::new();
    let mut ensure = |id: &str, kind: TrackKind| -> Result<()> {
        match needs.iter().find(|need| need.id == id) {
            Some(existing) if existing.kind != kind => Err(HandoffError::invalid(
                "CODEXVIDEO_HANDOFF_UNKNOWN_TRACK",
                format!("Track \"{id}\" is declared as {}, but usage requires {}.", kind_name(existing.kind), kind_name(kind)),
            )
            .with("trackId", id)
            .with("declaredKind", kind_name(existing.kind))
            .with("requiredKind", kind_name(kind))),
            Some(_) => Ok(()),
            None => {
                needs.push(TrackNeed { id: id.to_string(), kind, name: Some(id.to_string()), inferred: true });
                warnings.push(
                    ImportWarning::new("CODEXVIDEO_HANDOFF_TRACK_INFERRED", format!("Track \"{id}\" is not declared in tracks[]; it was created as a {} track.", kind_name(kind)))
                        .with("trackId", id)
                        .with("kind", kind_name(kind)),
                );
                Ok(())
            }
        }
    };
    for clip in &manifest.clips {
        let asset = manifest.asset(&clip.asset_id).ok_or_else(|| HandoffError::invalid("CODEXVIDEO_HANDOFF_UNKNOWN_MEDIA", "Clip references an unknown asset.").with("clipId", &clip.id))?;
        ensure(&clip.track_id, if asset.media_type == MediaType::Audio { TrackKind::Audio } else { TrackKind::Video })?;
    }
    for caption in &manifest.captions {
        if let Some(track_id) = &caption.track_id {
            ensure(track_id, TrackKind::Caption)?;
        }
    }
    needs.sort_by_key(|need| !is_character_track(&need.id));
    Ok((needs, warnings))
}

/// Everything about the manifest that can already be judged against the project:
/// a track that exists with the wrong kind, and clip or caption ids already in use.
pub fn check_against_project(needs: &[TrackNeed], manifest: &Manifest, project: &Project) -> Result<()> {
    for need in needs {
        if let Some(track) = project.tracks.iter().find(|track| track.id == need.id)
            && track.kind != need.kind
        {
            return Err(HandoffError::invalid(
                "CODEXVIDEO_HANDOFF_UNKNOWN_TRACK",
                format!("Track \"{}\" exists as {}, but CodexVideo manifest requires {}.", need.id, kind_name(track.kind), kind_name(need.kind)),
            )
            .with("trackId", &need.id)
            .with("existingKind", kind_name(track.kind))
            .with("requiredKind", kind_name(need.kind)));
        }
    }
    let ids = manifest.clips.iter().map(|clip| &clip.id).chain(manifest.captions.iter().map(|caption| &caption.id));
    let taken: Vec<&String> = ids.filter(|id| project.clips().any(|clip| &clip.id == *id)).collect();
    if !taken.is_empty() {
        return Err(HandoffError::conflict(
            "CODEXVIDEO_HANDOFF_CLIP_ID_CONFLICT",
            format!("{} clip or caption id(s) from the manifest already exist in the project; this manifest may have been imported before.", taken.len()),
        )
        .with("clipIds", taken));
    }
    Ok(())
}

// ---------------------------------------------------------------- metadata

fn obj_field<'a>(obj: Option<&'a Obj>, key: &str) -> Option<&'a Obj> {
    obj?.get(key)?.as_object()
}

fn text_field(obj: &Obj, key: &str) -> Option<String> {
    obj.get(key)?.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn finite_field(obj: &Obj, key: &str) -> Option<f64> {
    obj.get(key)?.as_f64().filter(|n| n.is_finite())
}

/// The producer fields every imported object carries in `metadata.codexVideo`.
fn provenance(into: &mut Obj) {
    into.insert("schema".into(), json!(crate::manifest::SCHEMA));
    into.insert("producer".into(), json!(crate::manifest::PRODUCER));
}

fn job_fields(manifest: &Manifest, into: &mut Obj) {
    for key in ["jobId", "profile"] {
        if let Some(value) = manifest.job_string(key) {
            into.insert(key.into(), json!(value));
        }
    }
}

/// `metadata` for an imported media asset.
pub fn asset_metadata(manifest: &Manifest, asset: &Asset) -> Metadata {
    let mut metadata = asset.metadata.clone().unwrap_or_default();
    let mut codex = obj_field(asset.metadata.as_ref(), "codexVideo").cloned().unwrap_or_default();
    provenance(&mut codex);
    codex.insert("manifestAssetId".into(), json!(asset.id));
    if let Some(role) = &asset.role {
        codex.insert("role".into(), json!(role));
    }
    if let Some(hash) = &asset.hash {
        codex.insert("hash".into(), json!(hash));
    }
    if let Some(duration) = asset.duration_sec {
        codex.insert("durationSec".into(), json!(duration));
    }
    if let Some(width) = asset.width {
        codex.insert("width".into(), json!(width));
    }
    if let Some(height) = asset.height {
        codex.insert("height".into(), json!(height));
    }
    job_fields(manifest, &mut codex);
    if let Some(created) = &manifest.created_at {
        codex.insert("createdAt".into(), json!(created));
    }
    metadata.insert("codexVideo".into(), Value::Object(codex));
    metadata
}

fn clip_metadata(manifest: &Manifest, clip: &ClipSpec, asset: &Asset) -> Obj {
    let mut metadata = clip.props.metadata.clone().unwrap_or_default();
    let mut codex = obj_field(Some(&metadata), "codexVideo").cloned().unwrap_or_default();
    let role = clip
        .role
        .clone()
        .or_else(|| text_field(&metadata, "role"))
        .or_else(|| text_field(&codex, "role"))
        .or_else(|| asset.role.clone());
    provenance(&mut codex);
    codex.insert("manifestClipId".into(), json!(clip.id));
    codex.insert("manifestAssetId".into(), json!(clip.asset_id));
    if let Some(role) = role {
        codex.insert("role".into(), json!(role));
    }
    job_fields(manifest, &mut codex);
    metadata.insert("codexVideo".into(), Value::Object(codex));
    metadata
}

fn caption_properties(manifest: &Manifest, caption: &CaptionSpec) -> ClipProperties {
    let mut metadata = caption.metadata.clone().unwrap_or_default();
    let mut codex = obj_field(Some(&metadata), "codexVideo").cloned().unwrap_or_default();
    provenance(&mut codex);
    codex.insert("manifestCaptionId".into(), json!(caption.id));
    if let Some(style) = &caption.style {
        codex.insert("style".into(), style.clone());
    }
    job_fields(manifest, &mut codex);
    metadata.insert("codexVideo".into(), Value::Object(codex));
    ClipProperties { caption_style: caption_style(caption.style.as_ref()), metadata: Some(metadata), ..ClipProperties::default() }
}

/// Only an object style yields a renderable style; a string such as `"telop"` stays in metadata.
fn caption_style(style: Option<&Value>) -> Option<CaptionStyle> {
    let style = style?.as_object()?;
    let placement = style.get("placement").and_then(Value::as_str).filter(|p| matches!(*p, "lower-third" | "center" | "title" | "subtitle")).map(str::to_string);
    let color = style.get("color").and_then(Value::as_str).map(str::to_string);
    let background = style.get("background").and_then(Value::as_str).map(str::to_string);
    let font_size = style.get("fontSize").and_then(Value::as_f64).filter(|n| n.is_finite() && *n > 0.0);
    let built = CaptionStyle { placement, color, background, font_size };
    (built != CaptionStyle::default()).then_some(built)
}

// ---------------------------------------------------------------- project metadata

fn interval_seconds(interval: &Obj) -> Option<(f64, f64)> {
    let first = |keys: &[&str]| keys.iter().find_map(|key| finite_field(interval, key));
    let start = first(&["startSeconds", "startSec", "start"])?;
    let end = first(&["endSeconds", "endSec", "end"]).or_else(|| first(&["durationSeconds", "durationSec", "duration"]).filter(|d| *d > 0.0).map(|d| start + d))?;
    (end > start).then_some((start, end))
}

/// Keeps each interval as written and adds `startSeconds` / `endSeconds`; drops (and reports) the unreadable ones.
fn normalize_intervals(value: &Value, warnings: &mut Vec<ImportWarning>) -> Option<Value> {
    let items = value.as_array()?;
    let mut kept = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match item.as_object().and_then(|interval| interval_seconds(interval).map(|bounds| (interval, bounds))) {
            Some((interval, (start, end))) => {
                let mut normalized = interval.clone();
                normalized.insert("startSeconds".into(), json!(start));
                normalized.insert("endSeconds".into(), json!(end));
                kept.push(Value::Object(normalized));
            }
            None => warnings.push(
                ImportWarning::new("CODEXVIDEO_HANDOFF_MUTED_INTERVAL_DROPPED", format!("audio.mutedIntervals[{index}] has no readable start and end and was not imported."))
                    .with("index", index)
                    .with("interval", item),
            ),
        }
    }
    Some(Value::Array(kept))
}

fn project_codex_metadata(project: &Project, manifest: &Manifest, job_root: Option<&std::path::Path>, warnings: &mut Vec<ImportWarning>) -> Obj {
    let mut codex = obj_field(project.metadata.as_ref(), "codexVideo").cloned().unwrap_or_default();
    if let (Some(previous), Some(next)) = (codex.get("jobId").and_then(Value::as_str), manifest.job_string("jobId"))
        && previous != next
    {
        warnings.push(
            ImportWarning::new("CODEXVIDEO_HANDOFF_PROJECT_JOB_REPLACED", "The project's CodexVideo job record now describes this manifest's job; the previous job id was replaced.")
                .with("previousJobId", previous)
                .with("jobId", next),
        );
    }
    provenance(&mut codex);
    codex.insert("manifestPath".into(), json!(manifest.path.display().to_string()));
    job_fields(manifest, &mut codex);
    if let Some(root) = job_root {
        codex.insert("jobRoot".into(), json!(root.display().to_string()));
    }
    if let Some(created) = &manifest.created_at {
        codex.insert("createdAt".into(), json!(created));
    }
    if let Some(manifest_audio) = &manifest.audio {
        let mut audio = obj_field(Some(&codex), "audio").cloned().unwrap_or_default();
        for (key, value) in manifest_audio {
            audio.insert(key.clone(), value.clone());
        }
        if let Some(intervals) = manifest_audio.get("mutedIntervals").and_then(|value| normalize_intervals(value, warnings)) {
            audio.insert("mutedIntervals".into(), intervals);
        }
        if let Some(flag) = audio.get("sourceAudioPreserved").and_then(Value::as_bool) {
            codex.insert("sourceAudioPreserved".into(), json!(flag));
        }
        if let Some(intervals) = audio.get("mutedIntervals").filter(|value| value.is_array()) {
            codex.insert("mutedIntervals".into(), intervals.clone());
        }
        codex.insert("audio".into(), Value::Object(audio));
    }
    codex
}

// ---------------------------------------------------------------- character overlay

fn is_character_clip(asset: &Asset, metadata: &Obj) -> bool {
    let clip_codex = obj_field(Some(metadata), "codexVideo");
    let asset_codex = obj_field(asset.metadata.as_ref(), "codexVideo");
    let text = |obj: Option<&Obj>, key: &str| obj.and_then(|o| text_field(o, key));
    text(clip_codex, "clipRole").as_deref() == Some("character")
        || text(clip_codex, "assetSource").as_deref() == Some("character")
        || obj_field(clip_codex, "character").is_some()
        || text(asset_codex, "sourceField").as_deref() == Some("character")
        || obj_field(asset_codex, "character").is_some()
}

fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

/// Default size and corner for a character clip with no explicit transform: about a sixth of
/// the frame wide, standing near the bottom in the `left`, `center` or `right` slot.
fn default_character_overlay(asset: &Asset, metadata: &Obj, frame: Resolution) -> (Position, f64, f64) {
    let (width, height) = (f64::from(frame.width), f64::from(frame.height));
    let source_width = f64::from(asset.width.unwrap_or(941));
    let source_height = f64::from(asset.height.unwrap_or(1672));
    let landscape = width >= height;
    let target_width = (width * if landscape { 0.17 } else { 0.26 }).clamp(if landscape { 240.0 } else { 180.0 }, if landscape { 360.0 } else { 300.0 });
    let target_height = height * if landscape { 0.55 } else { 0.42 };
    let scale = (target_width / source_width).min(target_height / source_height);
    let (display_width, display_height) = (source_width * scale, source_height * scale);
    let character = obj_field(obj_field(Some(metadata), "codexVideo"), "character");
    let slot = match character.and_then(|c| text_field(c, "slot")).map(|s| s.to_lowercase()).as_deref() {
        Some(slot @ ("left" | "center" | "right")) => slot.to_string(),
        _ => match character.and_then(|c| text_field(c, "placement")).map(|s| s.to_lowercase()) {
            Some(placement) if placement == "center" => "center".to_string(),
            Some(placement) if placement.contains("right") => "right".to_string(),
            _ => "left".to_string(),
        },
    };
    let side_margin = width * if landscape { 0.035 } else { 0.045 };
    let bottom_margin = height * if landscape { 0.105 } else { 0.09 };
    let x = match slot.as_str() {
        "center" => width / 2.0,
        "right" => width - side_margin - display_width / 2.0,
        _ => side_margin + display_width / 2.0,
    };
    let y = height - bottom_margin - display_height / 2.0;
    let opacity = match character.and_then(|c| finite_field(c, "opacity")) {
        Some(explicit) => explicit.clamp(0.0, 1.0),
        None => match character.and_then(|c| text_field(c, "visibility")).map(|s| s.to_lowercase()).as_deref() {
            Some("hidden" | "none") => 0.0,
            _ => 1.0,
        },
    };
    (Position { x: round6(x), y: round6(y) }, round6(scale), opacity)
}

fn visual_properties(asset: &Asset, metadata: &Obj, props: &ClipProps, frame: Resolution) -> ClipProperties {
    if props.has_visual() {
        return ClipProperties { position: props.position, scale: props.scale, rotation_degrees: props.rotation_degrees, opacity: props.opacity, crop: props.crop, ..ClipProperties::default() };
    }
    if is_character_clip(asset, metadata) {
        let (position, scale, opacity) = default_character_overlay(asset, metadata, frame);
        return ClipProperties { position: Some(position), scale: Some(scale), opacity: Some(opacity), ..ClipProperties::default() };
    }
    ClipProperties::default()
}

fn audio_properties(props: &ClipProps) -> Result<ClipProperties> {
    Ok(ClipProperties {
        volume_db: props.volume_db,
        muted: props.muted,
        fade_in_seconds: props.fade_in_seconds.map(|s| time(s, "fadeInSeconds")).transpose()?,
        fade_out_seconds: props.fade_out_seconds.map(|s| time(s, "fadeOutSeconds")).transpose()?,
        ..ClipProperties::default()
    })
}

fn has_embedded_audio(prepared: &PreparedImport) -> bool {
    prepared
        .register_media
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("mediaProbe")?.get("audio")?.get("hasAudio")?.as_bool())
        .unwrap_or(false)
}

fn manifest_hash_hex(hash: &str) -> Option<String> {
    let hex = hash.strip_prefix("sha256:").unwrap_or(hash).to_lowercase();
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}

// ---------------------------------------------------------------- the plan

/// How far a clip may run past the end of its media before the importer refuses it.
/// Producers measure audio length differently from ffprobe (an MP3's start offset and
/// encoder delay alone are about 25 ms), so a clip that overshoots by a few frames is
/// cut to the file instead of failing the whole import.
const MAX_OVERSHOOT_TRIMMED: Time = Time::from_micros(100_000);

pub struct Inputs<'a> {
    pub manifest: &'a Manifest,
    pub project: &'a Project,
    /// One per `manifest.assets`, same order.
    pub prepared: &'a [PreparedImport],
    pub options: &'a ImportOptions,
    pub job_root: Option<&'a std::path::Path>,
}

fn settings_json(title: &str, fps: f64, resolution: Resolution) -> Value {
    json!({ "title": title, "fps": fps, "resolution": { "width": resolution.width, "height": resolution.height } })
}

pub fn build(inputs: &Inputs) -> Result<Plan> {
    let Inputs { manifest, project, prepared, options, job_root } = *inputs;
    let mut warnings = manifest.compat_warnings.clone();
    let (needs, track_warnings) = derive_tracks(manifest)?;
    warnings.extend(track_warnings);
    check_against_project(&needs, manifest, project)?;

    // Hashes the producer recorded against what was actually copied.
    for (asset, copy) in manifest.assets.iter().zip(prepared) {
        if let Some(expected) = asset.hash.as_deref().and_then(manifest_hash_hex)
            && expected != copy.library.sha256.to_lowercase()
        {
            warnings.push(
                ImportWarning::new("CODEXVIDEO_HANDOFF_ASSET_HASH_MISMATCH", format!("Asset \"{}\" no longer matches the hash in the manifest; the file changed after the handoff was written.", asset.id))
                    .with("assetId", &asset.id)
                    .with("manifestSha256", expected)
                    .with("importedSha256", &copy.library.sha256),
            );
        }
    }

    // ---- project settings
    let mut operations = Vec::new();
    let manifest_project = manifest.project.as_ref();
    let explicit_title = options.project_title.as_deref().map(str::trim).filter(|title| !title.is_empty()).map(str::to_string);
    let manifest_resolution = manifest_project.and_then(|p| Some(Resolution { width: p.width?, height: p.height? }));
    let had_clips = project.clips().next().is_some();
    let mut title = explicit_title.clone();
    let (mut fps, mut resolution) = (None, None);
    // The manifest's title names the project only while the project is still empty; a
    // project with clips already has a name someone chose. Frame rate and size follow the manifest.
    if !options.keep_project_settings {
        if !had_clips {
            title = title.or_else(|| manifest_project.and_then(|p| p.title.clone()));
        }
        fps = manifest_project.and_then(|p| p.fps);
        resolution = manifest_resolution;
    }
    let title = title.filter(|title| *title != project.title);
    let fps = fps.filter(|fps| *fps != project.fps);
    let resolution = resolution.filter(|resolution| *resolution != project.resolution);
    let settings_changed = title.is_some() || fps.is_some() || resolution.is_some();
    let before = settings_json(&project.title, project.fps, project.resolution);
    if had_clips && (fps.is_some() || resolution.is_some()) {
        let after = settings_json(title.as_deref().unwrap_or(&project.title), fps.unwrap_or(project.fps), resolution.unwrap_or(project.resolution));
        warnings.push(
            ImportWarning::new("CODEXVIDEO_HANDOFF_PROJECT_SETTINGS_OVERWRITTEN", "The project already had clips; its frame rate or size was replaced by the manifest's. Pass keepProjectSettings to leave them.")
                .with("before", before.clone())
                .with("after", after),
        );
    }
    // Manifest values that differ from the project and were not applied.
    let mut kept = Vec::new();
    if let Some(manifest_title) = manifest_project.and_then(|p| p.title.as_ref())
        && explicit_title.is_none()
        && *manifest_title != project.title
        && (had_clips || options.keep_project_settings)
    {
        kept.push("title");
    }
    if options.keep_project_settings {
        if manifest_project.and_then(|p| p.fps).is_some_and(|f| f != project.fps) {
            kept.push("fps");
        }
        if manifest_resolution.is_some_and(|r| r != project.resolution) {
            kept.push("resolution");
        }
    }
    if !kept.is_empty() {
        warnings.push(
            ImportWarning::new("CODEXVIDEO_HANDOFF_PROJECT_SETTINGS_KEPT", "The project's own title, frame rate or size was kept instead of the manifest's.")
                .with("kept", kept)
                .with("project", before)
                .with("manifest", manifest_project),
        );
    }
    let frame = resolution.unwrap_or(project.resolution);
    let mut patch = Metadata::new();
    patch.insert("codexVideo".into(), Value::Object(project_codex_metadata(project, manifest, job_root, &mut warnings)));
    operations.push(Operation::SetProjectSettings(SetProjectSettings { title, fps, resolution, sample_rate: None, metadata: Some(patch) }));

    // ---- tracks (folded through core so the final order is exactly what core will produce)
    let mut sim = project.clone();
    let mut tracks = Vec::new();
    let mut created_track_ids = Vec::new();
    for need in &needs {
        if let Some(existing) = project.tracks.iter().find(|track| track.id == need.id) {
            if let Some(name) = need.name.as_ref().filter(|name| **name != existing.name) {
                warnings.push(
                    ImportWarning::new("CODEXVIDEO_HANDOFF_TRACK_NAME_DIFFERED", format!("Track \"{}\" already exists with a different name; the existing track was kept.", need.id))
                        .with("trackId", &need.id)
                        .with("manifestName", name)
                        .with("existingName", &existing.name),
                );
            }
            tracks.push(TrackEntry { manifest_track_id: need.id.clone(), track_id: need.id.clone(), kind: existing.kind, name: existing.name.clone(), status: TrackStatus::Existing, inferred: need.inferred });
            continue;
        }
        let insert_before_track_id = if need.kind == TrackKind::Video && is_character_track(&need.id) {
            sim.tracks.iter().find(|track| track.kind == TrackKind::Video && is_visual_track(&track.id)).map(|track| track.id.clone())
        } else {
            None
        };
        let operation = Operation::AddTrack(AddTrack {
            id: Some(need.id.clone()),
            kind: need.kind,
            name: need.name.clone(),
            insert_before_track_id,
            insert_after_track_id: None,
            locked: None,
            muted: None,
        });
        sim = avs_core::apply(&sim, &operation)?.0;
        operations.push(operation);
        created_track_ids.push(need.id.clone());
        tracks.push(TrackEntry { manifest_track_id: need.id.clone(), track_id: need.id.clone(), kind: need.kind, name: String::new(), status: TrackStatus::Created, inferred: need.inferred });
    }

    // ---- media
    for copy in prepared {
        operations.push(Operation::RegisterMedia(copy.register_media.clone()));
    }

    // ---- clips
    // Embedded audio goes to A1 when it is open for editing, else the first open audio track.
    let audio_target = {
        let open: Vec<_> = sim.tracks.iter().filter(|track| track.kind == TrackKind::Audio && !track.is_locked()).collect();
        open.iter().find(|track| track.id == "A1").or(open.first()).map(|track| track.id.clone())
    };
    let mut reserved: std::collections::HashSet<String> = project.clips().map(|clip| clip.id.clone()).collect();
    reserved.extend(manifest.clips.iter().map(|clip| clip.id.clone()));
    reserved.extend(manifest.captions.iter().map(|caption| caption.id.clone()));
    let mut add_clips = Vec::new();
    let mut clip_entries = Vec::new();
    for clip in &manifest.clips {
        let index = manifest.assets.iter().position(|asset| asset.id == clip.asset_id).unwrap_or_default();
        let (asset, copy) = (&manifest.assets[index], &prepared[index]);
        let media_id = copy.register_media.id.clone().unwrap_or_default();
        let metadata = clip_metadata(manifest, clip, asset);
        let audio = audio_properties(&clip.props)?;

        let linked_target = (asset.media_type == MediaType::Video && has_embedded_audio(copy)).then(|| audio_target.clone()).flatten();
        let linked_audio = linked_target.map(|track_id| {
            let base = format!("{}-audio", clip.id);
            let mut candidate = base.clone();
            let mut suffix = 1;
            while reserved.contains(&candidate) {
                candidate = format!("{base}-{suffix:03}");
                suffix += 1;
            }
            reserved.insert(candidate.clone());
            LinkedAudio { id: Some(candidate), track_id, name: None, properties: Some(ClipProperties { metadata: Some(metadata.clone()), ..audio.clone() }) }
        });
        if linked_audio.is_none() && asset.media_type != MediaType::Audio && clip.props.has_audio() {
            warnings.push(
                ImportWarning::new("CODEXVIDEO_HANDOFF_AUDIO_PROPERTIES_IGNORED", format!("Clip \"{}\" has volume, mute or fade values, but its media has no audio to apply them to.", clip.id))
                    .with("clipId", &clip.id)
                    .with("assetId", &asset.id),
            );
        }
        let mut duration = time(clip.duration_sec, "durationSec")?;
        let start = time(clip.start_sec, "startSec")?;
        let source_start = clip.source_start_sec.map(|s| time(s, "sourceStartSec")).transpose()?;
        if !asset.media_type.is_still() {
            let available = copy.register_media.duration_seconds - source_start.unwrap_or(Time::ZERO);
            let overshoot = duration - available;
            if overshoot.is_positive() && overshoot <= MAX_OVERSHOOT_TRIMMED && available.is_positive() {
                warnings.push(
                    ImportWarning::new(
                        "CODEXVIDEO_HANDOFF_CLIP_TRIMMED_TO_MEDIA",
                        format!("Clip \"{}\" ran {}s past the end of its media (ffprobe measured it shorter than the manifest); it was cut to the file.", clip.id, overshoot.secs()),
                    )
                    .with("clipId", &clip.id)
                    .with("assetId", &asset.id)
                    .with("manifestDurationSeconds", clip.duration_sec)
                    .with("durationSeconds", available.secs())
                    .with("mediaDurationSeconds", copy.register_media.duration_seconds.secs()),
                );
                duration = available;
            }
        }
        let mut properties = visual_properties(asset, &metadata, &clip.props, frame);
        if asset.media_type == MediaType::Audio {
            properties.volume_db = audio.volume_db;
            properties.muted = audio.muted;
            properties.fade_in_seconds = audio.fade_in_seconds;
            properties.fade_out_seconds = audio.fade_out_seconds;
        }
        properties.metadata = Some(metadata);
        clip_entries.push(ClipEntry {
            manifest_clip_id: clip.id.clone(),
            clip_id: clip.id.clone(),
            manifest_asset_id: clip.asset_id.clone(),
            media_id: media_id.clone(),
            track_id: clip.track_id.clone(),
            start_seconds: start.secs(),
            duration_seconds: duration.secs(),
            linked_audio_clip_id: linked_audio.as_ref().and_then(|l| l.id.clone()),
            linked_audio_track_id: linked_audio.as_ref().map(|l| l.track_id.clone()),
            link_group_id: None,
        });
        add_clips.push(AddClip {
            id: Some(clip.id.clone()),
            track_id: clip.track_id.clone(),
            media_id,
            start_seconds: start,
            duration_seconds: Some(duration),
            source_start_seconds: source_start,
            name: None,
            properties: Some(properties),
            linked_audio,
        });
    }
    if !add_clips.is_empty() {
        operations.push(Operation::AddClips(AddClips { clips: add_clips, edit_mode: avs_core::EditMode::Strict }));
    }

    // ---- captions
    let mut caption_entries = Vec::new();
    if !manifest.captions.is_empty() {
        let default_track = sim.tracks.iter().find(|track| track.kind == TrackKind::Caption).map(|track| track.id.clone());
        let mut captions = Vec::new();
        for caption in &manifest.captions {
            let Some(track_id) = caption.track_id.clone().or_else(|| default_track.clone()) else {
                return Err(HandoffError::invalid("CODEXVIDEO_HANDOFF_NO_CAPTION_TRACK", "The manifest has captions without a trackId, and the project has no caption track.").with("captionId", &caption.id));
            };
            caption_entries.push(CaptionEntry {
                manifest_caption_id: caption.id.clone(),
                caption_id: caption.id.clone(),
                track_id: track_id.clone(),
                start_seconds: caption.start_sec,
                duration_seconds: caption.duration_sec,
            });
            captions.push(CaptionInput {
                id: Some(caption.id.clone()),
                track_id: Some(track_id),
                text: caption.text.clone(),
                start_seconds: time(caption.start_sec, "startSec")?,
                duration_seconds: Some(time(caption.duration_sec, "durationSec")?),
                end_seconds: None,
                speaker: caption.speaker.clone(),
                language: caption.language.clone(),
                properties: Some(caption_properties(manifest, caption)),
            });
        }
        operations.push(Operation::AddCaptions(AddCaptions { track_id: None, captions }));
    }

    Ok(Plan { operations, tracks, clips: clip_entries, captions: caption_entries, warnings, created_track_ids, settings_changed })
}
