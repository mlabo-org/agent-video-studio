//! Reading and checking an `avs.codexvideo-handoff.v1` manifest.
//!
//! The manifest is parsed by walking the JSON rather than with derived structs, so each
//! bad field gets its own stable `CODEXVIDEO_HANDOFF_*` code and a `details` object that
//! names the field. Nothing here touches the project or copies a file; everything that
//! can be rejected from the manifest and the local files alone is rejected here, before
//! the first media byte is copied.

use crate::ImportWarning;
use crate::error::{HandoffError, Result};
use avs_core::{Crop, MediaType, Position, TrackKind};
use avs_session::fsutil::lexical_clean;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = "avs.codexvideo-handoff.v1";
pub const PRODUCER: &str = "codex-video";
/// `endSec` may differ from `startSec + durationSec` by this much (the sender rounds to 6 decimals).
const TIMING_EPSILON_SECONDS: f64 = 0.000_001;

pub type Obj = Map<String, Value>;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestProject {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_sec: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Asset {
    pub id: String,
    pub media_type: MediaType,
    pub role: Option<String>,
    pub path: PathBuf,
    pub duration_sec: Option<f64>,
    /// `hash`, or `sha256` (the field the sender actually writes).
    pub hash: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub metadata: Option<Obj>,
}

#[derive(Debug, Clone)]
pub struct TrackDecl {
    pub id: String,
    pub kind: TrackKind,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ClipProps {
    pub position: Option<Position>,
    pub scale: Option<f64>,
    pub rotation_degrees: Option<f64>,
    pub opacity: Option<f64>,
    pub crop: Option<Crop>,
    pub volume_db: Option<f64>,
    pub muted: Option<bool>,
    pub fade_in_seconds: Option<f64>,
    pub fade_out_seconds: Option<f64>,
    pub metadata: Option<Obj>,
}

impl ClipProps {
    pub fn has_visual(&self) -> bool {
        self.position.is_some() || self.scale.is_some() || self.rotation_degrees.is_some() || self.opacity.is_some() || self.crop.is_some()
    }

    pub fn has_audio(&self) -> bool {
        self.volume_db.is_some() || self.muted.is_some() || self.fade_in_seconds.is_some() || self.fade_out_seconds.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct ClipSpec {
    pub id: String,
    pub track_id: String,
    pub asset_id: String,
    pub role: Option<String>,
    pub start_sec: f64,
    pub duration_sec: f64,
    pub source_start_sec: Option<f64>,
    pub props: ClipProps,
}

#[derive(Debug, Clone)]
pub struct CaptionSpec {
    pub id: String,
    pub track_id: Option<String>,
    pub start_sec: f64,
    pub duration_sec: f64,
    pub text: String,
    pub language: Option<String>,
    pub speaker: Option<String>,
    pub style: Option<Value>,
    pub metadata: Option<Obj>,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    /// Absolute, lexically cleaned path of the manifest file.
    pub path: PathBuf,
    pub created_at: Option<String>,
    pub handoff: Option<Obj>,
    pub job: Option<Obj>,
    pub project: Option<ManifestProject>,
    pub assets: Vec<Asset>,
    pub tracks: Vec<TrackDecl>,
    pub clips: Vec<ClipSpec>,
    pub captions: Vec<CaptionSpec>,
    pub audio: Option<Obj>,
    /// The producer's own `warnings`, passed through untouched.
    pub warnings: Vec<Value>,
    pub unresolved: Vec<Value>,
    /// Notices from reading: old spellings accepted (`mediaId`, `captions`), a `jobRoot` not recorded.
    pub compat_warnings: Vec<ImportWarning>,
}

impl Manifest {
    pub fn job_string(&self, key: &str) -> Option<String> {
        let value = self.job.as_ref()?.get(key)?.as_str()?;
        (!value.trim().is_empty()).then(|| value.to_string())
    }

    pub fn asset(&self, id: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.id == id)
    }
}

fn fail(code: &str, message: impl Into<String>, details: Value) -> HandoffError {
    let mut error = HandoffError::invalid(code, message);
    error.details = details;
    error
}

fn invalid(message: impl Into<String>, details: Value) -> HandoffError {
    fail("CODEXVIDEO_HANDOFF_INVALID_MANIFEST", message, details)
}

fn is_missing(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}

// ---------------------------------------------------------------- the file

/// Reads the manifest file: absolute, a regular file (not a symlink), UTF-8 JSON, then parses it.
pub fn read(manifest_path: &Path) -> Result<Manifest> {
    let shown = manifest_path.display().to_string();
    if shown.trim().is_empty() {
        return Err(fail("CODEXVIDEO_HANDOFF_MANIFEST_REQUIRED", "manifestPath must be a non-empty string.", json!({ "fieldName": "manifestPath" })));
    }
    if shown.contains('\0') {
        return Err(fail("CODEXVIDEO_HANDOFF_MANIFEST_INVALID_PATH", "manifestPath must not include NUL characters.", json!({ "manifestPath": shown })));
    }
    if !manifest_path.is_absolute() {
        return Err(fail("CODEXVIDEO_HANDOFF_MANIFEST_NOT_ABSOLUTE", "manifestPath must be an absolute local path.", json!({ "manifestPath": shown })));
    }
    let path = lexical_clean(manifest_path);
    let details = json!({ "manifestPath": path.display().to_string() });
    let link = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if is_missing(&e) => {
            return Err(HandoffError::not_found("CODEXVIDEO_HANDOFF_MANIFEST_MISSING", "manifestPath does not exist.").with("manifestPath", path.display().to_string()));
        }
        Err(e) => return Err(avs_session::SessionError::io("stat", &path, &e).into()),
    };
    if link.file_type().is_symlink() {
        return Err(fail("CODEXVIDEO_HANDOFF_MANIFEST_SYMLINK", "manifestPath must be a direct JSON file, not a symlink.", details));
    }
    if link.is_dir() {
        return Err(fail("CODEXVIDEO_HANDOFF_MANIFEST_DIRECTORY", "manifestPath must be a JSON file, not a directory.", details));
    }
    if !link.is_file() {
        return Err(fail("CODEXVIDEO_HANDOFF_MANIFEST_NOT_FILE", "manifestPath must point to a regular JSON file.", details));
    }
    let bytes = fs::read(&path).map_err(|e| {
        fail("CODEXVIDEO_HANDOFF_MANIFEST_UNREADABLE", "Manifest JSON file could not be read.", json!({ "manifestPath": path.display().to_string(), "cause": e.to_string() }))
    })?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| {
        fail("CODEXVIDEO_HANDOFF_MANIFEST_JSON_INVALID", "Manifest path did not contain readable JSON.", json!({ "manifestPath": path.display().to_string(), "cause": e.to_string() }))
    })?;
    parse(&value, path)
}

/// `manifest.job.jobRoot`, when given, must be an absolute, existing, direct directory
/// (not a symlink). The project keeps it as the place exports may be written, so it is
/// recorded only when the manifest file lives inside it; a manifest exported elsewhere
/// is still imported, with a warning and without `jobRoot`.
pub fn validate_job_root(manifest: &mut Manifest) -> Result<Option<PathBuf>> {
    let Some(value) = manifest.job.as_ref().and_then(|job| job.get("jobRoot")).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let Some(text) = value.as_str().map(str::trim).filter(|text| !text.is_empty()) else {
        return Err(fail("CODEXVIDEO_HANDOFF_JOB_ROOT_INVALID_PATH", "manifest.job.jobRoot must be a non-empty string when provided.", json!({ "jobRoot": value })));
    };
    if text.contains('\0') {
        return Err(fail("CODEXVIDEO_HANDOFF_JOB_ROOT_INVALID_PATH", "manifest.job.jobRoot must not include NUL characters.", json!({ "jobRoot": text })));
    }
    if !Path::new(text).is_absolute() {
        return Err(fail("CODEXVIDEO_HANDOFF_JOB_ROOT_NOT_ABSOLUTE", "manifest.job.jobRoot must be an absolute local directory.", json!({ "jobRoot": text })));
    }
    let root = lexical_clean(Path::new(text));
    let details = json!({ "jobRoot": root.display().to_string() });
    let link = match fs::symlink_metadata(&root) {
        Ok(meta) => meta,
        Err(e) if is_missing(&e) => {
            return Err(HandoffError::not_found("CODEXVIDEO_HANDOFF_JOB_ROOT_MISSING", "manifest.job.jobRoot does not exist.").with("jobRoot", root.display().to_string()));
        }
        Err(e) => return Err(avs_session::SessionError::io("stat", &root, &e).into()),
    };
    if link.file_type().is_symlink() {
        return Err(fail("CODEXVIDEO_HANDOFF_JOB_ROOT_SYMLINK", "manifest.job.jobRoot must be a direct directory, not a symlink.", details));
    }
    if !link.is_dir() {
        return Err(fail("CODEXVIDEO_HANDOFF_JOB_ROOT_NOT_DIRECTORY", "manifest.job.jobRoot must point to a local directory.", details));
    }
    // Compare real paths so `/var` against `/private/var` (or any symlinked parent) cannot
    // make a contained manifest look foreign, or the reverse.
    let inside = match (fs::canonicalize(&manifest.path), fs::canonicalize(&root)) {
        (Ok(file), Ok(dir)) => file.starts_with(dir),
        _ => false,
    };
    if !inside {
        manifest.compat_warnings.push(
            ImportWarning::new(
                "CODEXVIDEO_HANDOFF_JOB_ROOT_NOT_RECORDED",
                "The manifest file is not inside manifest.job.jobRoot, so jobRoot was not recorded in the project and exports will not be allowed under it. Export the handoff into the job folder (jobRoot/avs/) to get that.",
            )
            .with("manifestPath", manifest.path.display().to_string())
            .with("jobRoot", root.display().to_string()),
        );
        return Ok(None);
    }
    Ok(Some(root))
}

/// Every asset must be an absolute path to a readable regular file, not a symlink.
pub fn check_asset_files(assets: &[Asset]) -> Result<()> {
    for asset in assets {
        let details = json!({ "assetId": asset.id, "assetPath": asset.path.display().to_string() });
        if !asset.path.is_absolute() {
            return Err(fail("CODEXVIDEO_HANDOFF_MISSING_ASSET", "CodexVideo asset path must be absolute.", details));
        }
        let link = match fs::symlink_metadata(&asset.path) {
            Ok(meta) => meta,
            Err(e) if is_missing(&e) => return Err(fail("CODEXVIDEO_HANDOFF_MISSING_ASSET", "CodexVideo asset path does not exist.", details)),
            Err(e) => return Err(avs_session::SessionError::io("stat", &asset.path, &e).into()),
        };
        if link.file_type().is_symlink() {
            return Err(fail("CODEXVIDEO_HANDOFF_ASSET_SYMLINK", "CodexVideo asset path must be a direct media file, not a symlink.", details));
        }
        if link.is_dir() {
            return Err(fail("CODEXVIDEO_HANDOFF_ASSET_DIRECTORY", "CodexVideo asset path must be a file, not a directory.", details));
        }
        if !link.is_file() {
            return Err(fail("CODEXVIDEO_HANDOFF_ASSET_NOT_FILE", "CodexVideo asset path must point to a regular file.", details));
        }
        if let Err(e) = fs::File::open(&asset.path) {
            let mut details = details;
            details["cause"] = json!(e.to_string());
            return Err(fail("CODEXVIDEO_HANDOFF_ASSET_UNREADABLE", "CodexVideo asset path is not readable.", details));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- parsing

fn present<'a>(obj: &'a Obj, key: &str) -> Option<&'a Value> {
    obj.get(key).filter(|value| !value.is_null())
}

fn req_str(obj: &Obj, key: &str, field: &str) -> Result<String> {
    match present(obj, key).and_then(Value::as_str).map(str::trim) {
        Some(text) if !text.is_empty() => Ok(text.to_string()),
        _ => Err(invalid(format!("{field} must be a non-empty string."), json!({ "fieldName": field }))),
    }
}

fn opt_str(obj: &Obj, key: &str) -> Result<Option<String>> {
    match present(obj, key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.trim().to_string()).filter(|text| !text.is_empty())),
        Some(other) => Err(invalid("Expected a string when provided.", json!({ "key": key, "value": other }))),
    }
}

