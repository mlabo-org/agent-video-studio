//! The session file: one project, its undo history and its activity log.
//!
//! Every change goes through [`Session::apply`], [`Session::apply_many`],
//! [`Session::undo`] or [`Session::redo`]. Each takes the file lock, reads the
//! file, checks the caller's revision, applies the edit through `avs_core::apply`
//! and writes the result with a temp file plus rename. There is no way to store a
//! project that did not come out of `avs_core::apply`.

use crate::env::{Env, SESSION_FILE_NAME};
use crate::error::{Result, SessionError};
use crate::fsutil::{DEFAULT_LOCK_TIMEOUT, Lock, atomic_write, now_iso, read_optional};
use avs_core::{Operation, Outcome, Project};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const SESSION_SCHEMA_VERSION: u32 = 2;
/// Undo steps kept (project snapshots).
pub const HISTORY_LIMIT: usize = 30;
/// Activity entries kept; older ones are dropped first.
pub const ACTIVITY_LIMIT: usize = 200;
const DEFAULT_TITLE: &str = "Untitled Project";

/// One line of the activity log. It records what happened; it is never part of the
/// project and is not duplicated into undo snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEntry {
    /// MCP tool name (`split_clip`, ...), or `undo` / `redo`.
    pub tool: String,
    pub summary: String,
    pub ok: bool,
    /// ISO 8601 UTC time.
    pub at: String,
    /// Revision the entry produced; absent for entries carried over from old files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

/// Something skipped while reading a file that did not stop it from opening.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoadWarning {
    /// `HISTORY_ENTRY_DROPPED`.
    pub code: String,
    pub message: String,
}

/// Cheap description of a session, used in workspace listings and plans.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: String,
    pub title: String,
    pub revision: u64,
    pub updated_at: String,
    pub media_count: usize,
    pub track_count: usize,
    pub clip_count: usize,
}

/// A session as read from disk.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    /// Unique per library; `None` for an old (v1) file until its first write.
    pub library_id: Option<String>,
    pub revision: u64,
    pub updated_at: String,
    pub project: Project,
    pub undo_depth: usize,
    pub redo_depth: usize,
    pub activity: Vec<ActivityEntry>,
    pub warnings: Vec<LoadWarning>,
    /// Schema version of the file that was read (1 means an old file, rewritten as 2 on the next change).
    pub file_schema_version: u32,
}

impl SessionState {
    pub fn summary(&self) -> ProjectSummary {
        summarize(&self.project, self.revision, &self.updated_at)
    }
}

/// Result of a write: the new revision and project, plus what each operation did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    /// `false` only for `undo` / `redo` with nothing to step through (nothing is written).
    pub changed: bool,
    pub revision: u64,
    pub updated_at: String,
    pub project: Project,
    /// One entry per operation, in order (empty for undo/redo).
    pub outcomes: Vec<Outcome>,
    pub undo_depth: usize,
    pub redo_depth: usize,
    pub warnings: Vec<LoadWarning>,
}

/// Where a library's files are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryPaths {
    pub data_directory: PathBuf,
    pub session_file: PathBuf,
}

impl LibraryPaths {
    /// The standard layout: `<dataDirectory>/project-session.json`.
    pub fn in_directory(data_directory: impl Into<PathBuf>) -> Self {
        let data_directory = data_directory.into();
        let session_file = data_directory.join(SESSION_FILE_NAME);
        LibraryPaths { data_directory, session_file }
    }

    /// `<dataDirectory>/.project-session.lock` (beside the session file in single-file mode).
    pub fn lock_file(&self) -> PathBuf {
        let stem = self.session_file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "session".into());
        self.session_file.with_file_name(format!(".{stem}.lock"))
    }

    pub fn media_root(&self) -> PathBuf {
        self.data_directory.join("media")
    }

    pub fn export_jobs_directory(&self) -> PathBuf {
        self.data_directory.join(".export-jobs")
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    paths: LibraryPaths,
    lock_timeout: Duration,
}

