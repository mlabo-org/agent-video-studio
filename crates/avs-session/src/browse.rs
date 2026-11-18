//! Folder listing for the folder picker. Reads only.

use crate::env::Env;
use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrowseMode {
    /// Start at the default library location; paths must be absolute or `~/...`.
    #[default]
    ProjectLibrary,
    /// Start at the home folder; any path.
    Filesystem,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: PathBuf,
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BrowseError {
    /// `NOT_FOUND`, `PERMISSION_DENIED`, `NOT_A_DIRECTORY` or `BROWSE_FAILED`.
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryListing {
    pub current_path: PathBuf,
    pub parent_path: Option<PathBuf>,
    pub home_path: PathBuf,
    pub default_path: PathBuf,
    pub entries: Vec<DirectoryEntry>,
    pub error: Option<BrowseError>,
}

/// Lists the sub-folders of `requested_path` (default: the mode's start folder).
/// Hidden folders are shown only when the folder itself is inside a hidden path.
/// A folder that cannot be read comes back as a listing with `error` set.
pub fn browse_directories(env: &Env, requested_path: Option<&str>, mode: BrowseMode) -> Result<DirectoryListing> {
    let home = crate::fsutil::lexical_clean(&env.home);
    let default_path = match mode {
        BrowseMode::Filesystem => home.clone(),
        BrowseMode::ProjectLibrary => env.default_data_directory(),
    };
    let current = match (mode, requested_path.map(str::trim).filter(|p| !p.is_empty())) {
        (BrowseMode::Filesystem, None) => default_path.clone(),
        (BrowseMode::Filesystem, Some(path)) => env.resolve_selected_path(path),
        (BrowseMode::ProjectLibrary, None) => default_path.clone(),
        (BrowseMode::ProjectLibrary, Some(path)) => env.normalize_data_directory(path)?,
    };
    let parent_path = allowed_parent(&current);
    let listing = |entries, error| DirectoryListing {
        current_path: current.clone(),
        parent_path: parent_path.clone(),
        home_path: home.clone(),
        default_path: default_path.clone(),
        entries,
        error,
    };
    let reader = match fs::read_dir(&current) {
        Ok(reader) => reader,
        Err(error) => return Ok(listing(Vec::new(), Some(browse_error(&current, &error)))),
    };
    let show_hidden = is_hidden_path(&current);
    let mut entries: Vec<DirectoryEntry> = Vec::new();
    for item in reader.flatten() {
        // `is_dir` follows links, so a link to a folder is offered like a folder.
        if !item.path().is_dir() {
            continue;
        }
        let name = item.file_name().to_string_lossy().into_owned();
        let hidden = name.starts_with('.');
        if hidden && !show_hidden {
            continue;
        }
        entries.push(DirectoryEntry { path: current.join(&name), name, hidden });
    }
    entries.sort_by_key(|entry| (entry.name.to_lowercase(), entry.name.clone()));
    Ok(listing(entries, None))
}

fn allowed_parent(current: &Path) -> Option<PathBuf> {
    let parent = current.parent()?;
    // The filesystem root is never offered as a parent.
    (parent.parent().is_some()).then(|| parent.to_path_buf())
}

fn is_hidden_path(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.') && c.as_os_str() != "." && c.as_os_str() != "..")
}

fn browse_error(path: &Path, error: &std::io::Error) -> BrowseError {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => "NOT_FOUND",
        std::io::ErrorKind::PermissionDenied => "PERMISSION_DENIED",
        std::io::ErrorKind::NotADirectory => "NOT_A_DIRECTORY",
        _ => "BROWSE_FAILED",
    };
    BrowseError { code: code.into(), message: format!("Could not list directories under {}: {error}", path.display()) }
}