fn opt_object(obj: &Obj, key: &str) -> Result<Option<Obj>> {
    match present(obj, key) {
        None => Ok(None),
        Some(Value::Object(map)) => Ok(Some(map.clone())),
        Some(other) => Err(invalid("Expected a JSON object.", json!({ "key": key, "value": other }))),
    }
}

fn array<'a>(obj: &'a Obj, key: &str) -> Result<&'a [Value]> {
    match present(obj, key) {
        None => Ok(&[]),
        Some(Value::Array(items)) => Ok(items),
        Some(_) => Err(invalid(format!("{key} must be an array when provided."), json!({ "fieldName": key }))),
    }
}

fn json_array(obj: &Obj, key: &str) -> Result<Vec<Value>> {
    Ok(array(obj, key)?.to_vec())
}

fn entry<'a>(value: &'a Value, what: &str) -> Result<&'a Obj> {
    value.as_object().ok_or_else(|| invalid(format!("Each {what}[] entry must be an object."), json!({ "entry": value })))
}

fn number(obj: &Obj, key: &str) -> Option<f64> {
    present(obj, key).and_then(Value::as_f64).filter(|n| n.is_finite())
}

/// A timing field: finite number, with the given lower bound (`positive` excludes zero).
fn timing(obj: &Obj, key: &str, field: &str, positive: bool) -> Result<Option<f64>> {
    let Some(value) = present(obj, key) else { return Ok(None) };
    match value.as_f64().filter(|n| n.is_finite() && if positive { *n > 0.0 } else { *n >= 0.0 }) {
        Some(n) => Ok(Some(n)),
        None => Err(fail(
            "CODEXVIDEO_HANDOFF_INVALID_TIMING",
            format!("{field} must be a {} finite number.", if positive { "positive" } else { "non-negative" }),
            json!({ "fieldName": field, "value": value }),
        )),
    }
}

