//! Import of a CodexVideo handoff manifest (`avs.codexvideo-handoff.v1`) into the
//! current project.
//!
//! [`import_handoff`] reads and checks the manifest, copies its media into the project
//! library, turns everything else into `avs_core::Operation`s and saves them with one
//! `Session::apply_many` call: one revision, one undo step, all or nothing. If anything
//! fails, nothing is saved and the copied media is deleted again.
//!
//! # What the manifest becomes
//!
//! | manifest | AVS |
//! |---|---|
//! | `project.title` / `fps` / `width`x`height` | project title / frame rate / resolution (see [`ImportOptions`]) |
//! | `job.jobId`, `profile`, `jobRoot`; `createdAt`; `audio` (`sourceAudioPreserved`, `mutedIntervals`) | project `metadata.codexVideo` |
//! | `assets[]` | media copied into the library, registered with `metadata.codexVideo` |
//! | `tracks[]`, tracks implied by `clips[]` / `captions[]` | tracks with the manifest's ids (existing ones of the same kind are reused) |
//! | `clips[]` | clips with the manifest's ids; `properties` copied; video with embedded audio gets a linked audio clip `<id>-audio` |
//! | character clips without a transform | default overlay position, scale and opacity from `metadata.codexVideo.character` |
//! | `captions[]` | caption clips with the manifest's ids; object `style` becomes `captionStyle` |
//!
//! Not imported: `tracks[].role`, `clips[].kind`, `clips[].endSec` (only checked),
//! `project.durationSec` / `language` (reported in `manifest.project`), the manifest's
//! top-level `metadata`. A clip that runs up to 100 ms past the end of its media is cut
//! to the file (with a warning); a larger overrun is refused. `warnings` and `unresolved` are passed through in the report.
//!
//! # Public API
//! - `import_handoff(&Env, &Session, &Path, &ImportOptions) -> Result<ImportReport, HandoffError>`
//! - `ImportOptions`, `ImportReport` (and its entry types), `ImportWarning`, `HandoffError`

#![deny(unsafe_code)]

pub mod error;
mod manifest;
mod plan;

pub use error::{HandoffError, Result};
pub use manifest::ManifestProject;

use avs_core::query::Inspection;
use avs_core::{MediaType, Resolution};
use avs_session::media::ImportedLibraryInfo;
use avs_session::{Env, ImportRequest, PreparedImport, Session, SessionError, discard_import, prepare_import};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::Path;

/// Arguments of the import beyond the manifest path.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOptions {
    /// Fail with `REVISION_MISMATCH` unless the project is at exactly this revision.
    #[serde(default)]
    pub expected_revision: Option<u64>,
    /// Title for the project; wins over the manifest's `project.title`.
    #[serde(default)]
    pub project_title: Option<String>,
    /// Leave the project's frame rate, resolution and (unless `projectTitle` is given) title
    /// as they are. By default the manifest's frame rate and resolution replace them, as the
    /// earlier implementation did (a warning records the change when the project already
    /// had clips); the manifest's title is applied only to a project that has no clips yet.
    #[serde(default)]
    pub keep_project_settings: bool,
}

/// Something the importer noticed that does not stop the import.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportWarning {
    /// Stable code, e.g. `CODEXVIDEO_HANDOFF_TRACK_INFERRED`.
    pub code: String,
    pub message: String,
    pub details: Value,
}

impl ImportWarning {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
        ImportWarning { code: code.to_string(), message: message.into(), details: Value::Object(Map::new()) }
    }

    pub(crate) fn with(mut self, key: &str, value: impl Serialize) -> Self {
        if let Value::Object(map) = &mut self.details {
            map.insert(key.to_string(), serde_json::to_value(value).unwrap_or(Value::Null));
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSummary {
    pub path: String,
    pub schema: &'static str,
    pub producer: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ManifestProject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<Map<String, Value>>,
    /// The producer's own `warnings`, unchanged.
    pub warnings: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    pub project_id: String,
    pub title: String,
    pub fps: f64,
    pub resolution: Resolution,
    /// Whether this import changed the project's title, frame rate or resolution.
    pub settings_changed: bool,
}

/// Manifest asset id -> AVS media.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaEntry {
    pub manifest_asset_id: String,
    pub media_id: String,
    #[serde(rename = "type")]
    pub media_type: MediaType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// `/project-library/media/...`
    pub source: String,
    pub library: ImportedLibraryInfo,
    /// The hash the manifest recorded, as written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackStatus {
    /// The project already had a track with this id and kind.
    Existing,
    Created,
}

/// Manifest track id -> AVS track.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackEntry {
    pub manifest_track_id: String,
    pub track_id: String,
    pub kind: avs_core::TrackKind,
    pub name: String,
    pub status: TrackStatus,
    /// The manifest did not declare it; a clip or caption implied it.
    pub inferred: bool,
}

