//! Thumbnails (a filmstrip per media item) and audio waveforms.
//!
//! Computed by ffmpeg on background threads and stored in a hidden cache folder
//! inside the library (`<library>/.cache/avs-app/`), so the second start is instant.
//! Drawing never waits: a missing strip or waveform is simply not drawn yet.

use avs_core::{MediaAsset, MediaType};
use eframe::egui;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

pub const STRIP_W: usize = 128;
pub const STRIP_H: usize = 72;
const MAX_STRIP_FRAMES: usize = 48;
/// Waveform peaks per second of media.
pub const PEAKS_PER_SECOND: u32 = 100;
const WAVE_RATE: u32 = 8000;
const CACHE_VERSION: &str = "v1";

pub struct Strip {
    pub frames: Vec<egui::TextureHandle>,
    /// Media seconds between consecutive frames (0 for a still).
    pub interval: f64,
}

pub enum Entry<T> {
    Pending,
    Ready(T),
    Failed(String),
}

enum Job {
    Strip { media_id: String, path: PathBuf, duration: f64, still: bool, cache: PathBuf },
    Wave { media_id: String, path: PathBuf, cache: PathBuf },
}

enum Done {
    Strip { media_id: String, result: Result<(Vec<Vec<u8>>, f64), String> },
    Wave { media_id: String, result: Result<Vec<u8>, String> },
}

pub struct MediaCache {
    cache_dir: PathBuf,
    media_root: PathBuf,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    pub strips: HashMap<String, Entry<Strip>>,
    pub waves: HashMap<String, Entry<Arc<Vec<u8>>>>,
}

impl MediaCache {
    pub fn new(data_directory: &Path, media_root: &Path) -> MediaCache {
        let (jobs, job_receiver) = mpsc::channel::<Job>();
        let (done_sender, done) = mpsc::channel::<Done>();
        let job_receiver = Arc::new(Mutex::new(job_receiver));
        for _ in 0..2 {
            let receiver = Arc::clone(&job_receiver);
            let sender = done_sender.clone();
            std::thread::spawn(move || {
                loop {
                    let job = match receiver.lock() {
                        Ok(guard) => guard.recv(),
                        Err(_) => return,
                    };
                    let Ok(job) = job else { return };
                    let result = match job {
                        Job::Strip { media_id, path, duration, still, cache } => Done::Strip { media_id, result: strip_job(&path, duration, still, &cache) },
                        Job::Wave { media_id, path, cache } => Done::Wave { media_id, result: wave_job(&path, &cache) },
                    };
                    if sender.send(result).is_err() {
                        return;
                    }
                }
            });
        }
        MediaCache { cache_dir: data_directory.join(".cache").join("avs-app"), media_root: media_root.to_path_buf(), jobs, done, strips: HashMap::new(), waves: HashMap::new() }
    }

    fn path_of(&self, media: &MediaAsset) -> Result<PathBuf, String> {
        let source = media.source.as_deref().ok_or_else(|| "no source".to_string())?;
        avs_session::resolve_media_source(&self.media_root, source).map_err(|error| error.message)
    }

    fn cache_file(&self, kind: &str, media: &MediaAsset) -> PathBuf {
        let safe: String = media.id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        self.cache_dir.join(format!("{kind}-{safe}-{CACHE_VERSION}.bin"))
    }

    /// Asks for the filmstrip of a picture media item (once).
    pub fn want_strip(&mut self, media: &MediaAsset) {
        if self.strips.contains_key(&media.id) || media.media_type == MediaType::Audio {
            return;
        }
        let cache = self.cache_file("strip", media);
        match self.path_of(media) {
            Ok(path) => {
                let job = Job::Strip { media_id: media.id.clone(), path, duration: media.duration_seconds.secs(), still: media.media_type.is_still(), cache };
                let _ = self.jobs.send(job);
                self.strips.insert(media.id.clone(), Entry::Pending);
            }
            Err(reason) => {
                self.strips.insert(media.id.clone(), Entry::Failed(reason));
            }
        }
    }

    /// Asks for the waveform of a media item with sound (once).
    pub fn want_wave(&mut self, media: &MediaAsset) {
        if self.waves.contains_key(&media.id) || media.media_type.is_still() {
            return;
        }
        let cache = self.cache_file("wave", media);
        match self.path_of(media) {
            Ok(path) => {
                let _ = self.jobs.send(Job::Wave { media_id: media.id.clone(), path, cache });
                self.waves.insert(media.id.clone(), Entry::Pending);
            }
            Err(reason) => {
                self.waves.insert(media.id.clone(), Entry::Failed(reason));
            }
        }
    }

    /// Takes finished work and turns frames into textures. Returns true when
    /// anything arrived (so the caller repaints).
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut any = false;
        while let Ok(done) = self.done.try_recv() {
            any = true;
            match done {
                Done::Strip { media_id, result } => {
                    let entry = match result {
                        Ok((frames, interval)) => Entry::Ready(Strip {
                            frames: frames
                                .iter()
                                .enumerate()
                                .map(|(index, rgba)| {
                                    let image = egui::ColorImage::from_rgba_unmultiplied([STRIP_W, STRIP_H], rgba);
                                    ctx.load_texture(format!("strip-{media_id}-{index}"), image, egui::TextureOptions::LINEAR)
                                })
                                .collect(),
                            interval,
                        }),
                        Err(reason) => Entry::Failed(reason),
                    };
                    self.strips.insert(media_id, entry);
                }
                Done::Wave { media_id, result } => {
                    let entry = match result {
                        Ok(peaks) => Entry::Ready(Arc::new(peaks)),
                        Err(reason) => Entry::Failed(reason),
                    };
                    self.waves.insert(media_id, entry);
                }
            }
        }
        any
    }

    pub fn pending(&self) -> bool {
        self.strips.values().any(|entry| matches!(entry, Entry::Pending)) || self.waves.values().any(|entry| matches!(entry, Entry::Pending))
    }

    /// The filmstrip frame closest to a media time.
    pub fn frame_at(&self, media_id: &str, media_seconds: f64) -> Option<&egui::TextureHandle> {
        let Some(Entry::Ready(strip)) = self.strips.get(media_id) else { return None };
        if strip.frames.is_empty() {
            return None;
        }
        let index = if strip.interval > 0.0 { (media_seconds / strip.interval).floor().max(0.0) as usize } else { 0 };
        strip.frames.get(index.min(strip.frames.len() - 1))
    }

    pub fn wave(&self, media_id: &str) -> Option<Arc<Vec<u8>>> {
        match self.waves.get(media_id) {
            Some(Entry::Ready(peaks)) => Some(Arc::clone(peaks)),
            _ => None,
        }
    }
}