fn req_timing(obj: &Obj, key: &str, field: &str, positive: bool) -> Result<f64> {
    timing(obj, key, field, positive)?.ok_or_else(|| {
        fail(
            "CODEXVIDEO_HANDOFF_INVALID_TIMING",
            format!("{field} must be a {} finite number.", if positive { "positive" } else { "non-negative" }),
            json!({ "fieldName": field, "value": Value::Null }),
        )
    })
}

fn pos_int(obj: &Obj, key: &str, field: &str) -> Result<Option<u32>> {
    let Some(value) = present(obj, key) else { return Ok(None) };
    match value.as_f64().filter(|n| n.is_finite() && *n > 0.0 && n.fract() == 0.0 && *n <= f64::from(u32::MAX)) {
        Some(n) => Ok(Some(n as u32)),
        None => Err(fail("CODEXVIDEO_HANDOFF_INVALID_PROJECT", format!("{field} must be a positive integer."), json!({ "fieldName": field, "value": value }))),
    }
}

/// A clip-property number: `finite`, `>= 0`, `> 0` or within `0..=1`.
#[derive(Clone, Copy)]
enum Bound {
    Finite,
    NonNegative,
    Positive,
    Unit,
}

fn prop(obj: &Obj, key: &str, field: &str, bound: Bound) -> Result<Option<f64>> {
    let Some(value) = present(obj, key) else { return Ok(None) };
    let (ok, what) = match (value.as_f64().filter(|n| n.is_finite()), bound) {
        (Some(_), Bound::Finite) => (true, "a finite number"),
        (Some(n), Bound::NonNegative) => (n >= 0.0, "a non-negative finite number"),
        (Some(n), Bound::Positive) => (n > 0.0, "a positive finite number"),
        (Some(n), Bound::Unit) => ((0.0..=1.0).contains(&n), "a number between 0 and 1"),
        (None, Bound::Finite) => (false, "a finite number"),
        (None, Bound::NonNegative) => (false, "a non-negative finite number"),
        (None, Bound::Positive) => (false, "a positive finite number"),
        (None, Bound::Unit) => (false, "a number between 0 and 1"),
    };
    if ok { Ok(value.as_f64()) } else { Err(invalid(format!("{field} must be {what}."), json!({ "fieldName": field, "value": value }))) }
}

