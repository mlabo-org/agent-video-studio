//! Where things live. Every location and override is a field of [`Env`]; only
//! [`Env::from_process`] reads the process environment, so tests build an `Env`
//! inside a temp directory and can never touch the real user data.

use crate::error::{Result, SessionError};
use crate::fsutil::lexical_clean;
use std::path::{Path, PathBuf};

pub const APP_DIRECTORY_NAME: &str = "Agent Video Studio";
pub const DATA_DIRECTORY_NAME: &str = "Project Library";
pub const SESSION_FILE_NAME: &str = "project-session.json";
pub const SETTINGS_FILE_NAME: &str = "project-library-settings.json";
pub const RECENT_FILE_NAME: &str = "recent-workspaces.json";
/// Same names the TypeScript implementation read.
pub const ENV_SESSION_PATH: &str = "AVS_PROJECT_SESSION_PATH";
pub const ENV_LIBRARY_ROOT: &str = "AVS_PROJECT_LIBRARY_ROOT";

#[derive(Debug, Clone, Default)]
pub struct Env {
    /// Home directory: base of the default locations and target of `~/`.
    pub home: PathBuf,
    /// Overrides the settings file location (default: under Application Support).
    pub settings_path: Option<PathBuf>,
    /// `AVS_PROJECT_LIBRARY_ROOT`: use exactly this data directory (no app suffix added).
    pub library_root: Option<String>,
    /// `AVS_PROJECT_SESSION_PATH`: use exactly this session file (single-file mode).
    pub session_path: Option<PathBuf>,
    /// `FFPROBE_PATH`.
    pub ffprobe_path: Option<PathBuf>,
    /// `FFMPEG_PATH`; `ffprobe` next to it is the second choice.
    pub ffmpeg_path: Option<PathBuf>,
    /// Directories searched for `ffprobe` last (the `PATH` entries).
    pub path_dirs: Vec<PathBuf>,
}

impl Env {
    /// No overrides, nothing on the search path: what tests start from.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Env { home: home.into(), ..Env::default() }
    }

    /// Reads `HOME`, the two `AVS_*` overrides, `FFPROBE_PATH`, `FFMPEG_PATH` and `PATH`.
    pub fn from_process() -> Self {
        let var = |name: &str| std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let cwd = std::env::current_dir().unwrap_or_default();
        let absolute = |value: String| {
            let path = PathBuf::from(value);
            if path.is_absolute() { path } else { lexical_clean(&cwd.join(path)) }
        };
        Env {
            home: std::env::home_dir().unwrap_or_default(),
            settings_path: None,
            library_root: var(ENV_LIBRARY_ROOT),
            session_path: var(ENV_SESSION_PATH).map(absolute),
            ffprobe_path: var("FFPROBE_PATH").map(absolute),
            ffmpeg_path: var("FFMPEG_PATH").map(absolute),
            path_dirs: {
                let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
                // An app started from Finder or the Dock gets a minimal PATH without the
                // package manager's directory, so look where ffprobe is normally installed
                // on macOS too (Homebrew on Apple silicon and Intel, MacPorts).
                for well_known in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"] {
                    if !dirs.iter().any(|dir| dir == Path::new(well_known)) {
                        dirs.push(PathBuf::from(well_known));
                    }
                }
                dirs
            },
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        if let Some(path) = &self.settings_path {
            return lexical_clean(path);
        }
        if cfg!(target_os = "macos") {
            self.home.join("Library").join("Application Support").join(APP_DIRECTORY_NAME).join(SETTINGS_FILE_NAME)
        } else {
            self.home.join(".config").join("agent-video-studio").join(SETTINGS_FILE_NAME)
        }
    }

    pub fn recent_file(&self) -> PathBuf {
        self.settings_file().with_file_name(RECENT_FILE_NAME)
    }

    /// The lock that serialises writes to the settings and recent files.
    pub fn config_lock_file(&self) -> PathBuf {
        self.settings_file().with_file_name(".app-config.lock")
    }

    pub fn default_data_directory(&self) -> PathBuf {
        let media = if cfg!(target_os = "macos") { "Movies" } else { "Videos" };
        self.home.join(media).join(APP_DIRECTORY_NAME).join(DATA_DIRECTORY_NAME)
    }

    pub fn library_root_active(&self) -> bool {
        self.library_root.is_some()
    }

    pub fn session_path_active(&self) -> bool {
        self.session_path.is_some()
    }

    /// Checks a data-directory string: non-empty, a path (not a URL), absolute or
    /// `~/...`, and not the filesystem root. Returns the cleaned absolute path.
    pub fn normalize_data_directory(&self, value: &str) -> Result<PathBuf> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(SessionError::invalid("DATA_DIRECTORY_REQUIRED", "Project library dataDirectory must be a non-empty string."));
        }
        if is_url_like(trimmed) {
            return Err(SessionError::invalid("DATA_DIRECTORY_URL", "Project library dataDirectory must be a filesystem path, not a URL."));
        }
        let expanded = self.expand_home(trimmed);
        if !expanded.is_absolute() {
            return Err(SessionError::invalid("DATA_DIRECTORY_NOT_ABSOLUTE", "Project library dataDirectory must be an absolute path or start with ~/."));
        }
        let resolved = lexical_clean(&expanded);
        if resolved.parent().is_none() {
            return Err(SessionError::invalid("DATA_DIRECTORY_ROOT", "Project library dataDirectory cannot resolve to the filesystem root."));
        }
        Ok(resolved)
    }

    /// Like [`Env::normalize_data_directory`], then makes the path app-owned: a
    /// folder ending in `Agent Video Studio/Project Library` is kept, `Agent Video
    /// Studio` gets `Project Library` added, anything else gets both added.
    pub fn app_owned_data_directory(&self, value: &str) -> Result<PathBuf> {
        let selected = self.normalize_data_directory(value)?;
        if is_app_owned(&selected) {
            return Ok(selected);
        }
        if selected.file_name().is_some_and(|name| name == APP_DIRECTORY_NAME) {
            return Ok(selected.join(DATA_DIRECTORY_NAME));
        }
        Ok(selected.join(APP_DIRECTORY_NAME).join(DATA_DIRECTORY_NAME))
    }

    /// `~`, `~/x` and relative-to-nothing inputs resolved the way the old tools did.
    pub fn resolve_selected_path(&self, value: &str) -> PathBuf {
        let value = value.trim();
        if value == "~" {
            return lexical_clean(&self.home);
        }
        if let Some(rest) = value.strip_prefix("~/") {
            return lexical_clean(&self.home.join(rest));
        }
        lexical_clean(Path::new(value))
    }

    fn expand_home(&self, value: &str) -> PathBuf {
        if value == "~" {
            return self.home.clone();
        }
        match value.strip_prefix("~/") {
            Some(rest) => self.home.join(rest),
            None => PathBuf::from(value),
        }
    }
}

pub fn is_app_owned(data_directory: &Path) -> bool {
    data_directory.file_name().is_some_and(|name| name == DATA_DIRECTORY_NAME)
        && data_directory.parent().and_then(Path::file_name).is_some_and(|name| name == APP_DIRECTORY_NAME)
}

fn is_url_like(value: &str) -> bool {
    let mut chars = value.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    for c in chars {
        if c == ':' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')) {
            return false;
        }
    }
    false
}

pub(crate) fn is_url(value: &str) -> bool {
    is_url_like(value)
}
