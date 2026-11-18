//! Bringing local files into the project library and finding them again.
//!
//! [`import_media`] copies a file under `<dataDirectory>/media/imports/<date>/`,
//! reads its facts with `ffprobe` (with a timeout) and registers it through
//! `Session::apply(RegisterMedia)`. If registering fails the copy is deleted.
//! The registered `source` is `/project-library/media/<relative path>`, and
//! [`resolve_media_source`] turns such a string back into a file path, refusing
//! anything that is not strictly inside the media folder.

use crate::env::Env;
use crate::error::{ErrorKind, Result, SessionError};
use crate::fsutil::{date_from_unix_secs, is_symlink, now_iso, unique_suffix};
use crate::session::{Applied, Session};
use avs_core::ops::RegisterMedia;
use avs_core::{MediaType, Metadata, Operation, Time};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const MEDIA_SOURCE_PREFIX: &str = "/project-library/media/";
pub const DEFAULT_IMAGE_DURATION_SECONDS: f64 = 5.0;
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

const EXTENSIONS: &[(&str, &[MediaType])] = &[
    ("mp4", &[MediaType::Video]),
    ("mov", &[MediaType::Video]),
    ("m4v", &[MediaType::Video]),
    ("webm", &[MediaType::Video, MediaType::Audio]),
    ("mp3", &[MediaType::Audio]),
    ("wav", &[MediaType::Audio]),
    ("m4a", &[MediaType::Audio]),
    ("aac", &[MediaType::Audio]),
    ("ogg", &[MediaType::Audio]),
    ("flac", &[MediaType::Audio]),
    ("png", &[MediaType::Image]),
    ("jpg", &[MediaType::Image]),
    ("jpeg", &[MediaType::Image]),
    ("webp", &[MediaType::Image]),
    ("gif", &[MediaType::Image]),
];

// ---------------------------------------------------------------- probe

/// What `ffprobe` said about a file.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaProbe {
    pub duration_seconds: Option<f64>,
    pub has_video: bool,
    pub has_audio: bool,
    /// Displayed size after rotation.
    pub display_width: Option<u32>,
    pub display_height: Option<u32>,
    pub rotation_degrees: i64,
    /// The `metadata.mediaProbe` object stored on the media asset (same shape the renderer reads).
    pub media_probe: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeAttempt {
    pub command: String,
    pub timed_out: bool,
    pub status: Option<i32>,
    pub error: Option<String>,
    pub stderr: Option<String>,
}

fn ffprobe_candidates(env: &Env) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        if !out.contains(&path) {
            out.push(path);
        }
    };
    if let Some(path) = &env.ffprobe_path {
        push(path.clone());
    }
    if let Some(ffmpeg) = &env.ffmpeg_path {
        push(ffmpeg.with_file_name("ffprobe"));
    }
    for dir in &env.path_dirs {
        push(dir.join("ffprobe"));
    }
    out
}

fn drain<R: Read + Send + 'static>(stream: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(stream) = stream {
            let _ = stream.take(8 * 1024 * 1024).read_to_end(&mut buffer);
        }
        buffer
    })
}