fn ffmpeg() -> Result<PathBuf, String> {
    avs_render::Toolchain::get().map(|toolchain| toolchain.ffmpeg.clone()).map_err(|error| error.to_string())
}

fn run_ffmpeg(args: &[String]) -> Result<Vec<u8>, String> {
    let mut child = Command::new(ffmpeg()?)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot start ffmpeg: {error}"))?;
    let mut bytes = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    }
    let status = child.wait().map_err(|error| error.to_string())?;
    if !status.success() && bytes.is_empty() {
        return Err(format!("ffmpeg exited with {status}"));
    }
    Ok(bytes)
}

fn write_cache(path: &Path, header: &[u8], body: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let temp = path.with_extension(format!("tmp{}", std::process::id()));
    let written = std::fs::File::create(&temp).and_then(|mut file| {
        file.write_all(header)?;
        file.write_all(body)
    });
    if written.is_ok() {
        let _ = std::fs::rename(&temp, path);
    } else {
        let _ = std::fs::remove_file(&temp);
    }
}

fn strip_job(path: &Path, duration: f64, still: bool, cache: &Path) -> Result<(Vec<Vec<u8>>, f64), String> {
    let frame_bytes = STRIP_W * STRIP_H * 4;
    if let Ok(bytes) = std::fs::read(cache)
        && bytes.len() > 16 && &bytes[..8] == b"AVSSTRIP" {
            let interval = f64::from_le_bytes(bytes[8..16].try_into().unwrap_or_default());
            let body = &bytes[16..];
            if body.len() % frame_bytes == 0 && !body.is_empty() {
                return Ok((body.chunks_exact(frame_bytes).map(<[u8]>::to_vec).collect(), interval));
            }
        }
    let count = if still { 1 } else { ((duration / 2.0).ceil() as usize).clamp(1, MAX_STRIP_FRAMES) };
    let interval = if still || duration <= 0.0 { 0.0 } else { duration / count as f64 };
    let fit = format!("scale={STRIP_W}:{STRIP_H}:force_original_aspect_ratio=decrease,pad={STRIP_W}:{STRIP_H}:(ow-iw)/2:(oh-ih)/2:color=black");
    let filter = if still { fit } else { format!("fps=fps={},{fit}", 1.0 / interval.max(1e-3)) };
    let args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-i"]
        .iter()
        .map(ToString::to_string)
        .chain([path.display().to_string(), "-an".into(), "-vf".into(), filter, "-frames:v".into(), count.to_string(), "-f".into(), "rawvideo".into(), "-pix_fmt".into(), "rgba".into(), "pipe:1".into()])
        .collect();
    let bytes = run_ffmpeg(&args)?;
    let frames: Vec<Vec<u8>> = bytes.chunks_exact(frame_bytes).map(<[u8]>::to_vec).collect();
    if frames.is_empty() {
        return Err("ffmpeg produced no picture".into());
    }
    let mut header = b"AVSSTRIP".to_vec();
    header.extend_from_slice(&interval.to_le_bytes());
    write_cache(cache, &header, &frames.concat());
    Ok((frames, interval))
}

fn wave_job(path: &Path, cache: &Path) -> Result<Vec<u8>, String> {
    if let Ok(bytes) = std::fs::read(cache)
        && bytes.len() >= 8 && &bytes[..8] == b"AVSWAVE1" {
            return Ok(bytes[8..].to_vec());
        }
    let args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-i"]
        .iter()
        .map(ToString::to_string)
        .chain([path.display().to_string(), "-vn".into(), "-ac".into(), "1".into(), "-ar".into(), WAVE_RATE.to_string(), "-f".into(), "f32le".into(), "pipe:1".into()])
        .collect();
    let bytes = run_ffmpeg(&args)?;
    let samples: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    let peaks = peaks(&samples, (WAVE_RATE / PEAKS_PER_SECOND) as usize);
    write_cache(cache, b"AVSWAVE1", &peaks);
    Ok(peaks)
}

/// Peak level of each bucket of samples, 0-255 on a square-root scale so quiet
/// speech is still visible.
pub fn peaks(samples: &[f32], bucket: usize) -> Vec<u8> {
    samples
        .chunks(bucket.max(1))
        .map(|chunk| {
            let peak = chunk.iter().fold(0.0f32, |max, sample| max.max(sample.abs())).min(1.0);
            (peak.sqrt() * 255.0).round() as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn peaks_are_bucketed_and_scaled() {
        let samples = [0.0, 0.25, -1.0, 0.0, 0.0, 0.0];
        assert_eq!(super::peaks(&samples, 3), vec![255, 0]);
        assert_eq!(super::peaks(&[0.25], 1), vec![128]);
    }
}