/// Manifest clip id -> AVS clip (and its linked audio clip, when one was made).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipEntry {
    pub manifest_clip_id: String,
    pub clip_id: String,
    pub manifest_asset_id: String,
    pub media_id: String,
    pub track_id: String,
    pub start_seconds: f64,
    pub duration_seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_audio_clip_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_audio_track_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_group_id: Option<String>,
}

/// Manifest caption id -> AVS caption clip.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionEntry {
    pub manifest_caption_id: String,
    pub caption_id: String,
    pub track_id: String,
    pub start_seconds: f64,
    pub duration_seconds: f64,
}

/// Every id this import added to the project.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedIds {
    pub media_ids: Vec<String>,
    pub track_ids: Vec<String>,
    /// Media clips including linked audio clips, not captions.
    pub clip_ids: Vec<String>,
    pub caption_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationSummary {
    /// `avs_core::validate` on the saved project. Always true after a successful import.
    pub ok: bool,
    pub error_count: usize,
    pub warning_count: usize,
    pub unresolved_count: usize,
    /// No validation errors and nothing in the manifest's `unresolved` list.
    pub import_complete: bool,
}

/// What the import did, in terms an agent can use for the next edit.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub schema_version: u32,
    /// Revision the import was saved as (one revision, one undo step).
    pub revision: u64,
    pub updated_at: String,
    pub undo_depth: usize,
    pub manifest: ManifestSummary,
    pub project: ProjectInfo,
    pub media: Vec<MediaEntry>,
    pub tracks: Vec<TrackEntry>,
    pub clips: Vec<ClipEntry>,
    pub captions: Vec<CaptionEntry>,
    pub created: CreatedIds,
    /// Importer notices. The producer's own `warnings` are in `manifest.warnings`.
    pub warnings: Vec<ImportWarning>,
    /// The producer's list of things it could not resolve (e.g. scenes without a visual).
    pub unresolved: Vec<Value>,
    pub validation: ValidationSummary,
    /// `avs_core::query::inspect` of the project after the import.
    pub timeline: Inspection,
}

fn discard_all(prepared: &[PreparedImport]) {
    for copy in prepared {
        discard_import(copy);
    }
}

fn tags(asset: &manifest::Asset) -> Vec<String> {
    std::iter::once("codex-video".to_string()).chain(asset.role.clone()).collect()
}

fn copy_assets(env: &Env, session: &Session, manifest: &manifest::Manifest) -> Result<Vec<PreparedImport>> {
    let mut prepared = Vec::with_capacity(manifest.assets.len());
    for asset in &manifest.assets {
        let request = ImportRequest {
            source_path: asset.path.display().to_string(),
            media_type: Some(asset.media_type),
            duration_seconds: asset.duration_sec,
            name: asset.path.file_name().map(|name| name.to_string_lossy().into_owned()),
            tags: Some(tags(asset)),
            metadata: Some(plan::asset_metadata(manifest, asset)),
            ..ImportRequest::default()
        };
        match prepare_import(env, session, &request) {
            Ok(copy) => prepared.push(copy),
            Err(error) => {
                discard_all(&prepared);
                return Err(HandoffError::from(error).with("assetId", &asset.id).with("assetPath", asset.path.display().to_string()));
            }
        }
    }
    Ok(prepared)
}

fn check_revision(expected: Option<u64>, actual: u64) -> Result<()> {
    match expected {
        Some(expected) if expected != actual => Err(SessionError::revision_mismatch("REVISION_MISMATCH", "Project session", expected, actual).into()),
        _ => Ok(()),
    }
}