impl Session {
    pub fn new(paths: LibraryPaths) -> Self {
        Session { paths, lock_timeout: DEFAULT_LOCK_TIMEOUT }
    }

    /// A session in the standard layout under `data_directory`.
    pub fn in_directory(data_directory: impl Into<PathBuf>) -> Self {
        Session::new(LibraryPaths::in_directory(data_directory))
    }

    /// The session the current settings and environment overrides point at. Reads
    /// settings only; creates nothing.
    pub fn for_env(env: &Env) -> Result<Self> {
        let config = crate::config::resolve_library(env)?;
        let mut paths = LibraryPaths::in_directory(config.data_directory);
        if let Some(session_path) = &env.session_path {
            paths.session_file = session_path.clone();
        }
        Ok(Session::new(paths))
    }

    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    pub fn paths(&self) -> &LibraryPaths {
        &self.paths
    }

    pub fn data_directory(&self) -> &Path {
        &self.paths.data_directory
    }

    /// Reads the session. Never writes or creates anything, and takes no lock
    /// (writers replace the file atomically). A missing file is `SESSION_NOT_FOUND`.
    pub fn load(&self) -> Result<SessionState> {
        match read_stored(&self.paths.session_file)? {
            Some(stored) => Ok(stored.into_state()),
            None => Err(not_found(&self.paths)),
        }
    }

    pub fn summary(&self) -> Result<ProjectSummary> {
        Ok(self.load()?.summary())
    }

    /// Creates the session with a new empty project (new unique project id).
    /// Fails with `SESSION_EXISTS` when a file is already there.
    pub fn create(&self, title: Option<&str>) -> Result<SessionState> {
        let _lock = self.lock()?;
        if crate::fsutil::path_exists(&self.paths.session_file) {
            return Err(SessionError::conflict("SESSION_EXISTS", format!("{} already exists.", self.paths.session_file.display()))
                .with("sessionFile", self.paths.session_file.display().to_string()));
        }
        let stored = Stored::fresh(title);
        write_stored(&self.paths.session_file, &stored)?;
        Ok(stored.into_state())
    }

    /// Loads the session, creating it first when the file does not exist.
    pub fn ensure(&self, title: Option<&str>) -> Result<SessionState> {
        if let Some(stored) = read_stored(&self.paths.session_file)? {
            return Ok(stored.into_state());
        }
        let _lock = self.lock()?;
        if let Some(stored) = read_stored(&self.paths.session_file)? {
            return Ok(stored.into_state());
        }
        let stored = Stored::fresh(title);
        write_stored(&self.paths.session_file, &stored)?;
        Ok(stored.into_state())
    }

    /// Applies one operation. With `expected_revision` set, fails with
    /// `REVISION_MISMATCH` unless the stored revision is exactly that.
    pub fn apply(&self, operation: &Operation, expected_revision: Option<u64>) -> Result<Applied> {
        self.apply_many(std::slice::from_ref(operation), expected_revision)
    }

    /// Applies several operations as one revision and one undo step. If any fails,
    /// nothing is stored and the error carries `operationIndex`.
    pub fn apply_many(&self, operations: &[Operation], expected_revision: Option<u64>) -> Result<Applied> {
        if operations.is_empty() {
            return Err(SessionError::invalid("NO_OPERATIONS", "apply_many needs at least one operation."));
        }
        self.write(expected_revision, |stored, next_revision| {
            let before = stored.project.clone();
            let mut project = before.clone();
            let mut outcomes = Vec::with_capacity(operations.len());
            let at = now_iso();
            let mut entries = Vec::with_capacity(operations.len());
            for (index, operation) in operations.iter().enumerate() {
                match avs_core::apply(&project, operation) {
                    Ok((next, outcome)) => {
                        entries.push(ActivityEntry {
                            tool: operation.tool_name(),
                            summary: outcome.summary.clone(),
                            ok: true,
                            at: at.clone(),
                            revision: Some(next_revision),
                        });
                        outcomes.push(outcome);
                        project = next;
                    }
                    Err(error) => {
                        return Err(SessionError::from(error).with("operationIndex", index).with("operationCount", operations.len()));
                    }
                }
            }
            stored.past.push(before);
            stored.future.clear();
            stored.project = project;
            stored.activity.extend(entries);
            Ok((true, outcomes))
        })
    }

