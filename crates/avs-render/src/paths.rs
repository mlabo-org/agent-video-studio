//! Output-path rules for exports.

use crate::error::{RenderError, Result};
use std::path::{Component, Path, PathBuf};

/// What `export` itself insists on: an absolute `.mp4` path whose parent folder exists
/// and which is not a directory. Where an output may live is the caller's policy; see
/// [`check_output_within`].
pub fn validate_output(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(RenderError::InvalidOutput(format!("output path must be absolute: {}", path.display())));
    }
    if path.components().any(|component| matches!(component, Component::ParentDir)) {
        return Err(RenderError::InvalidOutput(format!("output path must not contain '..': {}", path.display())));
    }
    let is_mp4 = path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"));
    if !is_mp4 {
        return Err(RenderError::InvalidOutput(format!("output must be an .mp4 file: {}", path.display())));
    }
    if path.file_name().is_none() || path.is_dir() {
        return Err(RenderError::InvalidOutput(format!("output must be a file path: {}", path.display())));
    }
    let parent = path.parent().ok_or_else(|| RenderError::InvalidOutput(format!("output has no parent folder: {}", path.display())))?;
    if !parent.is_dir() {
        return Err(RenderError::InvalidOutput(format!("the output folder does not exist: {}", parent.display())));
    }
    Ok(())
}

/// Succeeds when `path` lands inside one of `allowed_roots` after every symlink is
/// resolved. The file itself may not exist yet, so its parent folder is resolved and
/// the file name is re-attached; when the file does exist (even as a symlink) its own
/// resolution must also stay inside. Roots that do not exist are ignored.
pub fn check_output_within(path: &Path, allowed_roots: &[PathBuf]) -> Result<()> {
    validate_output(path)?;
    let parent = path.parent().expect("validated above");
    let name = path.file_name().expect("validated above");
    let mut resolved = parent
        .canonicalize()
        .map_err(|error| RenderError::InvalidOutput(format!("cannot resolve {}: {error}", parent.display())))?
        .join(name);
    if std::fs::symlink_metadata(&resolved).is_ok() {
        resolved = resolved
            .canonicalize()
            .map_err(|error| RenderError::InvalidOutput(format!("cannot resolve {}: {error}", resolved.display())))?;
    }
    let roots: Vec<PathBuf> = allowed_roots.iter().filter_map(|root| root.canonicalize().ok()).collect();
    if roots.iter().any(|root| resolved.starts_with(root)) {
        Ok(())
    } else {
        Err(RenderError::InvalidOutput(format!(
            "{} resolves to {}, which is outside the allowed folders",
            path.display(),
            resolved.display()
        )))
    }
}

/// The temporary file an export writes before renaming it into place.
pub(crate) fn partial_path(output: &Path) -> PathBuf {
    let name = output.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "export.mp4".into());
    output.with_file_name(format!(".{name}.partial.mp4"))
}
