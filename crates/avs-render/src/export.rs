//! Writing the final MP4.
//!
//! The file is first written beside its destination as `.<name>.partial.mp4` and
//! renamed over the destination only after ffmpeg exits cleanly. An output file that
//! already exists is therefore never touched until the new one is complete, and a
//! failed or cancelled export leaves nothing behind but the old file.

use crate::error::{RenderError, Result};
use crate::fmt::{fmt_num, fmt_time, shell_join};
use crate::graph::{GraphOptions, RenderGraph, RenderWarning, Streams, UsedSource, VideoFormat};
use crate::media::MediaResolver;
use crate::paths::{partial_path, validate_output};
use crate::tool::Toolchain;
use avs_core::{Project, Time};
use serde::Serialize;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How long a cancelled ffmpeg gets to stop after SIGTERM before SIGKILL.
const TERMINATE_GRACE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(50);
const STDERR_TAIL_BYTES: usize = 16 * 1024;

/// A shared flag the caller flips to stop an export.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// 0 to 1 of the timeline range being exported.
    pub ratio: f64,
    /// How far into the output ffmpeg has written.
    pub out_time_seconds: f64,
    /// Wall-clock seconds since the export started.
    pub elapsed_seconds: f64,
    pub frame: Option<u64>,
    pub speed: Option<String>,
    pub finished: bool,
}

/// Called on the exporting thread each time ffmpeg reports progress.
pub type ProgressCallback = Box<dyn FnMut(&Progress) + Send>;

pub struct ExportOptions {
    /// Absolute path of the `.mp4` to write.
    pub output: PathBuf,
    /// Timeline range to export; the whole timeline by default.
    pub start: Time,
    pub duration: Option<Time>,
    /// Defaults to the project's resolution and fps.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    /// Replace an existing output file once the new one is complete. When false an
    /// existing file is an error. (This crate does not decide whether replacing is
    /// allowed; the caller does.)
    pub overwrite: bool,
    pub cancel: Option<CancelToken>,
    pub on_progress: Option<ProgressCallback>,
}