    /// Steps back one snapshot. Writes nothing when there is nothing to undo.
    pub fn undo(&self, expected_revision: Option<u64>) -> Result<Applied> {
        self.step_history(expected_revision, "undo")
    }

    /// Steps forward one snapshot. Writes nothing when there is nothing to redo.
    pub fn redo(&self, expected_revision: Option<u64>) -> Result<Applied> {
        self.step_history(expected_revision, "redo")
    }

    fn step_history(&self, expected_revision: Option<u64>, direction: &'static str) -> Result<Applied> {
        self.write(expected_revision, |stored, next_revision| {
            let target = if direction == "undo" { stored.past.pop() } else { stored.future.pop() };
            let Some(target) = target else { return Ok((false, Vec::new())) };
            let current = std::mem::replace(&mut stored.project, target);
            let (other, label) = if direction == "undo" { (&mut stored.future, "Undid") } else { (&mut stored.past, "Redid") };
            other.push(current);
            let at = now_iso();
            let kept = other.len().saturating_sub(HISTORY_LIMIT);
            other.drain(..kept);
            stored.activity.push(ActivityEntry {
                tool: direction.to_string(),
                summary: format!("{label} the last change."),
                ok: true,
                at,
                revision: Some(next_revision),
            });
            Ok((true, Vec::new()))
        })
    }

    pub(crate) fn lock(&self) -> Result<Lock> {
        Lock::acquire(&self.paths.lock_file(), self.lock_timeout)
    }

    /// The single write path: lock, read, check revision, run `change`, bump the
    /// revision, trim, write. `change` returns whether it changed anything.
    fn write<F>(&self, expected_revision: Option<u64>, change: F) -> Result<Applied>
    where
        F: FnOnce(&mut Stored, u64) -> Result<(bool, Vec<Outcome>)>,
    {
        // Check before locking: taking the lock would create the folder and lock file.
        if !crate::fsutil::path_exists(&self.paths.session_file) {
            return Err(not_found(&self.paths));
        }
        let _lock = self.lock()?;
        let mut stored = read_stored(&self.paths.session_file)?.ok_or_else(|| not_found(&self.paths))?;
        if let Some(expected) = expected_revision
            && expected != stored.revision
        {
            return Err(SessionError::revision_mismatch("REVISION_MISMATCH", "Project session", expected, stored.revision));
        }
        let next_revision = stored.revision + 1;
        let (changed, outcomes) = change(&mut stored, next_revision)?;
        let warnings = std::mem::take(&mut stored.warnings);
        if changed {
            stored.revision = next_revision;
            stored.updated_at = now_iso();
            stored.library_id.get_or_insert_with(new_library_id);
            stored.trim();
            write_stored(&self.paths.session_file, &stored)?;
        }
        Ok(Applied {
            changed,
            revision: stored.revision,
            updated_at: stored.updated_at,
            undo_depth: stored.past.len(),
            redo_depth: stored.future.len(),
            project: stored.project,
            outcomes,
            warnings,
        })
    }
}

fn not_found(paths: &LibraryPaths) -> SessionError {
    SessionError::not_found("SESSION_NOT_FOUND", format!("No project session at {}.", paths.session_file.display()))
        .with("sessionFile", paths.session_file.display().to_string())
}

fn new_library_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub(crate) fn new_project_id() -> String {
    format!("project-{}", uuid::Uuid::new_v4().simple())
}

pub(crate) fn summarize(project: &Project, revision: u64, updated_at: &str) -> ProjectSummary {
    ProjectSummary {
        id: project.id.clone(),
        title: project.title.clone(),
        revision,
        updated_at: updated_at.to_string(),
        media_count: project.media.len(),
        track_count: project.tracks.len(),
        clip_count: project.tracks.iter().map(|track| track.clips.len()).sum(),
    }
}