fn parse_project(root: &Obj) -> Result<Option<ManifestProject>> {
    let Some(project) = opt_object(root, "project").map_err(|_| fail("CODEXVIDEO_HANDOFF_INVALID_PROJECT", "manifest.project must be an object when provided.", json!({ "project": root.get("project") })))? else {
        return Ok(None);
    };
    let width = pos_int(&project, "width", "project.width")?;
    let height = pos_int(&project, "height", "project.height")?;
    if width.is_some() != height.is_some() {
        return Err(fail(
            "CODEXVIDEO_HANDOFF_INVALID_PROJECT",
            "manifest.project width and height must be provided together.",
            json!({ "width": project.get("width"), "height": project.get("height") }),
        ));
    }
    let positive = |key: &str, field: &str| -> Result<Option<f64>> {
        let Some(value) = present(&project, key) else { return Ok(None) };
        value
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0.0)
            .map(Some)
            .ok_or_else(|| fail("CODEXVIDEO_HANDOFF_INVALID_TIMING", format!("{field} must be a positive finite number."), json!({ "fieldName": field, "value": value })))
    };
    Ok(Some(ManifestProject {
        title: opt_str(&project, "title")?,
        width,
        height,
        fps: positive("fps", "project.fps")?,
        duration_sec: positive("durationSec", "project.durationSec")?,
        language: opt_str(&project, "language")?,
    }))
}