impl ExportOptions {
    pub fn new(output: impl Into<PathBuf>) -> Self {
        ExportOptions {
            output: output.into(),
            start: Time::ZERO,
            duration: None,
            width: None,
            height: None,
            fps: None,
            overwrite: true,
            cancel: None,
            on_progress: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub output: PathBuf,
    pub start_seconds: Time,
    pub duration_seconds: Time,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub bytes: u64,
    /// Every media file that went into the render.
    pub sources: Vec<UsedSource>,
    pub caption_clip_ids: Vec<String>,
    /// Anything left out or changed; empty means the render is complete.
    pub warnings: Vec<RenderWarning>,
    pub ffmpeg_command: String,
    pub ffmpeg_version: String,
    pub elapsed_seconds: f64,
}

/// Deletes the partial file unless the export committed it.
struct PartialFile {
    path: PathBuf,
    committed: bool,
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub fn export(project: &Project, resolver: &dyn MediaResolver, mut options: ExportOptions) -> Result<ExportResult> {
    let began = Instant::now();
    validate_output(&options.output)?;
    if !options.overwrite && options.output.exists() {
        return Err(RenderError::InvalidOutput(format!("{} already exists", options.output.display())));
    }
    let cancelled = |options: &ExportOptions| options.cancel.as_ref().is_some_and(CancelToken::is_cancelled);
    if cancelled(&options) {
        return Err(RenderError::Cancelled);
    }

    let graph_options = GraphOptions {
        start: options.start,
        duration: options.duration,
        width: options.width.unwrap_or(project.resolution.width),
        height: options.height.unwrap_or(project.resolution.height),
        fps: options.fps.unwrap_or(project.fps),
        sample_rate: project.sample_rate,
        channels: 2,
        streams: Streams::Both,
        video_format: VideoFormat::Yuv420p,
    };
    let graph = RenderGraph::build(project, resolver, &graph_options)?;
    if cancelled(&options) {
        return Err(RenderError::Cancelled);
    }

    let toolchain = Toolchain::get()?;
    let partial = partial_path(&options.output);
    let args = export_args(&graph, &partial);
    let command = shell_join(&std::iter::once(toolchain.ffmpeg.to_string_lossy().into_owned()).chain(args.iter().cloned()).collect::<Vec<_>>());

    let mut guard = PartialFile { path: partial.clone(), committed: false };
    run_ffmpeg(toolchain, &args, &command, graph.duration, began, &mut options)?;

    let bytes = std::fs::metadata(&partial).map(|meta| meta.len()).unwrap_or(0);
    if bytes == 0 {
        return Err(RenderError::FfmpegFailed { exit_code: Some(0), stderr: "ffmpeg wrote no output".into(), command });
    }
    // Last chance to honour a cancel that arrived while ffmpeg was finishing.
    if cancelled(&options) {
        return Err(RenderError::Cancelled);
    }
    std::fs::rename(&partial, &options.output)?;
    guard.committed = true;
    // `finished` is reported only now that the file is in place.
    if let Some(callback) = options.on_progress.as_mut() {
        callback(&Progress {
            ratio: 1.0,
            out_time_seconds: graph.duration.secs(),
            elapsed_seconds: began.elapsed().as_secs_f64(),
            frame: None,
            speed: None,
            finished: true,
        });
    }

    Ok(ExportResult {
        output: options.output.clone(),
        start_seconds: graph.start,
        duration_seconds: graph.duration,
        width: graph.width,
        height: graph.height,
        fps: graph.fps,
        bytes,
        sources: graph.sources.clone(),
        caption_clip_ids: graph.caption_clip_ids.clone(),
        warnings: graph.warnings.clone(),
        ffmpeg_command: command,
        ffmpeg_version: toolchain.version.clone(),
        elapsed_seconds: began.elapsed().as_secs_f64(),
    })
}

/// The arguments after the `ffmpeg` program name. H.264 + AAC in an MP4 with the
/// index at the front, as the earlier implementation wrote it.
fn export_args(graph: &RenderGraph, partial: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["-y", "-hide_banner", "-loglevel", "error", "-nostdin", "-nostats", "-progress", "pipe:1"]
        .iter()
        .map(ToString::to_string)
        .collect();
    args.extend(graph.input_args());
    args.extend(graph.graph_args());
    let tail = [
        "-t",
        &fmt_time(graph.duration),
        "-r",
        &fmt_num(graph.fps),
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-b:a",
        "160k",
        "-movflags",
        "+faststart",
        "-f",
        "mp4",
    ];
    args.extend(tail.iter().map(ToString::to_string));
    args.push(partial.to_string_lossy().into_owned());
    args
}

enum Event {
    Progress(Progress),
}

fn run_ffmpeg(toolchain: &Toolchain, args: &[String], command: &str, duration: Time, began: Instant, options: &mut ExportOptions) -> Result<()> {
    let mut child = Command::new(&toolchain.ffmpeg)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| RenderError::ToolchainMissing(format!("cannot start ffmpeg: {error}")))?;

    let (sender, receiver) = mpsc::channel::<Event>();
    let total = duration.secs().max(1e-9);
    let stdout = child.stdout.take().expect("piped");
    let progress_reader = thread::spawn(move || {
        let mut packet = ProgressPacket::default();
        for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
            if let Some(progress) = packet.feed(&line, total, began) {
                if sender.send(Event::Progress(progress)).is_err() {
                    break;
                }
            }
        }
    });
    let tail = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_reader = spawn_tail_reader(child.stderr.take().expect("piped"), Arc::clone(&tail));

    let mut terminated_at: Option<Instant> = None;
    let mut killed = false;
    let status = loop {
        match receiver.recv_timeout(POLL) {
            Ok(Event::Progress(progress)) => {
                if let Some(callback) = options.on_progress.as_mut() {
                    callback(&progress);
                }
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if options.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
            match terminated_at {
                None => {
                    terminate(&mut child);
                    terminated_at = Some(Instant::now());
                }
                Some(at) if !killed && at.elapsed() >= TERMINATE_GRACE => {
                    let _ = child.kill();
                    killed = true;
                }
                Some(_) => {}
            }
        }
    };
    let _ = progress_reader.join();
    let _ = stderr_reader.join();
    while let Ok(Event::Progress(progress)) = receiver.try_recv() {
        if let Some(callback) = options.on_progress.as_mut() {
            callback(&progress);
        }
    }

    if options.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
        return Err(RenderError::Cancelled);
    }
    if !status.success() {
        let stderr = String::from_utf8_lossy(&tail.lock().map(|buffer| buffer.clone()).unwrap_or_default()).into_owned();
        return Err(RenderError::FfmpegFailed { exit_code: status.code(), stderr, command: command.to_string() });
    }
    Ok(())
}

/// Keeps the last [`STDERR_TAIL_BYTES`] of a stream so a failure can quote it while
/// the pipe never fills up.
pub(crate) fn spawn_tail_reader(mut stream: impl Read + Send + 'static, tail: Arc<Mutex<Vec<u8>>>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(read) = stream.read(&mut chunk) {
            if read == 0 {
                break;
            }
            if let Ok(mut buffer) = tail.lock() {
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.len() > STDERR_TAIL_BYTES {
                    let excess = buffer.len() - STDERR_TAIL_BYTES;
                    buffer.drain(..excess);
                }
            }
        }
    })
}