/// Reads only the summary of a session file (for workspace listings).
pub fn read_summary(session_file: &Path) -> Result<Option<ProjectSummary>> {
    Ok(read_stored(session_file)?.map(|stored| stored.into_state().summary()))
}

// ---------------------------------------------------------------- stored form

#[derive(Debug)]
struct Stored {
    library_id: Option<String>,
    revision: u64,
    updated_at: String,
    project: Project,
    past: Vec<Project>,
    future: Vec<Project>,
    activity: Vec<ActivityEntry>,
    warnings: Vec<LoadWarning>,
    file_schema_version: u32,
}

impl Stored {
    fn fresh(title: Option<&str>) -> Stored {
        let title = title.map(str::trim).filter(|t| !t.is_empty()).unwrap_or(DEFAULT_TITLE);
        Stored {
            library_id: Some(new_library_id()),
            revision: 1,
            updated_at: now_iso(),
            project: Project::empty(new_project_id(), title),
            past: Vec::new(),
            future: Vec::new(),
            activity: Vec::new(),
            warnings: Vec::new(),
            file_schema_version: SESSION_SCHEMA_VERSION,
        }
    }

    fn trim(&mut self) {
        let extra = self.past.len().saturating_sub(HISTORY_LIMIT);
        self.past.drain(..extra);
        let extra = self.future.len().saturating_sub(HISTORY_LIMIT);
        self.future.drain(..extra);
        let extra = self.activity.len().saturating_sub(ACTIVITY_LIMIT);
        self.activity.drain(..extra);
    }

