//! Workspaces: whole project libraries on disk. Create, switch, delete (reversible:
//! the folder is renamed to `<name>.deleted`), restore, and the read-only checks
//! that say what each of those would do.
//!
//! Every mutating call takes the config lock first, re-runs the same checks the
//! matching `inspect_*` call reports, and orders its steps so that a failure in a
//! later step puts the earlier one back.

use crate::config::{
    LibrarySettings, RecentList, check_recent_revision, check_settings_revision, config_lock, get_settings,
    recent_forget_locked, recent_list, recent_revision, recent_upsert_locked, resolve_library, write_settings_locked,
};
use crate::env::{ENV_LIBRARY_ROOT, ENV_SESSION_PATH, Env, SESSION_FILE_NAME, is_app_owned};
use crate::error::{ErrorKind, Result, SessionError};
use crate::fsutil::{is_symlink, looks_writable, path_exists};
use crate::session::{ProjectSummary, Session, read_summary};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const DELETED_SUFFIX: &str = ".deleted";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Blocker {
    /// `PROJECT_IO_*` (the codes the earlier implementation used).
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanWarning {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathState {
    pub path: PathBuf,
    pub exists: bool,
    pub is_directory: bool,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetState {
    pub input: String,
    pub path: Option<PathBuf>,
    pub exists: bool,
    pub is_directory: bool,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub target: TargetState,
    pub data_directory: Option<PathState>,
    pub session_file_name: &'static str,
    pub session_exists: bool,
    pub existing_project: Option<ProjectSummary>,
    pub blockers: Vec<Blocker>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequiredConfirmation {
    /// The caller must send exactly this path back as `confirmDataDirectory`.
    pub confirm_data_directory: Option<PathBuf>,
}

/// What deleting or restoring a workspace would do and what stops it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePlan {
    pub target: TargetState,
    pub data_directory: Option<PathState>,
    pub parent_workspace_path: Option<PathBuf>,
    pub deleted_data_directory_path: Option<PathBuf>,
    pub session_file_name: &'static str,
    pub session_exists: bool,
    /// Delete: this is the workspace in use. Restore: the destination is the workspace in use.
    pub current: bool,
    pub summary: Option<ProjectSummary>,
    pub blockers: Vec<Blocker>,
    pub warnings: Vec<PlanWarning>,
    pub required_confirmation: RequiredConfirmation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateInput {
    pub target_path: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub expected_settings_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateResult {
    pub created: ProjectSummary,
    pub inspection: Inspection,
    pub settings: LibrarySettings,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchInput {
    pub target_path: String,
    #[serde(default)]
    pub expected_settings_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchResult {
    pub opened: ProjectSummary,
    pub inspection: Inspection,
    pub settings: LibrarySettings,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmedInput {
    pub target_path: String,
    /// Must equal `plan.requiredConfirmation.confirmDataDirectory`.
    pub confirm_data_directory: String,
    #[serde(default)]
    pub expected_recent_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    pub deleted_data_directory: PathBuf,
    pub moved_to: PathBuf,
    pub summary: ProjectSummary,
    pub plan: WorkspacePlan,
    pub recent: RecentList,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult {
    pub restored_data_directory: PathBuf,
    pub restored_from: PathBuf,
    pub summary: ProjectSummary,
    pub plan: WorkspacePlan,
    pub recent: RecentList,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverrideStatus {
    pub env: &'static str,
    pub active: bool,
    pub affected_operations: Vec<&'static str>,
    pub next_step: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IoStatus {
    pub session_path_active: bool,
    pub library_root_active: bool,
    pub environment_overrides: Vec<OverrideStatus>,
    pub settings: LibrarySettings,
    pub current_project: Inspection,
}

// ---------------------------------------------------------------- inspection

fn blocker(code: &str, message: impl Into<String>) -> Blocker {
    Blocker { code: code.to_string(), message: message.into() }
}

fn push_unique(blockers: &mut Vec<Blocker>, item: Blocker) {
    if !blockers.iter().any(|b| b.code == item.code) {
        blockers.push(item);
    }
}

fn directory_state(path: &Path) -> PathState {
    match fs::metadata(path) {
        Ok(meta) => PathState { path: path.to_path_buf(), exists: true, is_directory: meta.is_dir(), writable: meta.is_dir() && looks_writable(path) },
        Err(_) => PathState { path: path.to_path_buf(), exists: false, is_directory: false, writable: false },
    }
}

fn target_state(input: &str, path: &Path) -> TargetState {
    let state = directory_state(path);
    TargetState { input: input.to_string(), path: Some(path.to_path_buf()), exists: state.exists, is_directory: state.is_directory, writable: state.writable }
}

fn file_state(path: &Path) -> (bool, bool) {
    match fs::metadata(path) {
        Ok(meta) => (true, meta.is_file()),
        Err(_) => (false, false),
    }
}

fn blocked_error(blockers: &[Blocker]) -> SessionError {
    let first = &blockers[0];
    SessionError::new(ErrorKind::Blocked, first.code.clone(), first.message.clone()).with("blockers", blockers)
}

fn throw_first(blockers: &[Blocker]) -> Result<()> {
    if blockers.is_empty() { Ok(()) } else { Err(blocked_error(blockers)) }
}

fn resolved_current(env: &Env) -> Option<PathBuf> {
    resolve_library(env).ok().map(|c| c.data_directory)
}

fn deleted_path(data_directory: &Path) -> PathBuf {
    let mut name = data_directory.as_os_str().to_os_string();
    name.push(DELETED_SUFFIX);
    PathBuf::from(name)
}

/// Checks a target folder: where its data directory would be, whether a session is
/// there and readable, and what would block using it. Reads only.
pub fn inspect_target(env: &Env, target_path: &str) -> Inspection {
    let input = target_path.trim();
    let mut blockers = Vec::new();
    let data_directory = match env.app_owned_data_directory(input) {
        Ok(path) => path,
        Err(error) => {
            blockers.push(blocker("PROJECT_IO_TARGET_INVALID", error.message));
            return Inspection {
                target: TargetState { input: input.to_string(), path: None, exists: false, is_directory: false, writable: false },
                data_directory: None,
                session_file_name: SESSION_FILE_NAME,
                session_exists: false,
                existing_project: None,
                blockers,
            };
        }
    };
    let target = target_state(input, &env.resolve_selected_path(input));
    let data_state = directory_state(&data_directory);
    let session_file = data_directory.join(SESSION_FILE_NAME);
    let (session_exists, session_is_file) = file_state(&session_file);
    let mut existing_project = None;

    if target.exists && !target.is_directory {
        blockers.push(blocker("PROJECT_IO_TARGET_NOT_DIRECTORY", "Project I/O target path exists but is not a directory."));
    }
    if data_state.exists && !data_state.is_directory {
        blockers.push(blocker("PROJECT_IO_DATA_DIRECTORY_NOT_DIRECTORY", "Resolved project library data directory exists but is not a directory."));
    }
    if data_state.exists && data_state.is_directory && !data_state.writable {
        blockers.push(blocker("PROJECT_IO_DATA_DIRECTORY_NOT_WRITABLE", "Resolved project library data directory is not writable."));
    }
    if !data_state.exists && !looks_writable(&data_directory) {
        blockers.push(blocker("PROJECT_IO_DATA_DIRECTORY_PARENT_NOT_WRITABLE", "Resolved project library data directory cannot be created from a writable parent directory."));
    }
    if session_exists && !session_is_file {
        blockers.push(blocker("PROJECT_IO_SESSION_NOT_FILE", format!("{SESSION_FILE_NAME} exists but is not a file.")));
    }
    if session_exists && session_is_file {
        match read_summary(&session_file) {
            Ok(summary) => existing_project = summary,
            Err(error) => blockers.push(blocker("PROJECT_IO_SESSION_INVALID", error.message)),
        }
    }
    Inspection {
        target,
        data_directory: Some(data_state),
        session_file_name: SESSION_FILE_NAME,
        session_exists: session_exists && session_is_file,
        existing_project,
        blockers,
    }
}

/// What deleting `target_path`'s workspace would do. Reads only.
pub fn inspect_delete(env: &Env, target_path: &str) -> WorkspacePlan {
    let inspection = inspect_target(env, target_path);
    let mut blockers = inspection.blockers.clone();
    let mut warnings = Vec::new();
    let data_path = inspection.data_directory.as_ref().map(|d| d.path.clone());
    let current_directory = resolved_current(env);
    let current = matches!((&current_directory, &data_path), (Some(c), Some(d)) if c == d);
    let deleted = data_path.as_deref().map(deleted_path);

    if current_directory.is_none() {
        warnings.push(PlanWarning { code: "PROJECT_IO_CURRENT_WORKSPACE_UNRESOLVED".into(), message: "Current project workspace could not be resolved from project library settings.".into() });
    }
    if env.library_root_active() {
        blockers.push(blocker("PROJECT_IO_LIBRARY_ROOT_ENV_ACTIVE", format!("Project delete cannot run while {ENV_LIBRARY_ROOT} is active.")));
    }
    if !inspection.target.exists {
        blockers.push(blocker("PROJECT_IO_TARGET_MISSING", "Project delete target does not exist."));
    }
    if inspection.target.path.as_deref().is_some_and(is_symlink) {
        blockers.push(blocker("PROJECT_IO_TARGET_SYMLINK", "Project delete target is a symbolic link and is not safe to rename."));
    }
    if let Some(data) = &inspection.data_directory {
        if !is_app_owned(&data.path) {
            blockers.push(blocker("PROJECT_IO_DATA_DIRECTORY_NOT_APP_OWNED", "Project delete is limited to app-owned project library data directories."));
        }
        if !data.exists {
            blockers.push(blocker("PROJECT_IO_DATA_DIRECTORY_MISSING", "Project delete data directory does not exist."));
        }
        if data.exists && is_symlink(&data.path) {
            blockers.push(blocker("PROJECT_IO_DATA_DIRECTORY_SYMLINK", "Project delete data directory is a symbolic link and is not safe to rename."));
        }
        if data.exists && data.path.parent().is_some_and(|p| !looks_writable(p)) {
            blockers.push(blocker("PROJECT_IO_DATA_DIRECTORY_PARENT_NOT_WRITABLE", "Project delete data directory parent is not writable."));
        }
    }
    if !inspection.session_exists {
        blockers.push(blocker("PROJECT_IO_SESSION_MISSING", format!("Cannot delete project workspace because {SESSION_FILE_NAME} does not exist.")));
    }
    if current {
        blockers.push(blocker("PROJECT_IO_DELETE_CURRENT_WORKSPACE", "Cannot delete the currently active project workspace."));
    }
    if deleted.as_deref().is_some_and(path_exists) {
        blockers.push(blocker("PROJECT_IO_DELETE_DESTINATION_EXISTS", "Project delete destination already exists."));
    }
    if let (Some(target), Some(data)) = (&inspection.target.path, &data_path)
        && target != data
    {
        warnings.push(PlanWarning {
            code: "PROJECT_IO_DELETE_RESOLVES_APP_OWNED_DATA_DIRECTORY".into(),
            message: "Delete will rename the resolved app-owned project library data directory, not the selected parent directory.".into(),
        });
    }
    let mut deduped = Vec::new();
    for item in blockers {
        push_unique(&mut deduped, item);
    }
    WorkspacePlan {
        target: inspection.target,
        parent_workspace_path: parent_workspace(data_path.as_deref()),
        deleted_data_directory_path: deleted,
        session_file_name: SESSION_FILE_NAME,
        session_exists: inspection.session_exists,
        current,
        summary: inspection.existing_project,
        blockers: deduped,
        warnings,
        required_confirmation: RequiredConfirmation { confirm_data_directory: data_path },
        data_directory: inspection.data_directory,
    }
}

/// What restoring a deleted workspace would do. `target_path` is the original
/// workspace location, not the `.deleted` folder. Reads only.
pub fn inspect_restore(env: &Env, target_path: &str) -> WorkspacePlan {
    let input = target_path.trim();
    let mut blockers = Vec::new();
    let selected = env.resolve_selected_path(input);
    let aborted = |input: &str, selected: Option<PathBuf>, blockers: Vec<Blocker>| WorkspacePlan {
        target: TargetState { input: input.to_string(), path: selected, exists: false, is_directory: false, writable: false },
        data_directory: None,
        parent_workspace_path: None,
        deleted_data_directory_path: None,
        session_file_name: SESSION_FILE_NAME,
        session_exists: false,
        current: false,
        summary: None,
        blockers,
        warnings: Vec::new(),
        required_confirmation: RequiredConfirmation { confirm_data_directory: None },
    };
    if selected.to_string_lossy().ends_with(DELETED_SUFFIX) {
        blockers.push(blocker("PROJECT_IO_RESTORE_TARGET_DELETED_PATH", "Project restore target must be the original workspace path, not the .deleted source path."));
        return aborted(input, Some(selected), blockers);
    }
    let data_directory = match env.app_owned_data_directory(input) {
        Ok(path) => path,
        Err(error) => {
            blockers.push(blocker("PROJECT_IO_TARGET_INVALID", error.message));
            return aborted(input, Some(selected), blockers);
        }
    };
    let target = target_state(input, &selected);
    let data_state = directory_state(&data_directory);
    let deleted = deleted_path(&data_directory);
    let deleted_state = directory_state(&deleted);
    let session_file = deleted.join(SESSION_FILE_NAME);
    let (session_exists, session_is_file) = file_state(&session_file);
    let current = resolved_current(env).is_some_and(|c| c == data_directory);
    let mut summary = None;

    if env.library_root_active() {
        blockers.push(blocker("PROJECT_IO_LIBRARY_ROOT_ENV_ACTIVE", format!("Project restore cannot run while {ENV_LIBRARY_ROOT} is active.")));
    }
    if !is_app_owned(&data_directory) {
        blockers.push(blocker("PROJECT_IO_DATA_DIRECTORY_NOT_APP_OWNED", "Project restore is limited to app-owned project library data directories."));
    }
    if target.exists && !target.is_directory {
        blockers.push(blocker("PROJECT_IO_TARGET_NOT_DIRECTORY", "Project restore target path exists but is not a directory."));
    }
    if data_state.exists {
        blockers.push(blocker("PROJECT_IO_RESTORE_DESTINATION_EXISTS", "Project restore destination already exists."));
    }
    if current {
        blockers.push(blocker("PROJECT_IO_RESTORE_CURRENT_WORKSPACE_COLLISION", "Cannot restore over the currently configured project workspace."));
    }
    if !deleted_state.exists {
        blockers.push(blocker("PROJECT_IO_RESTORE_SOURCE_MISSING", "Project restore source .deleted directory does not exist."));
    }
    if deleted_state.exists && !deleted_state.is_directory {
        blockers.push(blocker("PROJECT_IO_RESTORE_SOURCE_NOT_DIRECTORY", "Project restore source .deleted path exists but is not a directory."));
    }
    if deleted_state.exists && is_symlink(&deleted) {
        blockers.push(blocker("PROJECT_IO_RESTORE_SOURCE_SYMLINK", "Project restore source .deleted directory is a symbolic link and is not safe to rename."));
    }
    if data_directory.parent().is_some_and(|p| !looks_writable(p)) {
        blockers.push(blocker("PROJECT_IO_DATA_DIRECTORY_PARENT_NOT_WRITABLE", "Project restore data directory parent is not writable."));
    }
    if !session_exists {
        blockers.push(blocker("PROJECT_IO_SESSION_MISSING", format!("Cannot restore project workspace because {SESSION_FILE_NAME} does not exist in the .deleted source.")));
    } else if !session_is_file {
        blockers.push(blocker("PROJECT_IO_SESSION_NOT_FILE", format!("{SESSION_FILE_NAME} exists in the .deleted source but is not a file.")));
    } else {
        match read_summary(&session_file) {
            Ok(found) => summary = found,
            Err(error) => blockers.push(blocker("PROJECT_IO_SESSION_INVALID", error.message)),
        }
    }
    let mut deduped = Vec::new();
    for item in blockers {
        push_unique(&mut deduped, item);
    }
    WorkspacePlan {
        target,
        parent_workspace_path: parent_workspace(Some(&data_directory)),
        deleted_data_directory_path: Some(deleted),
        session_file_name: SESSION_FILE_NAME,
        session_exists: session_exists && session_is_file,
        current,
        summary,
        blockers: deduped,
        warnings: Vec::new(),
        required_confirmation: RequiredConfirmation { confirm_data_directory: Some(data_directory.clone()) },
        data_directory: Some(data_state),
    }
}

fn parent_workspace(data_directory: Option<&Path>) -> Option<PathBuf> {
    let data_directory = data_directory.filter(|d| is_app_owned(d))?;
    data_directory.parent()?.parent().map(Path::to_path_buf)
}

// ---------------------------------------------------------------- mutations

fn ensure_can_switch(env: &Env, operation: &str) -> Result<()> {
    if env.library_root_active() {
        return Err(SessionError::new(
            ErrorKind::Blocked,
            "PROJECT_IO_LIBRARY_ROOT_ENV_ACTIVE",
            format!("{operation} cannot change the active project library while {ENV_LIBRARY_ROOT} is active."),
        )
        .with("env", ENV_LIBRARY_ROOT));
    }
    if env.session_path_active() {
        return Err(SessionError::new(
            ErrorKind::Blocked,
            "PROJECT_IO_LEGACY_SESSION_PATH_ACTIVE",
            format!("{operation} cannot change the active session while {ENV_SESSION_PATH} is active."),
        )
        .with("env", ENV_SESSION_PATH));
    }
    Ok(())
}

fn require_data_directory(inspection: &Inspection) -> Result<PathBuf> {
    inspection
        .data_directory
        .as_ref()
        .map(|d| d.path.clone())
        .ok_or_else(|| SessionError::invalid("PROJECT_IO_TARGET_INVALID", "Project I/O target path is invalid."))
}

/// Creates a new workspace with an empty project (new unique project id) under
/// `target_path` and makes it current.
pub fn create_workspace(env: &Env, input: &CreateInput) -> Result<CreateResult> {
    ensure_can_switch(env, "Project create")?;
    let _lock = config_lock(env)?;
    let inspection = inspect_target(env, &input.target_path);
    throw_first(&inspection.blockers)?;
    if inspection.session_exists {
        return Err(SessionError::conflict("PROJECT_IO_SESSION_EXISTS", format!("{SESSION_FILE_NAME} already exists for the selected project library target.")));
    }
    check_settings_revision(resolve_library(env)?.revision, input.expected_settings_revision)?;
    let directory = require_data_directory(&inspection)?;
    crate::config::assert_writable_directory(&directory)?;
    let session = Session::in_directory(&directory);
    let created = session.create(input.title.as_deref())?;
    let settings = match write_settings_locked(env, directory.clone(), input.expected_settings_revision) {
        Ok(settings) => settings,
        Err(error) => {
            let _ = fs::remove_file(&session.paths().session_file);
            return Err(error);
        }
    };
    let mut warnings = Vec::new();
    if let Err(error) = recent_upsert_locked(env, &directory) {
        warnings.push(format!("The recent workspaces list could not be updated: {}", error.message));
    }
    Ok(CreateResult { created: created.summary(), inspection: inspect_target(env, &directory.display().to_string()), settings, warnings })
}

/// Makes an existing workspace current.
pub fn switch_workspace(env: &Env, input: &SwitchInput) -> Result<SwitchResult> {
    ensure_can_switch(env, "Project open")?;
    let _lock = config_lock(env)?;
    let inspection = inspect_target(env, &input.target_path);
    throw_first(&inspection.blockers)?;
    if !inspection.session_exists {
        return Err(SessionError::not_found(
            "PROJECT_IO_SESSION_MISSING",
            format!("Cannot open project library target because {SESSION_FILE_NAME} does not exist."),
        ));
    }
    check_settings_revision(resolve_library(env)?.revision, input.expected_settings_revision)?;
    let directory = require_data_directory(&inspection)?;
    let opened = Session::in_directory(&directory).load()?.summary();
    let settings = write_settings_locked(env, directory.clone(), input.expected_settings_revision)?;
    let mut warnings = Vec::new();
    if let Err(error) = recent_upsert_locked(env, &directory) {
        warnings.push(format!("The recent workspaces list could not be updated: {}", error.message));
    }
    Ok(SwitchResult { opened, inspection: inspect_target(env, &directory.display().to_string()), settings, warnings })
}

fn check_confirmation(env: &Env, input: &ConfirmedInput, expected: &Path, code: &str, what: &str) -> Result<()> {
    if env.resolve_selected_path(&input.confirm_data_directory) != expected {
        return Err(SessionError::new(ErrorKind::Blocked, code, format!("Project {what} confirmation does not match the resolved data directory."))
            .with("expected", expected.display().to_string())
            .with("actual", input.confirm_data_directory.clone()));
    }
    Ok(())
}

/// Moves a workspace to `<name>.deleted` and removes it from the recent list.
/// Nothing is erased. Refuses the workspace in use, symbolic links and a wrong
/// confirmation path. If the list update fails the folder is moved back.
pub fn delete_workspace(env: &Env, input: &ConfirmedInput) -> Result<DeleteResult> {
    let _lock = config_lock(env)?;
    let plan = inspect_delete(env, &input.target_path);
    throw_first(&plan.blockers)?;
    let (Some(data), Some(moved_to), Some(summary)) = (plan.data_directory.as_ref().map(|d| d.path.clone()), plan.deleted_data_directory_path.clone(), plan.summary.clone()) else {
        return Err(SessionError::invalid("PROJECT_IO_TARGET_INVALID", "Project delete target path is invalid."));
    };
    check_confirmation(env, input, &data, "PROJECT_IO_DELETE_CONFIRMATION_MISMATCH", "delete")?;
    check_recent_revision(recent_revision(env)?, input.expected_recent_revision)?;
    // Wait out a writer that is in the middle of saving this workspace.
    let _session_lock = Session::in_directory(&data).lock()?;
    fs::rename(&data, &moved_to).map_err(|e| SessionError::io("rename to .deleted", &data, &e))?;
    let forgotten = recent_forget_locked(env, &data.display().to_string(), None);
    match forgotten {
        Ok(result) => Ok(DeleteResult { deleted_data_directory: data, moved_to, summary, plan, recent: result.list }),
        Err(error) => {
            let _ = fs::rename(&moved_to, &data);
            Err(error)
        }
    }
}

/// Moves `<name>.deleted` back and puts the workspace on the recent list. It does
/// not make the workspace current. If the list update fails the move is undone.
pub fn restore_workspace(env: &Env, input: &ConfirmedInput) -> Result<RestoreResult> {
    let _lock = config_lock(env)?;
    let plan = inspect_restore(env, &input.target_path);
    throw_first(&plan.blockers)?;
    let (Some(data), Some(source), Some(summary)) = (plan.data_directory.as_ref().map(|d| d.path.clone()), plan.deleted_data_directory_path.clone(), plan.summary.clone()) else {
        return Err(SessionError::invalid("PROJECT_IO_TARGET_INVALID", "Project restore target path is invalid."));
    };
    check_confirmation(env, input, &data, "PROJECT_IO_RESTORE_CONFIRMATION_MISMATCH", "restore")?;
    check_recent_revision(recent_revision(env)?, input.expected_recent_revision)?;
    fs::rename(&source, &data).map_err(|e| SessionError::io("rename from .deleted", &source, &e))?;
    if let Err(error) = recent_upsert_locked(env, &data) {
        let _ = fs::rename(&data, &source);
        return Err(error);
    }
    Ok(RestoreResult { restored_data_directory: data, restored_from: source, summary, plan, recent: recent_list(env)? })
}

/// Settings, environment overrides and a check of the current workspace.
pub fn io_status(env: &Env) -> Result<IoStatus> {
    let settings = get_settings(env)?;
    let current_project = inspect_target(env, &settings.data_directory.display().to_string());
    Ok(IoStatus {
        session_path_active: env.session_path_active(),
        library_root_active: env.library_root_active(),
        environment_overrides: vec![
            OverrideStatus {
                env: ENV_LIBRARY_ROOT,
                active: env.library_root_active(),
                affected_operations: vec!["create_project_session", "switch_project_library", "delete_project_workspace", "restore_project_workspace"],
                next_step: format!("Unset {ENV_LIBRARY_ROOT} before changing, deleting or restoring workspaces."),
            },
            OverrideStatus {
                env: ENV_SESSION_PATH,
                active: env.session_path_active(),
                affected_operations: vec!["create_project_session", "switch_project_library", "set_project_library_settings", "reset_project_library_settings"],
                next_step: format!("Restart without {ENV_SESSION_PATH} to use the project library."),
            },
        ],
        settings,
        current_project,
    })
}

