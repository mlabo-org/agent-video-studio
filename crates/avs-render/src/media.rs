//! How the renderer finds the file behind a media record.

use std::path::PathBuf;

/// Maps a media `source` string (whatever the session stores) to an absolute path on
/// disk. The session crate owns what `source` means; this crate only asks.
/// Return `Err(reason)` when the source cannot be located.
pub trait MediaResolver {
    fn resolve(&self, source: &str) -> Result<PathBuf, String>;
}

impl<F> MediaResolver for F
where
    F: Fn(&str) -> Result<PathBuf, String>,
{
    fn resolve(&self, source: &str) -> Result<PathBuf, String> {
        self(source)
    }
}
