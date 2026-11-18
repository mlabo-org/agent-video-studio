//! Live preview: ffmpeg decodes the same composition graph that the export uses and
//! hands raw RGBA frames and raw PCM to the caller through pipes.
//!
//! Two ffmpeg processes run, one for the picture and one for the sound, both built
//! by [`RenderGraph::build`] from the same project and start time. Playback speed is
//! the caller's: it reads audio at the device rate and pulls the frame whose
//! timestamp matches the audio clock.

use crate::error::{RenderError, Result};
use crate::export::spawn_tail_reader;
use crate::fmt::{fmt_num, fmt_time};
use crate::graph::{GraphOptions, RenderGraph, Streams, VideoFormat};
use crate::media::MediaResolver;
use crate::tool::{HELPER_TIMEOUT, Toolchain, run_capture};
use avs_core::{Project, Time};
use std::io::{self, Read};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Frames buffered ahead of the consumer. The pipe and ffmpeg stall behind it, so
/// decoding never runs far ahead of playback.
const FRAME_QUEUE: usize = 3;
const FRAME_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct PreviewOptions {
    /// Timeline position where playback starts.
    pub from: Time,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub sample_rate: u32,
    /// 1 or 2.
    pub channels: u32,
}

#[derive(Debug, Clone)]
pub struct Frame {
    /// Timeline position of this frame.
    pub pts: Time,
    /// `width * height * 4` bytes, straight RGBA.
    pub rgba: Vec<u8>,
}

/// Interleaved 32-bit little-endian float samples (`f32le`), at the rate and channel
/// count requested in [`PreviewOptions`]. Reading blocks until ffmpeg has data and
/// returns 0 at the end of the timeline.
pub struct AudioReader {
    stdout: ChildStdout,
}