struct Finished {
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

fn run_with_timeout(command: &Path, args: &[&std::ffi::OsStr], timeout: Duration) -> std::io::Result<Finished> {
    let mut child = Command::new(command).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            // The process ended, so both pipes close and the readers finish.
            return Ok(Finished { status: status.code(), stdout: stdout.join().unwrap_or_default(), stderr: stderr.join().unwrap_or_default(), timed_out: false });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            // Grandchildren may still hold the pipes open, so the readers are left to finish on their own.
            return Ok(Finished { status: None, stdout: Vec::new(), stderr: Vec::new(), timed_out: true });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Runs `ffprobe` on a file. Each candidate (`FFPROBE_PATH`, `ffprobe` beside
/// `FFMPEG_PATH`, then `PATH`) gets `timeout`; a hung process is killed.
pub fn probe_media(env: &Env, file: &Path, timeout: Duration) -> std::result::Result<MediaProbe, Vec<ProbeAttempt>> {
    let mut attempts = Vec::new();
    let args: [&std::ffi::OsStr; 7] = ["-v".as_ref(), "error".as_ref(), "-show_format".as_ref(), "-show_streams".as_ref(), "-of".as_ref(), "json".as_ref(), file.as_os_str()];
    for command in ffprobe_candidates(env) {
        let mut attempt = ProbeAttempt { command: command.display().to_string(), timed_out: false, status: None, error: None, stderr: None };
        match run_with_timeout(&command, &args, timeout) {
            Err(error) => attempt.error = Some(error.to_string()),
            Ok(Finished { status, stdout, stderr, timed_out }) => {
                attempt.status = status;
                attempt.timed_out = timed_out;
                let stderr = String::from_utf8_lossy(&stderr).trim().chars().take(500).collect::<String>();
                if !stderr.is_empty() {
                    attempt.stderr = Some(stderr);
                }
                if timed_out {
                    attempt.error = Some(format!("ffprobe did not finish within {} ms and was killed.", timeout.as_millis()));
                    attempts.push(attempt);
                    // A file that hangs one ffprobe will hang the next one too.
                    return Err(attempts);
                }
                if status == Some(0) {
                    match parse_probe(&stdout) {
                        Ok(probe) => return Ok(probe),
                        Err(message) => attempt.error = Some(message),
                    }
                }
            }
        }
        attempts.push(attempt);
    }
    Err(attempts)
}

fn parse_probe(stdout: &[u8]) -> std::result::Result<MediaProbe, String> {
    let root: Value = serde_json::from_slice(stdout).map_err(|e| format!("ffprobe output was not JSON: {e}"))?;
    let root = root.as_object().ok_or("ffprobe JSON root was not an object.")?;
    let streams: Vec<&Map<String, Value>> = root.get("streams").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_object).collect()).unwrap_or_default();

    let positive = |value: Option<&Value>| -> Option<f64> {
        let number = match value? {
            Value::Number(n) => n.as_f64()?,
            Value::String(s) => s.parse().ok()?,
            _ => return None,
        };
        (number.is_finite() && number > 0.0).then_some(number)
    };
    let duration = root
        .get("format")
        .and_then(Value::as_object)
        .and_then(|f| positive(f.get("duration")))
        .or_else(|| streams.iter().find_map(|s| positive(s.get("duration"))));
    let integer = |value: Option<&Value>| positive(value).map(|n| n.round() as u32);
    let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);

    let audio_streams: Vec<_> = streams.iter().filter(|s| s.get("codec_type").and_then(Value::as_str) == Some("audio")).collect();
    let mut audio = json!({ "hasAudio": !audio_streams.is_empty(), "streamCount": audio_streams.len() });
    if let Some(primary) = audio_streams.first() {
        if let Some(codec) = text(primary.get("codec_name")) {
            audio["codecName"] = json!(codec);
        }
        if let Some(rate) = integer(primary.get("sample_rate")) {
            audio["sampleRate"] = json!(rate);
        }
        if let Some(channels) = integer(primary.get("channels")) {
            audio["channelCount"] = json!(channels);
        }
    }

    let is_attached_picture = |stream: &Map<String, Value>| match stream.get("disposition").and_then(|d| d.get("attached_pic")) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(n)) => n.as_f64() == Some(1.0),
        Some(Value::String(text)) => text == "1",
        _ => false,
    };
    let video_stream = streams.iter().find(|s| s.get("codec_type").and_then(Value::as_str) == Some("video") && !is_attached_picture(s));
    let mut media_probe = json!({ "schemaVersion": 1, "audio": audio });
    let mut display = (None, None);
    let mut rotation = 0;
    if let Some(stream) = video_stream {
        rotation = read_rotation(stream).unwrap_or(0);
        let coded_width = integer(stream.get("coded_width")).or_else(|| integer(stream.get("width")));
        let coded_height = integer(stream.get("coded_height")).or_else(|| integer(stream.get("height")));
        let stored_width = integer(stream.get("width")).or(coded_width);
        let stored_height = integer(stream.get("height")).or(coded_height);
        let mut video = json!({ "rotationDegrees": rotation });
        if let Some(codec) = text(stream.get("codec_name")) {
            video["codecName"] = json!(codec);
        }
        if let Some(w) = coded_width {
            video["codedWidth"] = json!(w);
        }
        if let Some(h) = coded_height {
            video["codedHeight"] = json!(h);
        }
        if let (Some(w), Some(h)) = (stored_width, stored_height) {
            let (dw, dh) = if rotation % 180 == 0 { (w, h) } else { (h, w) };
            let divisor = gcd(dw, dh).max(1);
            video["displayWidth"] = json!(dw);
            video["displayHeight"] = json!(dh);
            video["displayAspectRatio"] = json!({
                "width": dw / divisor,
                "height": dh / divisor,
                "value": (f64::from(dw) / f64::from(dh) * 1e6).round() / 1e6,
                "label": format!("{}:{}", dw / divisor, dh / divisor),
            });
            video["orientation"] = json!(if dw == dh { "square" } else if dw > dh { "landscape" } else { "portrait" });
            display = (Some(dw), Some(dh));
        }
        media_probe["video"] = video;
    }
    Ok(MediaProbe {
        duration_seconds: duration,
        has_video: video_stream.is_some(),
        has_audio: !audio_streams.is_empty(),
        display_width: display.0,
        display_height: display.1,
        rotation_degrees: rotation,
        media_probe,
    })
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn read_rotation(stream: &Map<String, Value>) -> Option<i64> {
    let number = |v: &Value| match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    };
    let degrees = stream
        .get("tags")
        .and_then(|t| t.get("rotate"))
        .and_then(number)
        .or_else(|| stream.get("side_data_list")?.as_array()?.iter().find_map(|d| d.get("rotation").and_then(number)))?;
    Some((degrees.round() as i64).rem_euclid(360))
}

