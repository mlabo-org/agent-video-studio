//! Agent Video Studio storage layer: the project session file, settings, workspaces,
//! media import and export job state. Synchronous; no async runtime.
//!
//! The MCP server and the editor UI use this crate for everything that touches disk.
//! Edits to a project exist in exactly one form, `avs_core::Operation`, and reach the
//! disk only through [`Session::apply`] / [`Session::apply_many`] / [`Session::undo`] /
//! [`Session::redo`]. All types serialize to JSON (camelCase) for tool output.
//!
//! Nothing here reads the process environment except [`Env::from_process`]; every
//! location is a field of [`Env`], so tests run entirely inside a temp directory.
//!
//! # Public API
//!
//! ## [`env`]
//! - `Env::from_process() -> Env`, `Env::new(home) -> Env`
//! - `Env::{settings_file, recent_file, default_data_directory, normalize_data_directory, app_owned_data_directory}`
//!
//! ## [`session`] (the project file; revisions, undo, activity log)
//! - `Session::for_env(&Env) -> Result<Session>`, `Session::in_directory(dir) -> Session`
//! - `Session::load(&self) -> Result<SessionState>` (read only; never creates)
//! - `Session::summary(&self) -> Result<ProjectSummary>`
//! - `Session::create(&self, title: Option<&str>) -> Result<SessionState>`; `Session::ensure(title)` creates if missing
//! - `Session::apply(&self, &Operation, expected_revision: Option<u64>) -> Result<Applied>`
//! - `Session::apply_many(&self, &[Operation], expected_revision) -> Result<Applied>` (one revision, one undo step, all or nothing)
//! - `Session::undo(&self, expected_revision) -> Result<Applied>`, `Session::redo(..)`
//! - `Session::resolve_media_source(&self, source: &str) -> Result<PathBuf>`
//! - `read_summary(session_file) -> Result<Option<ProjectSummary>>`
//!
//! ## [`config`] (which library is current; recent list)
//! - `get_settings(&Env) -> Result<LibrarySettings>`, `resolve_library(&Env) -> Result<ResolvedLibrary>`
//! - `set_settings(&Env, data_directory: &str, expected_revision) -> Result<LibrarySettings>`
//! - `reset_settings(&Env, expected_revision) -> Result<LibrarySettings>` (works on a broken file)
//! - `recent_list(&Env) -> Result<RecentList>`, `recent_forget(&Env, dir: &str, expected_recent_revision) -> Result<ForgetResult>`
//!
//! ## [`workspace`] (whole libraries on disk)
//! - `io_status(&Env) -> Result<IoStatus>`, `inspect_target(&Env, path: &str) -> Inspection`
//! - `create_workspace(&Env, &CreateInput) -> Result<CreateResult>`, `switch_workspace(&Env, &SwitchInput) -> Result<SwitchResult>`
//! - `inspect_delete(&Env, path) -> WorkspacePlan`, `delete_workspace(&Env, &ConfirmedInput) -> Result<DeleteResult>` (renames to `.deleted`)
//! - `inspect_restore(&Env, path) -> WorkspacePlan`, `restore_workspace(&Env, &ConfirmedInput) -> Result<RestoreResult>`
//!
//! ## [`media`]
//! - `import_media(&Env, &Session, &ImportRequest, expected_revision) -> Result<ImportedMedia>` (copy, probe, `RegisterMedia`; copy removed on failure)
//! - `prepare_import(..) -> Result<PreparedImport>` + `discard_import(&PreparedImport)` for callers that register through `apply_many`
//! - `probe_media(&Env, file, timeout) -> Result<MediaProbe, Vec<ProbeAttempt>>`
//! - `resolve_media_source(media_root, source) -> Result<PathBuf>`
//!
//! ## [`browse`]
//! - `browse_directories(&Env, requested: Option<&str>, BrowseMode) -> Result<DirectoryListing>`
//!
//! ## [`jobs`] (export job state files, shared between processes)
//! - `ExportJobs::for_library(&LibraryPaths)`; `create`, `get`, `list`, `mark_running`, `report_progress`,
//!   `request_cancel`, `finish(&JobResult)`, `reap_stale`
//! - `pid_is_alive(pid) -> Option<bool>`
//!
//! ## [`error`]
//! - `SessionError { kind, code, message, details }`; codes are stable strings (`REVISION_MISMATCH`,
//!   `LOCK_TIMEOUT`, `SESSION_NOT_FOUND`, core codes such as `CLIP_OVERLAP`, ...).

#![deny(unsafe_code)]

pub mod browse;
pub mod config;
pub mod env;
pub mod error;
pub mod fsutil;
pub mod jobs;
pub mod media;
pub mod session;
pub mod workspace;

pub use browse::{BrowseMode, DirectoryListing, browse_directories};
pub use config::{
    ForgetResult, LibrarySettings, RecentList, ResolvedLibrary, get_settings, recent_forget, recent_list, reset_settings, resolve_library, set_settings,
};
pub use env::Env;
pub use error::{ErrorKind, Result, SessionError};
pub use jobs::{ExportJob, ExportJobs, JobResult, JobState, JobStatus, NewExportJob, pid_is_alive};
pub use media::{ImportRequest, ImportedMedia, MediaProbe, PreparedImport, discard_import, import_media, prepare_import, probe_media, resolve_media_source};
pub use session::{ACTIVITY_LIMIT, ActivityEntry, Applied, HISTORY_LIMIT, LibraryPaths, ProjectSummary, Session, SessionState, read_summary};
pub use workspace::{
    ConfirmedInput, CreateInput, SwitchInput, WorkspacePlan, create_workspace, delete_workspace, inspect_delete, inspect_restore, inspect_target,
    io_status, restore_workspace, switch_workspace,
};