#[cfg(unix)]
fn terminate(child: &mut Child) {
    // SAFETY: `kill` with a pid we own (the child has not been waited on yet).
    let sent = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) } == 0;
    if !sent {
        let _ = child.kill();
    }
}

#[cfg(not(unix))]
fn terminate(child: &mut Child) {
    let _ = child.kill();
}

/// Collects ffmpeg's `key=value` progress lines until a `progress=` line closes a packet.
#[derive(Default)]
struct ProgressPacket {
    out_time_us: Option<f64>,
    frame: Option<u64>,
    speed: Option<String>,
}

impl ProgressPacket {
    fn feed(&mut self, line: &str, total_seconds: f64, began: Instant) -> Option<Progress> {
        let (key, value) = line.split_once('=')?;
        let value = value.trim();
        match key.trim() {
            "out_time_us" | "out_time_ms" => self.out_time_us = value.parse::<f64>().ok().or(self.out_time_us),
            "frame" => self.frame = value.parse().ok(),
            "speed" => self.speed = Some(value.to_string()).filter(|speed| speed != "N/A"),
            "progress" => {
                let out_seconds = self.out_time_us.unwrap_or(0.0).max(0.0) / 1_000_000.0;
                let finished = value == "end";
                let packet = std::mem::take(self);
                return Some(Progress {
                    ratio: if finished { 1.0 } else { (out_seconds / total_seconds).clamp(0.0, 1.0) },
                    out_time_seconds: out_seconds,
                    elapsed_seconds: began.elapsed().as_secs_f64(),
                    frame: packet.frame,
                    speed: packet.speed,
                    // `finished` is set by `export` after the file is in place;
                    // ffmpeg's own `end` only reports ratio 1.
                    finished: false,
                });
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_packets_close_on_progress_line() {
        let began = Instant::now();
        let mut packet = ProgressPacket::default();
        assert!(packet.feed("frame=30", 10.0, began).is_none());
        assert!(packet.feed("out_time_us=2500000", 10.0, began).is_none());
        assert!(packet.feed("speed=1.5x", 10.0, began).is_none());
        let progress = packet.feed("progress=continue", 10.0, began).expect("packet");
        assert_eq!(progress.frame, Some(30));
        assert!((progress.ratio - 0.25).abs() < 1e-9);
        assert_eq!(progress.speed.as_deref(), Some("1.5x"));
        let progress = packet.feed("progress=end", 10.0, began).expect("packet");
        assert_eq!(progress.ratio, 1.0);
    }
}