// ---------------------------------------------------------------- import

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest {
    /// Absolute local path of the file to copy in.
    pub source_path: String,
    /// `video`, `audio` or `image`; the extension decides when omitted.
    #[serde(default, rename = "type")]
    pub media_type: Option<MediaType>,
    /// Required-by-probe for audio/video when omitted; images default to 5 s.
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub transcript: Option<String>,
    #[serde(default)]
    pub metadata: Option<Metadata>,
    /// Default 20 000.
    #[serde(default)]
    pub probe_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedLibraryInfo {
    pub absolute_path: PathBuf,
    pub relative_media_path: String,
    pub media_source: String,
    pub original_source_path: PathBuf,
    pub original_file_name: String,
    pub byte_size: u64,
    pub sha256: String,
    pub imported_at: String,
}

/// A copied file and the `RegisterMedia` input that describes it. Until it is
/// registered, [`discard_import`] removes the copy.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedImport {
    pub register_media: RegisterMedia,
    pub library: ImportedLibraryInfo,
    pub probe: Option<MediaProbe>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedMedia {
    pub media_id: String,
    pub source: String,
    pub applied: Applied,
    pub library: ImportedLibraryInfo,
    pub probe: Option<MediaProbe>,
}

fn import_error(code: &str, message: impl Into<String>) -> SessionError {
    SessionError::invalid(code, message)
}

fn extension_types(extension: &str) -> Option<&'static [MediaType]> {
    EXTENSIONS.iter().find(|(e, _)| *e == extension).map(|(_, types)| *types)
}

fn normalize_source(source: &str) -> Result<PathBuf> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return Err(import_error("IMPORT_MEDIA_SOURCE_REQUIRED", "sourcePath must be a non-empty string."));
    }
    if trimmed.contains('\0') {
        return Err(import_error("IMPORT_MEDIA_SOURCE_INVALID", "sourcePath must not include NUL characters."));
    }
    if crate::env::is_url(trimmed) {
        return Err(import_error("IMPORT_MEDIA_SOURCE_URL", "sourcePath must be a local filesystem path, not a URL.").with("sourcePath", trimmed));
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err(import_error("IMPORT_MEDIA_SOURCE_NOT_ABSOLUTE", "sourcePath must be an absolute local path.").with("sourcePath", trimmed));
    }
    Ok(crate::fsutil::lexical_clean(&path))
}

fn check_source_file(path: &Path) -> Result<()> {
    let link = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(SessionError::not_found("IMPORT_MEDIA_SOURCE_MISSING", "sourcePath does not exist.").with("sourcePath", path.display().to_string()));
        }
        Err(e) => return Err(SessionError::io("stat", path, &e)),
    };
    if link.file_type().is_symlink() {
        return Err(import_error("IMPORT_MEDIA_SOURCE_SYMLINK", "sourcePath must be a direct media file, not a symlink.").with("sourcePath", path.display().to_string()));
    }
    if link.is_dir() {
        return Err(import_error("IMPORT_MEDIA_SOURCE_DIRECTORY", "sourcePath must be a file, not a directory.").with("sourcePath", path.display().to_string()));
    }
    if !link.is_file() {
        return Err(import_error("IMPORT_MEDIA_SOURCE_NOT_FILE", "sourcePath must point to a regular file.").with("sourcePath", path.display().to_string()));
    }
    Ok(())
}

fn safe_file_stem(source: &Path) -> String {
    let stem = source.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let cleaned: String = stem.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    let mut limited = String::new();
    for c in cleaned.chars() {
        if limited.len() + c.len_utf8() > 100 {
            break;
        }
        limited.push(c);
    }
    if limited.is_empty() { "media".to_string() } else { limited }
}