fn parse_asset(value: &Value) -> Result<Asset> {
    let obj = entry(value, "assets")?;
    let media_type = match present(obj, "type").and_then(Value::as_str) {
        Some("video") => MediaType::Video,
        Some("audio") => MediaType::Audio,
        Some("image") => MediaType::Image,
        _ => return Err(fail("CODEXVIDEO_HANDOFF_UNSUPPORTED_ASSET_TYPE", "assets[].type must be video, audio, or image.", json!({ "type": obj.get("type") }))),
    };
    let hash = match opt_str(obj, "hash")? {
        Some(hash) => Some(hash),
        None => opt_str(obj, "sha256")?,
    };
    Ok(Asset {
        id: req_str(obj, "id", "assets[].id")?,
        media_type,
        role: opt_str(obj, "role")?,
        path: PathBuf::from(req_str(obj, "path", "assets[].path")?),
        duration_sec: timing(obj, "durationSec", "assets[].durationSec", true)?,
        hash,
        width: pos_int(obj, "width", "assets[].width")?,
        height: pos_int(obj, "height", "assets[].height")?,
        metadata: opt_object(obj, "metadata")?,
    })
}

fn parse_track(value: &Value, compat: &mut Vec<ImportWarning>) -> Result<TrackDecl> {
    let obj = entry(value, "tracks")?;
    let kind = match present(obj, "kind").and_then(Value::as_str) {
        Some("video") => TrackKind::Video,
        Some("audio") => TrackKind::Audio,
        Some("caption") => TrackKind::Caption,
        Some("captions") => {
            compat.push(ImportWarning::new("CODEXVIDEO_HANDOFF_LEGACY_TRACK_KIND_ALIAS", "tracks[].kind \"captions\" was read as \"caption\".").with("legacyValue", "captions").with("canonicalValue", "caption"));
            TrackKind::Caption
        }
        _ => return Err(invalid("tracks[].kind must be video, audio, or caption.", json!({ "kind": obj.get("kind") }))),
    };
    Ok(TrackDecl { id: req_str(obj, "id", "tracks[].id")?, kind, name: opt_str(obj, "name")? })
}