/// Imports the manifest at `manifest_path` into the project `session` points at.
///
/// Order of work: read and check the manifest and its files; check the project (it
/// exists, `expected_revision`, id and track conflicts) - all before any byte is copied;
/// copy the media; plan; save with one `apply_many` pinned to the revision the plan was
/// made against. A failure at any point after the copy deletes the copies.
pub fn import_handoff(env: &Env, session: &Session, manifest_path: &Path, options: &ImportOptions) -> Result<ImportReport> {
    let mut manifest = manifest::read(manifest_path)?;
    let job_root = manifest::validate_job_root(&mut manifest)?;
    manifest::check_asset_files(&manifest.assets)?;

    let early = session.load()?;
    check_revision(options.expected_revision, early.revision)?;
    let (needs, _) = plan::derive_tracks(&manifest)?;
    plan::check_against_project(&needs, &manifest, &early.project)?;

    let prepared = copy_assets(env, session, &manifest)?;
    match commit(session, &manifest, &prepared, options, job_root.as_deref()) {
        Ok(report) => Ok(report),
        Err(error) => {
            discard_all(&prepared);
            Err(error)
        }
    }
}

fn commit(session: &Session, manifest: &manifest::Manifest, prepared: &[PreparedImport], options: &ImportOptions, job_root: Option<&Path>) -> Result<ImportReport> {
    // Copying can take a while; plan against the project as it is now.
    let state = session.load()?;
    check_revision(options.expected_revision, state.revision)?;
    let plan = plan::build(&plan::Inputs { manifest, project: &state.project, prepared, options, job_root })?;
    let applied = session.apply_many(&plan.operations, Some(state.revision))?;
    let project = &applied.project;

    let media: Vec<MediaEntry> = manifest
        .assets
        .iter()
        .zip(prepared)
        .map(|(asset, copy)| MediaEntry {
            manifest_asset_id: asset.id.clone(),
            media_id: copy.register_media.id.clone().unwrap_or_default(),
            media_type: asset.media_type,
            role: asset.role.clone(),
            source: copy.library.media_source.clone(),
            library: copy.library.clone(),
            hash: asset.hash.clone(),
        })
        .collect();
    let tracks: Vec<TrackEntry> = plan
        .tracks
        .iter()
        .map(|entry| {
            let name = project.track(&entry.track_id).map(|track| track.name.clone()).unwrap_or_else(|_| entry.name.clone());
            TrackEntry { name, ..entry.clone() }
        })
        .collect();
    let clips: Vec<ClipEntry> = plan
        .clips
        .iter()
        .map(|entry| {
            let link_group_id = project.clip(&entry.clip_id).ok().and_then(|clip| clip.link.as_ref()).map(|link| link.group_id.clone());
            ClipEntry { link_group_id, ..entry.clone() }
        })
        .collect();
    let created = CreatedIds {
        media_ids: media.iter().map(|entry| entry.media_id.clone()).collect(),
        track_ids: plan.created_track_ids.clone(),
        clip_ids: clips.iter().flat_map(|entry| std::iter::once(entry.clip_id.clone()).chain(entry.linked_audio_clip_id.clone())).collect(),
        caption_ids: plan.captions.iter().map(|entry| entry.caption_id.clone()).collect(),
    };
    let validation = avs_core::validate(project);
    let unresolved_count = manifest.unresolved.len();
    Ok(ImportReport {
        schema_version: 1,
        revision: applied.revision,
        updated_at: applied.updated_at.clone(),
        undo_depth: applied.undo_depth,
        manifest: ManifestSummary {
            path: manifest.path.display().to_string(),
            schema: manifest::SCHEMA,
            producer: manifest::PRODUCER,
            created_at: manifest.created_at.clone(),
            handoff: manifest.handoff.clone(),
            job: manifest.job.clone(),
            project: manifest.project.clone(),
            audio: manifest.audio.clone(),
            warnings: manifest.warnings.clone(),
        },
        project: ProjectInfo {
            project_id: project.id.clone(),
            title: project.title.clone(),
            fps: project.fps,
            resolution: project.resolution,
            settings_changed: plan.settings_changed,
        },
        media,
        tracks,
        clips,
        captions: plan.captions,
        created,
        warnings: plan.warnings,
        unresolved: manifest.unresolved.clone(),
        validation: ValidationSummary {
            ok: validation.ok,
            error_count: validation.errors.len(),
            warning_count: validation.warnings.len(),
            unresolved_count,
            import_complete: validation.ok && unresolved_count == 0,
        },
        timeline: avs_core::query::inspect(project),
    })
}