/// Copies the file into the library, probes it and builds the `RegisterMedia`
/// input. Writes only the copy, not the session.
pub fn prepare_import(env: &Env, session: &Session, request: &ImportRequest) -> Result<PreparedImport> {
    let source = normalize_source(&request.source_path)?;
    check_source_file(&source)?;
    let extension = source.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let Some(allowed) = extension_types(&extension) else {
        return Err(import_error("IMPORT_MEDIA_UNSUPPORTED_EXTENSION", "sourcePath uses an unsupported media extension.").with("extension", format!(".{extension}")));
    };
    let media_type = match request.media_type {
        None => allowed[0],
        Some(MediaType::Sequence) => return Err(import_error("IMPORT_MEDIA_UNSUPPORTED_TYPE", "type must be video, audio, or image.")),
        Some(requested) if allowed.contains(&requested) => requested,
        Some(requested) => {
            return Err(import_error("IMPORT_MEDIA_EXTENSION_TYPE_MISMATCH", format!("Extension .{extension} is not supported for {requested:?} media."))
                .with("extension", format!(".{extension}")));
        }
    };
    if let Some(duration) = request.duration_seconds
        && !(duration.is_finite() && duration > 0.0)
    {
        return Err(import_error("IMPORT_MEDIA_INVALID_DURATION", "durationSeconds must be a positive finite number.").with("durationSeconds", duration));
    }

    let timeout = request.probe_timeout_ms.map_or(DEFAULT_PROBE_TIMEOUT, Duration::from_millis);
    let probe = probe_media(env, &source, timeout);
    let duration = match (request.duration_seconds, media_type, &probe) {
        (Some(duration), _, _) => duration,
        (None, MediaType::Image, _) => DEFAULT_IMAGE_DURATION_SECONDS,
        (None, _, Ok(probe)) if probe.duration_seconds.is_some() => probe.duration_seconds.unwrap_or_default(),
        (None, _, other) => {
            let attempts = other.as_ref().err().cloned().unwrap_or_default();
            return Err(import_error(
                "IMPORT_MEDIA_DURATION_UNPROBEABLE",
                "durationSeconds was omitted for audio or video media, but ffprobe could not read a finite positive duration.",
            )
            .with("sourcePath", source.display().to_string())
            .with("attempts", attempts));
        }
    };
    let duration_time = Time::from_secs((duration * 1e6).round() / 1e6)
        .filter(|t| t.is_positive())
        .ok_or_else(|| import_error("IMPORT_MEDIA_INVALID_DURATION", "durationSeconds must be a positive finite number."))?;
    let probe = probe.ok();

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let generated_id = format!("media-import-{}-{}", now.as_millis(), uuid::Uuid::new_v4().simple().to_string().get(..8).unwrap_or("00000000"));
    let import_directory = session.paths().media_root().join("imports").join(date_from_unix_secs(now.as_secs() as i64));
    fs::create_dir_all(&import_directory).map_err(|e| SessionError::io("create directory", &import_directory, &e))?;
    let (absolute_path, byte_size, sha256) = copy_into(&source, &import_directory, &safe_file_stem(&source), &extension)?;
    let relative = absolute_path
        .strip_prefix(session.paths().media_root())
        .map(|p| p.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"))
        .unwrap_or_default();
    let media_source = format!("{MEDIA_SOURCE_PREFIX}{relative}");
    let original_file_name = source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let imported_at = now_iso();

    let mut metadata = request.metadata.clone().unwrap_or_default();
    metadata.insert("byteSize".into(), json!(byte_size));
    metadata.insert("sha256".into(), json!(sha256));
    metadata.insert("projectLibraryPath".into(), json!(relative));
    metadata.insert("originalFileName".into(), json!(original_file_name));
    metadata.insert("importedAt".into(), json!(imported_at));
    if let Some(probe) = &probe {
        metadata.insert("mediaProbe".into(), probe.media_probe.clone());
    }
    let register_media = RegisterMedia {
        id: Some(generated_id),
        name: request.name.clone().unwrap_or_else(|| original_file_name.clone()),
        media_type,
        duration_seconds: duration_time,
        source: Some(media_source.clone()),
        thumbnail: (media_type == MediaType::Image).then(|| media_source.clone()),
        tags: request.tags.clone(),
        transcript: request.transcript.clone(),
        metadata: Some(metadata),
    };
    Ok(PreparedImport {
        register_media,
        library: ImportedLibraryInfo {
            absolute_path,
            relative_media_path: relative,
            media_source,
            original_source_path: source,
            original_file_name,
            byte_size,
            sha256,
            imported_at,
        },
        probe,
    })
}

/// Deletes the copy made by [`prepare_import`] (use when registering it failed).
pub fn discard_import(prepared: &PreparedImport) {
    let _ = fs::remove_file(&prepared.library.absolute_path);
}

/// Copy + probe + `Session::apply(RegisterMedia)`. The copy is deleted when the
/// edit is rejected (stale revision, lock timeout, ...). It never holds the
/// session lock while copying.
pub fn import_media(env: &Env, session: &Session, request: &ImportRequest, expected_revision: Option<u64>) -> Result<ImportedMedia> {
    let prepared = prepare_import(env, session, request)?;
    match session.apply(&Operation::RegisterMedia(prepared.register_media.clone()), expected_revision) {
        Ok(applied) => Ok(ImportedMedia {
            media_id: prepared.register_media.id.clone().unwrap_or_default(),
            source: prepared.library.media_source.clone(),
            applied,
            library: prepared.library,
            probe: prepared.probe,
        }),
        Err(error) => {
            discard_import(&prepared);
            Err(error)
        }
    }
}

/// Streams `source` into a temp file while hashing, then claims the first free
/// name (`stem.ext`, `stem-2.ext`, ...) with `create_new` and renames over it.
fn copy_into(source: &Path, directory: &Path, stem: &str, extension: &str) -> Result<(PathBuf, u64, String)> {
    let temp = directory.join(format!(".import.{}.{}.tmp", std::process::id(), unique_suffix()));
    let result = (|| -> Result<(PathBuf, u64, String)> {
        let mut input = File::open(source).map_err(|e| SessionError::io("open", source, &e))?;
        let mut output = OpenOptions::new().write(true).create_new(true).open(&temp).map_err(|e| SessionError::io("create", &temp, &e))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1 << 20];
        let mut size = 0u64;
        loop {
            let read = input.read(&mut buffer).map_err(|e| SessionError::io("read", source, &e))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            output.write_all(&buffer[..read]).map_err(|e| SessionError::io("write", &temp, &e))?;
            size += read as u64;
        }
        output.sync_all().map_err(|e| SessionError::io("sync", &temp, &e))?;
        drop(output);
        let digest: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        for n in 1u32.. {
            let name = if n == 1 { format!("{stem}.{extension}") } else { format!("{stem}-{n}.{extension}") };
            let target = directory.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&target) {
                Ok(_) => {
                    if let Err(e) = fs::rename(&temp, &target) {
                        let _ = fs::remove_file(&target);
                        return Err(SessionError::io("rename", &target, &e));
                    }
                    return Ok((target, size, digest));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(SessionError::io("create", &target, &e)),
            }
        }
        unreachable!("the loop only ends by returning")
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

// ---------------------------------------------------------------- resolve

/// Turns `/project-library/media/<relative path>` into a file under `media_root`.
/// Rejects other prefixes, empty/`.`/`..` components, backslashes, NUL, symbolic
/// links in any component below `media_root`, and anything that is not a file.
pub fn resolve_media_source(media_root: &Path, source: &str) -> Result<PathBuf> {
    let bad = |message: &str| SessionError::invalid("MEDIA_SOURCE_INVALID", message.to_string()).with("source", source);
    let relative = source.strip_prefix(MEDIA_SOURCE_PREFIX).ok_or_else(|| bad("Media source must start with /project-library/media/."))?;
    if relative.contains('\0') || relative.contains('\\') {
        return Err(bad("Media source contains a forbidden character."));
    }
    let mut path = media_root.to_path_buf();
    let mut count = 0;
    for component in relative.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(bad("Media source must be a clean relative path inside the media folder."));
        }
        count += 1;
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(SessionError::new(ErrorKind::Invalid, "MEDIA_SOURCE_SYMLINK", "Media source passes through a symbolic link.").with("source", source));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(SessionError::not_found("MEDIA_SOURCE_NOT_FOUND", format!("No media file at {}.", path.display())).with("source", source));
            }
            Err(e) => return Err(SessionError::io("stat", &path, &e)),
        }
    }
    if count == 0 || !path.is_file() {
        return Err(bad("Media source is not a file."));
    }
    debug_assert!(!is_symlink(&path));
    Ok(path)
}

impl Session {
    /// [`resolve_media_source`] against this library's media folder.
    pub fn resolve_media_source(&self, source: &str) -> Result<PathBuf> {
        resolve_media_source(&self.paths().media_root(), source)
    }
}