fn parse_clip_props(clip: &Obj) -> Result<ClipProps> {
    let Some(props) = opt_object(clip, "properties")? else { return Ok(ClipProps::default()) };
    let position = match present(&props, "position") {
        None => None,
        Some(Value::Object(p)) => match (number(p, "x"), number(p, "y")) {
            (Some(x), Some(y)) => Some(Position { x, y }),
            _ => return Err(invalid("clips[].properties.position must include finite x and y values.", json!({ "value": props.get("position") }))),
        },
        Some(other) => return Err(invalid("clips[].properties.position must be an object when provided.", json!({ "value": other }))),
    };
    let crop = match present(&props, "crop") {
        None => None,
        Some(Value::Object(c)) => {
            let side = |key: &str| prop(c, key, &format!("clips[].properties.crop.{key}"), Bound::Unit);
            let (left, right, top, bottom) = (side("left")?, side("right")?, side("top")?, side("bottom")?);
            let (Some(left), Some(right), Some(top), Some(bottom)) = (left, right, top, bottom) else {
                return Err(invalid("clips[].properties.crop must include left, right, top, and bottom values.", json!({ "value": props.get("crop") })));
            };
            if left + right >= 1.0 || top + bottom >= 1.0 {
                return Err(invalid("clips[].properties.crop must leave a visible image area.", json!({ "value": props.get("crop") })));
            }
            Some(Crop { left, right, top, bottom })
        }
        Some(other) => return Err(invalid("clips[].properties.crop must be an object when provided.", json!({ "value": other }))),
    };
    let muted = match present(&props, "muted") {
        None => None,
        Some(Value::Bool(flag)) => Some(*flag),
        Some(other) => return Err(invalid("clips[].properties.muted must be a boolean.", json!({ "value": other }))),
    };
    Ok(ClipProps {
        position,
        scale: prop(&props, "scale", "clips[].properties.scale", Bound::Positive)?,
        rotation_degrees: prop(&props, "rotationDegrees", "clips[].properties.rotationDegrees", Bound::Finite)?,
        opacity: prop(&props, "opacity", "clips[].properties.opacity", Bound::Unit)?,
        crop,
        volume_db: prop(&props, "volumeDb", "clips[].properties.volumeDb", Bound::Finite)?,
        muted,
        fade_in_seconds: prop(&props, "fadeInSeconds", "clips[].properties.fadeInSeconds", Bound::NonNegative)?,
        fade_out_seconds: prop(&props, "fadeOutSeconds", "clips[].properties.fadeOutSeconds", Bound::NonNegative)?,
        metadata: match props.get("metadata") {
            Some(Value::Object(map)) => Some(map.clone()),
            _ => None,
        },
    })
}

fn check_end(obj: &Obj, what: &str, id: &str, start: f64, duration: f64, field: &str) -> Result<()> {
    if let Some(end) = timing(obj, "endSec", field, false)?
        && (end - (start + duration)).abs() > TIMING_EPSILON_SECONDS
    {
        return Err(fail(
            "CODEXVIDEO_HANDOFF_INVALID_TIMING",
            format!("{what} \"{id}\" endSec does not match startSec + durationSec."),
            json!({ "id": id, "startSec": start, "durationSec": duration, "endSec": end }),
        ));
    }
    Ok(())
}

fn parse_clip(value: &Value, assets: &[Asset], compat: &mut Vec<ImportWarning>) -> Result<ClipSpec> {
    let obj = entry(value, "clips")?;
    let id = req_str(obj, "id", "clips[].id")?;
    let asset_id = opt_str(obj, "assetId")?;
    let legacy = opt_str(obj, "mediaId")?;
    if let (Some(a), Some(b)) = (&asset_id, &legacy)
        && a != b
    {
        return Err(invalid("clips[] cannot provide conflicting assetId and legacy mediaId values.", json!({ "clipId": id, "assetId": a, "mediaId": b })));
    }
    if asset_id.is_none() && legacy.is_some() {
        compat.push(
            ImportWarning::new("CODEXVIDEO_HANDOFF_LEGACY_CLIP_MEDIA_ID_ALIAS", format!("Clip \"{id}\" used the legacy mediaId field; it was read as assetId."))
                .with("clipId", &id)
                .with("legacyField", "mediaId")
                .with("canonicalField", "assetId"),
        );
    }
    let asset_id = match asset_id.or(legacy) {
        Some(asset_id) => asset_id,
        None => return Err(invalid("clips[].assetId must be a non-empty string.", json!({ "fieldName": "clips[].assetId", "clipId": id }))),
    };
    if !assets.iter().any(|asset| asset.id == asset_id) {
        return Err(fail("CODEXVIDEO_HANDOFF_UNKNOWN_MEDIA", format!("Clip \"{id}\" references unknown manifest asset \"{asset_id}\"."), json!({ "clipId": id, "assetId": asset_id })));
    }
    let start_sec = req_timing(obj, "startSec", "clips[].startSec", false)?;
    let duration_sec = req_timing(obj, "durationSec", "clips[].durationSec", true)?;
    check_end(obj, "Clip", &id, start_sec, duration_sec, "clips[].endSec")?;
    Ok(ClipSpec {
        track_id: req_str(obj, "trackId", "clips[].trackId")?,
        asset_id,
        role: opt_str(obj, "role")?,
        start_sec,
        duration_sec,
        source_start_sec: timing(obj, "sourceStartSec", "clips[].sourceStartSec", false)?,
        props: parse_clip_props(obj)?,
        id,
    })
}

