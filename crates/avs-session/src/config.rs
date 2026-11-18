//! Library settings (which data directory is current) and the recent-workspaces
//! list. Both live next to each other in the app config directory and share one
//! lock. Reading never creates a file or a folder.

use crate::env::{ENV_SESSION_PATH, Env, SESSION_FILE_NAME};
use crate::error::{ErrorKind, Result, SessionError};
use crate::fsutil::{DEFAULT_LOCK_TIMEOUT, Lock, looks_writable, now_iso, read_optional, write_json};
use crate::session::{ProjectSummary, read_summary};
use serde::Serialize;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const RECENT_SCHEMA_VERSION: u32 = 1;
pub const RECENT_LIMIT: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingsSource {
    Default,
    Config,
    Env,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedLibrary {
    pub revision: u64,
    pub data_directory: PathBuf,
    pub source: SettingsSource,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibrarySettings {
    pub schema_version: u32,
    pub revision: u64,
    pub data_directory: PathBuf,
    pub source: SettingsSource,
    /// From permission bits only (reading must not create anything).
    pub writable: bool,
}

pub(crate) fn config_lock(env: &Env) -> Result<Lock> {
    Lock::acquire(&env.config_lock_file(), DEFAULT_LOCK_TIMEOUT)
}

// ---------------------------------------------------------------- settings

struct StoredSettings {
    revision: u64,
    data_directory: PathBuf,
}

fn invalid_settings(env: &Env, code: &str, message: String) -> SessionError {
    SessionError::invalid(code, format!("{message} Run reset_library_settings to discard the file."))
        .with("settingsFile", env.settings_file().display().to_string())
}

fn read_stored_settings(env: &Env) -> Result<Option<StoredSettings>> {
    let path = env.settings_file();
    let Some(bytes) = read_optional(&path)? else { return Ok(None) };
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| invalid_settings(env, "SETTINGS_INVALID_JSON", format!("{} is not valid JSON: {e}.", path.display())))?;
    let Some(map) = value.as_object() else {
        return Err(invalid_settings(env, "SETTINGS_INVALID", "Project library settings must be an object.".into()));
    };
    if map.get("schemaVersion").and_then(Value::as_u64) != Some(SETTINGS_SCHEMA_VERSION as u64) {
        return Err(invalid_settings(env, "SETTINGS_INVALID", "Unsupported project library settings schemaVersion.".into()));
    }
    let Some(revision) = map.get("revision").and_then(Value::as_u64).filter(|r| *r >= 1) else {
        return Err(invalid_settings(env, "SETTINGS_INVALID", "Settings revision must be a positive integer.".into()));
    };
    let Some(data_directory) = map.get("dataDirectory").and_then(Value::as_str) else {
        return Err(invalid_settings(env, "SETTINGS_INVALID", "Settings dataDirectory must be a string.".into()));
    };
    let data_directory = env.app_owned_data_directory(data_directory).map_err(|e| invalid_settings(env, "SETTINGS_INVALID", e.message))?;
    Ok(Some(StoredSettings { revision, data_directory }))
}

/// The data directory in effect: `AVS_PROJECT_LIBRARY_ROOT`, else the settings file, else the default.
pub fn resolve_library(env: &Env) -> Result<ResolvedLibrary> {
    if let Some(root) = &env.library_root {
        let revision = read_stored_settings(env).ok().flatten().map_or(0, |s| s.revision);
        return Ok(ResolvedLibrary { revision, data_directory: env.normalize_data_directory(root)?, source: SettingsSource::Env });
    }
    Ok(match read_stored_settings(env)? {
        Some(stored) => ResolvedLibrary { revision: stored.revision, data_directory: stored.data_directory, source: SettingsSource::Config },
        None => ResolvedLibrary { revision: 0, data_directory: env.default_data_directory(), source: SettingsSource::Default },
    })
}

pub fn get_settings(env: &Env) -> Result<LibrarySettings> {
    let resolved = resolve_library(env)?;
    Ok(LibrarySettings {
        schema_version: SETTINGS_SCHEMA_VERSION,
        revision: resolved.revision,
        writable: looks_writable(&resolved.data_directory),
        data_directory: resolved.data_directory,
        source: resolved.source,
    })
}

pub(crate) fn ensure_session_path_not_active(env: &Env, operation: &str) -> Result<()> {
    if env.session_path_active() {
        return Err(SessionError::new(
            ErrorKind::Blocked,
            "ENV_OVERRIDE_ACTIVE",
            format!("{operation} is blocked while {ENV_SESSION_PATH} is set: the process uses a single session file and library changes would not affect it."),
        )
        .with("env", ENV_SESSION_PATH));
    }
    Ok(())
}

/// Points the app at another data directory (made app-owned: `.../Agent Video Studio/Project Library`).
/// Creates the directory and proves it is writable.
pub fn set_settings(env: &Env, data_directory: &str, expected_revision: Option<u64>) -> Result<LibrarySettings> {
    ensure_session_path_not_active(env, "Changing the project library settings")?;
    let _lock = config_lock(env)?;
    let directory = env.app_owned_data_directory(data_directory)?;
    assert_writable_directory(&directory)?;
    write_settings_locked(env, directory, expected_revision)
}

/// Writes the settings file; the caller holds the config lock.
pub(crate) fn write_settings_locked(env: &Env, directory: PathBuf, expected_revision: Option<u64>) -> Result<LibrarySettings> {
    let current = read_stored_settings(env)?.map_or(0, |s| s.revision);
    check_settings_revision(current, expected_revision)?;
    let revision = current + 1;
    write_json(
        &env.settings_file(),
        &json!({ "schemaVersion": SETTINGS_SCHEMA_VERSION, "revision": revision, "dataDirectory": directory }),
    )?;
    Ok(LibrarySettings {
        schema_version: SETTINGS_SCHEMA_VERSION,
        revision,
        data_directory: directory,
        source: SettingsSource::Config,
        writable: true,
    })
}

pub(crate) fn check_settings_revision(actual: u64, expected: Option<u64>) -> Result<()> {
    match expected {
        Some(expected) if expected != actual => Err(SessionError::revision_mismatch("SETTINGS_REVISION_MISMATCH", "Project library settings", expected, actual)),
        _ => Ok(()),
    }
}

/// Deletes the settings file so the default applies again. Works even when the
/// file is unreadable (that is the way to repair a broken one); the revision check
/// only applies when the file can be read.
pub fn reset_settings(env: &Env, expected_revision: Option<u64>) -> Result<LibrarySettings> {
    ensure_session_path_not_active(env, "Resetting the project library settings")?;
    let _lock = config_lock(env)?;
    if let Ok(Some(stored)) = read_stored_settings(env) {
        check_settings_revision(stored.revision, expected_revision)?;
    }
    let path = env.settings_file();
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(SessionError::io("remove", &path, &e)),
    }
    get_settings(env)
}

