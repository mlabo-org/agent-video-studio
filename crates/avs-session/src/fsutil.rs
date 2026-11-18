//! Small filesystem helpers shared by every module: atomic writes, the advisory
//! lock, lexical path cleaning and a UTC timestamp formatter.

use crate::error::{ErrorKind, Result, SessionError};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long a writer waits for the lock before giving up with `LOCK_TIMEOUT`.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Holds an OS advisory lock on a file until dropped. The operating system releases
/// it when the process exits, so there is never a stale lock to clean up. The lock
/// file itself is never deleted (deleting it would let two holders lock two inodes).
#[derive(Debug)]
pub struct Lock {
    file: File,
}

impl Lock {
    pub fn acquire(path: &Path, timeout: Duration) -> Result<Lock> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| SessionError::io("create directory", parent, &e))?;
        }
        // A fresh handle per call: the lock belongs to the open file description.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| SessionError::io("open lock file", path, &e))?;
        let deadline = Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Lock { file }),
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err(SessionError::new(
                            ErrorKind::LockTimeout,
                            "LOCK_TIMEOUT",
                            format!("Timed out after {} ms waiting for the lock {}.", timeout.as_millis(), path.display()),
                        )
                        .with("lockPath", path.display().to_string())
                        .with("timeoutMs", timeout.as_millis() as u64));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(TryLockError::Error(e)) => return Err(SessionError::io("lock", path, &e)),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Writes `bytes` to a temporary file beside `path`, syncs it, then renames it over
/// `path`. Readers see the old or the new file, never a partial one.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| SessionError::invalid("PATH_INVALID", format!("{} has no parent directory.", path.display())))?;
    fs::create_dir_all(parent).map_err(|e| SessionError::io("create directory", parent, &e))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), unique_suffix()));
    let write = || -> std::io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    };
    match write() {
        Ok(()) => {
            // Best effort: make the rename itself durable.
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&temp);
            Err(SessionError::io("write", path, &e))
        }
    }
}

pub fn unique_suffix() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{nanos}-{}", uuid::Uuid::new_v4().simple().to_string().get(..6).unwrap_or("0"))
}

pub fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| SessionError::invalid("SERIALIZE_FAILED", e.to_string()))?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

/// Reads a file, mapping "does not exist" to `None`.
pub fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => Ok(None),
        Err(e) => Err(SessionError::io("read", path, &e)),
    }
}

/// Resolves `.` and `..` without touching the filesystem (like Node's `path.resolve`).
pub fn lexical_clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

pub fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Whether `path` looks writable from its permission bits. This never creates
/// anything, so it can be used on read-only paths; the real write still reports
/// the real error. For a missing path the nearest existing ancestor is checked.
pub fn looks_writable(path: &Path) -> bool {
    let mut candidate = path;
    loop {
        match fs::metadata(candidate) {
            Ok(meta) => return meta.is_dir() && !meta.permissions().readonly(),
            Err(_) => match candidate.parent() {
                Some(parent) if parent != candidate => candidate = parent,
                _ => return false,
            },
        }
    }
}

/// Current time as `2026-01-31T12:34:56.789Z`.
pub fn now_iso() -> String {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    iso_from_unix_millis(elapsed.as_millis() as i64)
}

pub fn iso_from_unix_millis(millis: i64) -> String {
    let days = millis.div_euclid(86_400_000);
    let rest = millis.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

/// `YYYY-MM-DD` (UTC) for a unix time.
pub fn date_from_unix_secs(secs: i64) -> String {
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_timestamps() {
        assert_eq!(iso_from_unix_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_unix_millis(1_709_210_096_789), "2024-02-29T12:34:56.789Z");
    }

    #[test]
    fn cleans_paths_lexically() {
        assert_eq!(lexical_clean(Path::new("/a/b/../c/./d")), PathBuf::from("/a/c/d"));
    }
}