fn parse_caption(value: &Value) -> Result<CaptionSpec> {
    let obj = entry(value, "captions")?;
    let id = req_str(obj, "id", "captions[].id")?;
    let start_sec = req_timing(obj, "startSec", "captions[].startSec", false)?;
    let duration_sec = req_timing(obj, "durationSec", "captions[].durationSec", true)?;
    check_end(obj, "Caption", &id, start_sec, duration_sec, "captions[].endSec")?;
    let text = req_str(obj, "text", "captions[].text")?;
    Ok(CaptionSpec {
        track_id: opt_str(obj, "trackId")?,
        start_sec,
        duration_sec,
        text,
        language: opt_str(obj, "language")?,
        speaker: opt_str(obj, "speaker")?,
        style: present(obj, "style").cloned(),
        metadata: opt_object(obj, "metadata")?,
        id,
    })
}

fn unique(field: &str, ids: impl Iterator<Item = String>) -> Result<()> {
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id.clone()) {
            return Err(invalid(format!("{field} contains duplicate ids."), json!({ "fieldName": field, "id": id })));
        }
    }
    Ok(())
}

pub fn parse(value: &Value, path: PathBuf) -> Result<Manifest> {
    let Some(root) = value.as_object() else {
        return Err(invalid("CodexVideo handoff manifest must be a JSON object.", json!({ "manifestPath": path.display().to_string() })));
    };
    if root.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(fail(
            "CODEXVIDEO_HANDOFF_UNSUPPORTED_SCHEMA",
            "Unsupported CodexVideo handoff manifest schema.",
            json!({ "expectedSchema": SCHEMA, "actualSchema": root.get("schema"), "manifestPath": path.display().to_string() }),
        ));
    }
    if root.get("producer").and_then(Value::as_str) != Some(PRODUCER) {
        return Err(fail(
            "CODEXVIDEO_HANDOFF_UNSUPPORTED_PRODUCER",
            "Unsupported CodexVideo handoff manifest producer.",
            json!({ "expectedProducer": PRODUCER, "actualProducer": root.get("producer"), "manifestPath": path.display().to_string() }),
        ));
    }
    let mut compat = Vec::new();
    let assets = array(root, "assets")?.iter().map(parse_asset).collect::<Result<Vec<_>>>()?;
    let tracks = array(root, "tracks")?.iter().map(|track| parse_track(track, &mut compat)).collect::<Result<Vec<_>>>()?;
    let clips = array(root, "clips")?.iter().map(|clip| parse_clip(clip, &assets, &mut compat)).collect::<Result<Vec<_>>>()?;
    let captions = array(root, "captions")?.iter().map(parse_caption).collect::<Result<Vec<_>>>()?;
    unique("assets", assets.iter().map(|asset| asset.id.clone()))?;
    unique("tracks", tracks.iter().map(|track| track.id.clone()))?;
    // Clips and captions are both clips on the timeline, so they share one id space.
    unique("clips", clips.iter().map(|clip| clip.id.clone()))?;
    unique("captions", captions.iter().map(|caption| caption.id.clone()))?;
    unique("clips and captions", clips.iter().map(|clip| clip.id.clone()).chain(captions.iter().map(|caption| caption.id.clone())))?;
    Ok(Manifest {
        created_at: opt_str(root, "createdAt")?,
        handoff: opt_object(root, "handoff")?,
        job: opt_object(root, "job")?,
        project: parse_project(root)?,
        audio: opt_object(root, "audio")?,
        warnings: json_array(root, "warnings")?,
        unresolved: json_array(root, "unresolved")?,
        compat_warnings: compat,
        path,
        assets,
        tracks,
        clips,
        captions,
    })
}