impl Read for AudioReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl AudioReader {
    /// Fills `out` with as many samples as are available (blocking for at least one
    /// while the stream is open) and returns how many were written.
    pub fn read_samples(&mut self, out: &mut [f32]) -> io::Result<usize> {
        let mut bytes = vec![0u8; out.len() * 4];
        let mut filled = 0;
        while filled < 4 {
            let read = self.stdout.read(&mut bytes[filled..])?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        // A sample may straddle two reads; finish the partial one.
        while filled % 4 != 0 {
            let read = self.stdout.read(&mut bytes[filled..filled + (4 - filled % 4)])?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        let samples = filled / 4;
        for (slot, chunk) in out.iter_mut().zip(bytes[..samples * 4].chunks_exact(4)) {
            *slot = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        Ok(samples)
    }
}

pub struct PreviewStream {
    /// Frames in timeline order; `pts` advances by one frame period.
    pub video: Receiver<Frame>,
    pub audio: AudioReader,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub sample_rate: u32,
    pub channels: u32,
    pub warnings: Vec<crate::graph::RenderWarning>,
    /// Stops both ffmpeg processes when the stream (or whatever is left of it after
    /// `video` or `audio` was moved out to another thread) is dropped.
    guard: PreviewGuard,
}

/// Owns the two ffmpeg processes and the caption images they read. Kept apart from
/// [`PreviewStream`] so that stream has no `Drop` of its own and `video` and `audio`
/// can be moved to different threads.
struct PreviewGuard {
    video_child: Child,
    audio_child: Child,
    video_diagnostics: Arc<Mutex<Vec<u8>>>,
    audio_diagnostics: Arc<Mutex<Vec<u8>>>,
    _graphs: (RenderGraph, RenderGraph),
}

impl Drop for PreviewGuard {
    fn drop(&mut self) {
        reap(&mut self.video_child);
        reap(&mut self.audio_child);
    }
}

impl PreviewStream {
    pub fn start(project: &Project, resolver: &dyn MediaResolver, options: PreviewOptions) -> Result<PreviewStream> {
        let base = GraphOptions {
            start: options.from,
            duration: None,
            width: options.width,
            height: options.height,
            fps: options.fps,
            sample_rate: options.sample_rate,
            channels: options.channels,
            streams: Streams::VideoOnly,
            video_format: VideoFormat::Rgba,
        };
        let video_graph = RenderGraph::build(project, resolver, &base)?;
        let audio_graph = RenderGraph::build(project, resolver, &GraphOptions { streams: Streams::AudioOnly, ..base })?;
        let toolchain = Toolchain::get()?;

        let mut video_args = common_args();
        video_args.extend(video_graph.input_args());
        video_args.extend(video_graph.graph_args());
        video_args.extend(["-r", &fmt_num(options.fps), "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"].map(String::from));

        let mut audio_args = common_args();
        audio_args.extend(audio_graph.input_args());
        audio_args.extend(audio_graph.graph_args());
        audio_args.extend(["-f", "f32le", "-acodec", "pcm_f32le", "-ar", &options.sample_rate.to_string(), "-ac", &options.channels.to_string(), "pipe:1"].map(String::from));

        let mut video_child = spawn(toolchain, &video_args)?;
        let mut audio_child = match spawn(toolchain, &audio_args) {
            Ok(child) => child,
            Err(error) => {
                reap(&mut video_child);
                return Err(error);
            }
        };

        let video_diagnostics = Arc::new(Mutex::new(Vec::new()));
        let audio_diagnostics = Arc::new(Mutex::new(Vec::new()));
        spawn_tail_reader(video_child.stderr.take().expect("piped"), Arc::clone(&video_diagnostics));
        spawn_tail_reader(audio_child.stderr.take().expect("piped"), Arc::clone(&audio_diagnostics));

        let (sender, video) = mpsc::sync_channel::<Frame>(FRAME_QUEUE);
        let mut stdout = video_child.stdout.take().expect("piped");
        let frame_bytes = options.width as usize * options.height as usize * 4;
        let (from, fps) = (options.from, options.fps);
        // Not joined on drop: it may be blocked in `send` until the receiver goes away.
        thread::spawn(move || {
            for index in 0u64.. {
                let mut rgba = vec![0u8; frame_bytes];
                if stdout.read_exact(&mut rgba).is_err() {
                    break;
                }
                let pts = from + Time::from_micros((index as f64 * 1_000_000.0 / fps).round() as i64);
                if sender.send(Frame { pts, rgba }).is_err() {
                    break;
                }
            }
        });

        let warnings = video_graph.warnings.iter().chain(audio_graph.warnings.iter()).cloned().fold(Vec::new(), |mut all, warning| {
            if !all.contains(&warning) {
                all.push(warning);
            }
            all
        });
        let audio = AudioReader { stdout: audio_child.stdout.take().expect("piped") };
        Ok(PreviewStream {
            video,
            audio,
            width: options.width,
            height: options.height,
            fps: options.fps,
            sample_rate: options.sample_rate,
            channels: options.channels,
            warnings,
            guard: PreviewGuard { video_child, audio_child, video_diagnostics, audio_diagnostics, _graphs: (video_graph, audio_graph) },
        })
    }

    /// What ffmpeg printed to stderr so far (most recent part), for explaining a
    /// stream that ended early. Empty when ffmpeg had nothing to complain about.
    pub fn diagnostics(&self) -> String {
        let text = |buffer: &Arc<Mutex<Vec<u8>>>| String::from_utf8_lossy(&buffer.lock().map(|b| b.clone()).unwrap_or_default()).trim().to_string();
        [text(&self.guard.video_diagnostics), text(&self.guard.audio_diagnostics)].iter().filter(|part| !part.is_empty()).cloned().collect::<Vec<_>>().join("\n")
    }
}

fn common_args() -> Vec<String> {
    ["-hide_banner", "-loglevel", "error", "-nostdin", "-nostats"].iter().map(ToString::to_string).collect()
}

fn spawn(toolchain: &Toolchain, args: &[String]) -> Result<Child> {
    Command::new(&toolchain.ffmpeg)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| RenderError::ToolchainMissing(format!("cannot start ffmpeg: {error}")))
}

fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// One picture of the timeline at `at`, for scrubbing while stopped. Returns
/// `width * height * 4` bytes of straight RGBA.
pub fn render_frame(project: &Project, resolver: &dyn MediaResolver, at: Time, width: u32, height: u32) -> Result<Vec<u8>> {
    let fps = project.fps;
    let options = GraphOptions {
        start: at,
        // One frame period, rounded up so the graph always holds a frame.
        duration: Some(Time::from_micros((1_000_000.0 / fps).ceil() as i64).max(Time::from_micros(1))),
        width,
        height,
        fps,
        sample_rate: project.sample_rate,
        channels: 2,
        streams: Streams::VideoOnly,
        video_format: VideoFormat::Rgba,
    };
    let graph = RenderGraph::build(project, resolver, &options)?;
    let toolchain = Toolchain::get()?;
    let mut args = common_args();
    args.extend(graph.input_args());
    args.extend(graph.graph_args());
    args.extend(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"].map(String::from));
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run_capture(&toolchain.ffmpeg, &argv, FRAME_TIMEOUT.max(HELPER_TIMEOUT))?;
    let expected = width as usize * height as usize * 4;
    if !output.success || output.stdout.len() != expected {
        return Err(RenderError::FfmpegFailed {
            exit_code: output.exit_code,
            stderr: format!(
                "{}\n(expected {expected} bytes of picture, got {}; graph window {}s from {}s)",
                String::from_utf8_lossy(&output.stderr).trim(),
                output.stdout.len(),
                fmt_time(graph.duration),
                fmt_time(graph.start)
            ),
            command: crate::fmt::shell_join(&args),
        });
    }
    Ok(output.stdout)
}
