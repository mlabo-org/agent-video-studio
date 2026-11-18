//! Finding ffmpeg and ffprobe, and running short helper commands safely.

use crate::error::{RenderError, Result};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

/// Every helper call (`-version`, `-filters`, ffprobe) gets this long at most.
pub const HELPER_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug)]
pub struct Toolchain {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// First line of `ffmpeg -version`.
    pub version: String,
    filters: HashSet<String>,
}

static TOOLCHAIN: OnceLock<std::result::Result<Toolchain, RenderError>> = OnceLock::new();

impl Toolchain {
    /// Finds ffmpeg (`FFMPEG_PATH`, then `PATH`) and ffprobe (`FFPROBE_PATH`, the
    /// directory of ffmpeg, then `PATH`) and lists the ffmpeg filters. The lookup
    /// runs once per process; later calls return the same answer.
    pub fn get() -> Result<&'static Toolchain> {
        TOOLCHAIN.get_or_init(Toolchain::detect).as_ref().map_err(Clone::clone)
    }

    fn detect() -> Result<Toolchain> {
        let ffmpeg = find_program("ffmpeg", "FFMPEG_PATH", None)?;
        let version = run_capture(&ffmpeg, &["-hide_banner", "-version"], HELPER_TIMEOUT)?;
        let version = String::from_utf8_lossy(&version.stdout).lines().next().unwrap_or_default().to_string();
        let sibling = ffmpeg.parent().map(|dir| dir.join("ffprobe"));
        let ffprobe = find_program("ffprobe", "FFPROBE_PATH", sibling)?;
        let listing = run_capture(&ffmpeg, &["-hide_banner", "-filters"], HELPER_TIMEOUT)?;
        // The table starts after a line of dashes; each row is "<flags> <name> <in>-><out> <text>".
        let filters = String::from_utf8_lossy(&listing.stdout)
            .lines()
            .skip_while(|line| !line.trim_start().starts_with("---"))
            .skip(1)
            .filter_map(|line| line.split_whitespace().nth(1).map(str::to_string))
            .collect();
        Ok(Toolchain { ffmpeg, ffprobe, version, filters })
    }

    pub fn has_filter(&self, name: &str) -> bool {
        self.filters.contains(name)
    }

    pub fn require_filters(&self, names: &[&str]) -> Result<()> {
        let missing: Vec<&str> = names.iter().copied().filter(|name| !self.has_filter(name)).collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(RenderError::MissingFilter(format!("this ffmpeg build lacks the filter(s): {}", missing.join(", "))))
        }
    }

    /// True when the file has at least one audio stream.
    pub fn has_audio_stream(&self, path: &Path) -> Result<bool> {
        let args = ["-v", "error", "-select_streams", "a", "-show_entries", "stream=index", "-of", "csv=p=0", &path.to_string_lossy()];
        let output = run_capture(&self.ffprobe, &args, HELPER_TIMEOUT)?;
        if !output.success {
            return Err(RenderError::MediaUnavailable {
                media_id: String::new(),
                source: path.display().to_string(),
                reason: format!("ffprobe could not read the file: {}", String::from_utf8_lossy(&output.stderr).trim()),
            });
        }
        Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
    }
}

/// Searched after `PATH`: Homebrew on Apple silicon and on Intel, and MacPorts.
pub const WELL_KNOWN_TOOL_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"];

fn find_program(name: &str, env_var: &str, sibling: Option<PathBuf>) -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(value) = std::env::var_os(env_var).filter(|value| !value.is_empty()) {
        candidates.push(PathBuf::from(value));
    }
    candidates.extend(sibling);
    candidates.push(PathBuf::from(name));
    // An app started from Finder or the Dock gets a minimal PATH without the package
    // manager's directory, so look where ffmpeg is normally installed on macOS too.
    candidates.extend(WELL_KNOWN_TOOL_DIRS.iter().map(|dir| Path::new(dir).join(name)));
    for candidate in &candidates {
        if let Ok(output) = run_capture(candidate, &["-version"], HELPER_TIMEOUT) {
            if output.success {
                return Ok(candidate.clone());
            }
        }
    }
    Err(RenderError::ToolchainMissing(format!("{name} was not found. Install it or set {env_var} to its path.")))
}

#[derive(Debug)]
pub struct Captured {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Runs a command to completion with a deadline. Both pipes are drained on threads
/// so a chatty child can never block on a full pipe; on timeout the child is killed.
pub fn run_capture(program: &Path, args: &[&str], timeout: Duration) -> Result<Captured> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| RenderError::ToolchainMissing(format!("cannot run {}: {error}", program.display())))?;
    let drain = |stream: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut buffer = Vec::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_end(&mut buffer);
            }
            buffer
        })
    };
    let stdout = drain(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let stderr = drain(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RenderError::Timeout(format!("{} did not finish within {}s", program.display(), timeout.as_secs())));
            }
            None => thread::sleep(Duration::from_millis(5)),
        }
    };
    Ok(Captured {
        success: status.success(),
        exit_code: status.code(),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}