/// Creates the directory if needed and proves a file can be created in it.
pub(crate) fn assert_writable_directory(directory: &Path) -> Result<()> {
    fs::create_dir_all(directory).map_err(|e| {
        SessionError::invalid("DATA_DIRECTORY_CREATE_FAILED", format!("Project library dataDirectory could not be created: {}: {e}", directory.display()))
    })?;
    if !directory.is_dir() {
        return Err(SessionError::invalid("DATA_DIRECTORY_NOT_DIRECTORY", format!("Project library dataDirectory is not a directory: {}", directory.display())));
    }
    let probe = directory.join(format!(".avs-write-test-{}-{}", std::process::id(), crate::fsutil::unique_suffix()));
    let result = fs::OpenOptions::new().write(true).create_new(true).open(&probe);
    let _ = fs::remove_file(&probe);
    result.map(|_| ()).map_err(|e| {
        SessionError::invalid("DATA_DIRECTORY_NOT_WRITABLE", format!("Project library dataDirectory is not writable: {}: {e}", directory.display()))
    })
}

// ---------------------------------------------------------------- recent workspaces

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reachability {
    Available,
    DataDirectoryMissing,
    DataDirectoryNotDirectory,
    SessionMissing,
    SessionNotFile,
    SessionInvalid,
    EntryInvalid,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentWorkspace {
    pub data_directory: String,
    pub first_opened_at: String,
    pub last_opened_at: String,
    pub current: bool,
    pub reachable: bool,
    pub reachability: Reachability,
    pub summary: Option<ProjectSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentList {
    pub schema_version: u32,
    pub revision: u64,
    pub entries: Vec<RecentWorkspace>,
    /// Set when the file was unreadable and treated as empty (the next change replaces it).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgetResult {
    pub forgotten: bool,
    #[serde(flatten)]
    pub list: RecentList,
}

#[derive(Debug, Clone)]
struct Entry {
    raw_directory: String,
    canonical: Option<PathBuf>,
    first_opened_at: String,
    last_opened_at: String,
}

struct RecentFile {
    revision: u64,
    entries: Vec<Entry>,
    warning: Option<String>,
}

fn read_recent(env: &Env) -> Result<RecentFile> {
    let path = env.recent_file();
    let Some(bytes) = read_optional(&path)? else { return Ok(RecentFile { revision: 0, entries: Vec::new(), warning: None }) };
    let parsed = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(|v| v.get("schemaVersion").and_then(Value::as_u64) == Some(RECENT_SCHEMA_VERSION as u64))
        .and_then(|v| Some((v.get("revision")?.as_u64()?, v.get("entries")?.as_array()?.clone())));
    let Some((revision, items)) = parsed else {
        return Ok(RecentFile {
            revision: 0,
            entries: Vec::new(),
            warning: Some(format!("{} could not be read and is treated as empty; the next change replaces it.", path.display())),
        });
    };
    let mut entries: Vec<Entry> = Vec::new();
    for item in items {
        let raw = item.get("dataDirectory").and_then(Value::as_str).unwrap_or_default().to_string();
        let stamp = |key: &str| item.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty()).unwrap_or("1970-01-01T00:00:00.000Z").to_string();
        let canonical = env.app_owned_data_directory(&raw).ok();
        let entry = Entry { raw_directory: raw, canonical, first_opened_at: stamp("firstOpenedAt"), last_opened_at: stamp("lastOpenedAt") };
        if !entries.iter().any(|e| same_entry(e, &entry)) {
            entries.push(entry);
        }
    }
    entries.truncate(RECENT_LIMIT);
    Ok(RecentFile { revision, entries, warning: None })
}

fn same_entry(a: &Entry, b: &Entry) -> bool {
    match (&a.canonical, &b.canonical) {
        (Some(x), Some(y)) => x == y,
        (None, None) => a.raw_directory == b.raw_directory,
        _ => false,
    }
}

fn write_recent(env: &Env, revision: u64, entries: &[Entry]) -> Result<()> {
    let items: Vec<Value> = entries
        .iter()
        .map(|e| {
            let directory = e.canonical.as_ref().map_or_else(|| e.raw_directory.clone(), |p| p.display().to_string());
            json!({ "dataDirectory": directory, "firstOpenedAt": e.first_opened_at, "lastOpenedAt": e.last_opened_at })
        })
        .collect();
    write_json(&env.recent_file(), &json!({ "schemaVersion": RECENT_SCHEMA_VERSION, "revision": revision, "entries": items }))
}

fn inspect_entry(entry: &Entry, current: Option<&Path>) -> RecentWorkspace {
    let make = |reachability, summary, message: Option<String>, is_current| RecentWorkspace {
        data_directory: entry.canonical.as_ref().map_or_else(|| entry.raw_directory.clone(), |p| p.display().to_string()),
        first_opened_at: entry.first_opened_at.clone(),
        last_opened_at: entry.last_opened_at.clone(),
        current: is_current,
        reachable: reachability == Reachability::Available,
        reachability,
        summary,
        message,
    };
    let Some(directory) = &entry.canonical else {
        return make(Reachability::EntryInvalid, None, Some("Recent workspace entry is invalid.".into()), false);
    };
    let is_current = current.is_some_and(|c| c == directory);
    match fs::metadata(directory) {
        Err(_) => return make(Reachability::DataDirectoryMissing, None, Some("Project library data directory is missing.".into()), is_current),
        Ok(meta) if !meta.is_dir() => {
            return make(Reachability::DataDirectoryNotDirectory, None, Some("Project library data directory is not a directory.".into()), is_current);
        }
        Ok(_) => {}
    }
    let session_file = directory.join(SESSION_FILE_NAME);
    match fs::metadata(&session_file) {
        Err(_) => return make(Reachability::SessionMissing, None, Some(format!("{SESSION_FILE_NAME} is missing.")), is_current),
        Ok(meta) if !meta.is_file() => {
            return make(Reachability::SessionNotFile, None, Some(format!("{SESSION_FILE_NAME} is not a file.")), is_current);
        }
        Ok(_) => {}
    }
    match read_summary(&session_file) {
        Ok(Some(summary)) => make(Reachability::Available, Some(summary), None, is_current),
        Ok(None) => make(Reachability::SessionMissing, None, Some(format!("{SESSION_FILE_NAME} is missing.")), is_current),
        Err(error) => make(Reachability::SessionInvalid, None, Some(error.message), is_current),
    }
}

fn to_list(env: &Env, file: &RecentFile) -> RecentList {
    let current = resolve_library(env).ok().map(|c| c.data_directory);
    RecentList {
        schema_version: RECENT_SCHEMA_VERSION,
        revision: file.revision,
        entries: file.entries.iter().map(|entry| inspect_entry(entry, current.as_deref())).collect(),
        warnings: file.warning.iter().cloned().collect(),
    }
}

/// Recent workspaces, newest first, each checked for reachability. Reads only.
pub fn recent_list(env: &Env) -> Result<RecentList> {
    Ok(to_list(env, &read_recent(env)?))
}

/// Removes a workspace from the list (the folder is untouched).
pub fn recent_forget(env: &Env, data_directory: &str, expected_recent_revision: Option<u64>) -> Result<ForgetResult> {
    let _lock = config_lock(env)?;
    recent_forget_locked(env, data_directory, expected_recent_revision)
}

pub(crate) fn recent_forget_locked(env: &Env, data_directory: &str, expected: Option<u64>) -> Result<ForgetResult> {
    let mut file = read_recent(env)?;
    check_recent_revision(file.revision, expected)?;
    let target = env.app_owned_data_directory(data_directory).ok();
    let before = file.entries.len();
    file.entries.retain(|e| match (&e.canonical, &target) {
        (Some(a), Some(b)) => a != b,
        _ => e.raw_directory != data_directory,
    });
    let forgotten = file.entries.len() != before;
    if forgotten {
        file.revision += 1;
        write_recent(env, file.revision, &file.entries)?;
    }
    Ok(ForgetResult { forgotten, list: to_list(env, &file) })
}

/// Moves a workspace to the top of the list; the caller holds the config lock.
pub(crate) fn recent_upsert_locked(env: &Env, data_directory: &Path) -> Result<()> {
    let mut file = read_recent(env)?;
    let now = now_iso();
    let canonical = env.app_owned_data_directory(&data_directory.display().to_string())?;
    let mut first = now.clone();
    file.entries.retain(|e| {
        if e.canonical.as_ref() == Some(&canonical) {
            first = e.first_opened_at.clone();
            false
        } else {
            true
        }
    });
    file.entries.insert(0, Entry { raw_directory: canonical.display().to_string(), canonical: Some(canonical), first_opened_at: first, last_opened_at: now });
    file.entries.truncate(RECENT_LIMIT);
    write_recent(env, file.revision + 1, &file.entries)
}

/// Current recent revision (0 when the file is missing or unreadable).
pub(crate) fn recent_revision(env: &Env) -> Result<u64> {
    Ok(read_recent(env)?.revision)
}

pub(crate) fn check_recent_revision(actual: u64, expected: Option<u64>) -> Result<()> {
    match expected {
        Some(expected) if expected != actual => Err(SessionError::revision_mismatch("RECENT_REVISION_MISMATCH", "Recent project workspaces", expected, actual)),
        _ => Ok(()),
    }
}