    fn into_state(self) -> SessionState {
        SessionState {
            library_id: self.library_id,
            revision: self.revision,
            updated_at: self.updated_at,
            undo_depth: self.past.len(),
            redo_depth: self.future.len(),
            project: self.project,
            activity: self.activity,
            warnings: self.warnings,
            file_schema_version: self.file_schema_version,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvelopeOut<'a> {
    schema_version: u32,
    library_id: &'a str,
    revision: u64,
    updated_at: &'a str,
    project: &'a Project,
    history: HistoryOut<'a>,
    activity: &'a [ActivityEntry],
}

#[derive(Serialize)]
struct HistoryOut<'a> {
    past: &'a [Project],
    future: &'a [Project],
}

fn write_stored(path: &Path, stored: &Stored) -> Result<()> {
    let library_id = stored.library_id.as_deref().unwrap_or_default();
    let envelope = EnvelopeOut {
        schema_version: SESSION_SCHEMA_VERSION,
        library_id,
        revision: stored.revision,
        updated_at: &stored.updated_at,
        project: &stored.project,
        history: HistoryOut { past: &stored.past, future: &stored.future },
        activity: &stored.activity,
    };
    let mut bytes = serde_json::to_vec(&envelope).map_err(|e| SessionError::invalid("SERIALIZE_FAILED", e.to_string()))?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

fn read_stored(path: &Path) -> Result<Option<Stored>> {
    let Some(bytes) = read_optional(path)? else { return Ok(None) };
    parse_stored(&bytes, path).map(Some)
}

/// Checks the invariants of a project read from disk; the same rules every edit enforces.
fn check_project(project: &Project) -> std::result::Result<(), Vec<avs_core::Issue>> {
    let validation = avs_core::validate(project);
    if validation.ok { Ok(()) } else { Err(validation.errors) }
}

fn read_project(value: Value) -> std::result::Result<Project, String> {
    let mut project: Project = serde_json::from_value(value).map_err(|e| e.to_string())?;
    project.adopt_legacy_fields();
    check_project(&project).map_err(|issues| {
        issues.iter().take(3).map(|i| format!("{}: {}", i.code, i.message)).collect::<Vec<_>>().join("; ")
    })?;
    Ok(project)
}

fn parse_stored(bytes: &[u8], path: &Path) -> Result<Stored> {
    let invalid = |code: &str, message: String| SessionError::invalid(code, message).with("sessionFile", path.display().to_string());
    let root: Value = serde_json::from_slice(bytes).map_err(|e| invalid("SESSION_INVALID_JSON", format!("{} is not valid JSON: {e}", path.display())))?;
    let Value::Object(mut root) = root else {
        return Err(invalid("SESSION_INVALID", "Project session JSON must be an object.".into()));
    };
    let version = root.get("schemaVersion").and_then(Value::as_u64).unwrap_or(0);
    if version != 1 && version != 2 {
        return Err(invalid("SESSION_UNSUPPORTED_SCHEMA", format!("Unsupported project session schemaVersion: {}.", root.get("schemaVersion").unwrap_or(&Value::Null)))
            .with("schemaVersion", root.get("schemaVersion").cloned().unwrap_or(Value::Null)));
    }
    let revision = root
        .get("revision")
        .and_then(Value::as_u64)
        .filter(|r| *r >= 1)
        .ok_or_else(|| invalid("SESSION_INVALID", "Project session revision must be a positive integer.".into()))?;
    let updated_at = root.get("updatedAt").and_then(Value::as_str).unwrap_or_default().to_string();

    let project_value = root.remove("project").ok_or_else(|| invalid("SESSION_INVALID", "Project session has no project.".into()))?;
    let mut project: Project = serde_json::from_value(project_value)
        .map_err(|e| invalid("SESSION_PROJECT_INVALID", format!("The current project could not be read: {e}")))?;
    project.adopt_legacy_fields();
    if let Err(issues) = check_project(&project) {
        return Err(invalid("SESSION_PROJECT_INVALID", format!("The current project is invalid: {} issue(s), first: {}.", issues.len(), issues[0].message))
            .with("issues", issues));
    }

    // Old files keep their activity inside the current project; history snapshots are ignored.
    let mut activity: Vec<ActivityEntry> = if version == 1 {
        project
            .legacy_tool_history
            .iter()
            .map(|entry| ActivityEntry {
                tool: entry.get("toolName").and_then(Value::as_str).unwrap_or("unknown").to_string(),
                summary: entry.get("summary").and_then(Value::as_str).unwrap_or_default().to_string(),
                ok: entry.get("status").and_then(Value::as_str) != Some("error"),
                at: entry.get("createdAt").and_then(Value::as_str).unwrap_or(&updated_at).to_string(),
                revision: None,
            })
            .collect()
    } else {
        match root.remove("activity") {
            Some(Value::Array(items)) => items.into_iter().filter_map(|item| serde_json::from_value(item).ok()).collect(),
            _ => Vec::new(),
        }
    };
    project.legacy_tool_history.clear();
    let extra = activity.len().saturating_sub(ACTIVITY_LIMIT);
    activity.drain(..extra);

    let mut warnings = Vec::new();
    let mut history = match root.remove("history") {
        Some(Value::Object(map)) => map,
        _ => Default::default(),
    };
    let mut read_list = |name: &str, warnings: &mut Vec<LoadWarning>| -> Vec<Project> {
        let Some(Value::Array(items)) = history.remove(name) else { return Vec::new() };
        let mut kept = Vec::new();
        for (index, item) in items.into_iter().enumerate() {
            match read_project(item) {
                Ok(snapshot) => kept.push(snapshot),
                Err(reason) => warnings.push(LoadWarning {
                    code: "HISTORY_ENTRY_DROPPED".into(),
                    message: format!("Dropped history.{name}[{index}]: {reason}"),
                }),
            }
        }
        kept
    };
    let mut past = read_list("past", &mut warnings);
    let mut future = read_list("future", &mut warnings);
    for list in [&mut past, &mut future] {
        let extra = list.len().saturating_sub(HISTORY_LIMIT);
        list.drain(..extra);
    }

    let library_id = if version == 2 { root.get("libraryId").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string) } else { None };
    Ok(Stored {
        library_id,
        revision,
        updated_at,
        project,
        past,
        future,
        activity,
        warnings,
        file_schema_version: version as u32,
    })
}

