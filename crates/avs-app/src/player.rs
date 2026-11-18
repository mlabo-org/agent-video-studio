//! The viewer: playback through `avs_render::PreviewStream` and single frames for
//! scrubbing through `avs_render::render_frame`.
//!
//! Playback at normal speed is clocked by the sound card: the picture shown is the
//! newest decoded frame whose time is not after the samples already played, so the
//! two cannot drift. Without an output device, or at other speeds, a wall clock
//! is used. Reverse play repeats single-frame renders backwards (the decoder only
//! runs forwards).

use avs_core::{Project, Time};
use avs_render::{Frame, PreviewOptions, PreviewStream};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Samples (interleaved stereo) buffered ahead of the sound card.
const AUDIO_HIGH_WATER_SECONDS: f64 = 0.4;

struct AudioShared {
    /// Interleaved stereo samples.
    pcm: Mutex<VecDeque<f32>>,
    /// Stereo frames handed to the device since the last restart.
    played: AtomicU64,
    playing: AtomicBool,
    /// Peak of the last callback, as f32 bits (for the level meter).
    level: AtomicU32,
}

struct AudioOut {
    _stream: cpal::Stream,
    shared: Arc<AudioShared>,
    rate: u32,
}

fn open_audio() -> Result<AudioOut, String> {
    let device = cpal::default_host().default_output_device().ok_or("no sound output device")?;
    let supported = device.default_output_config().map_err(|error| error.to_string())?;
    if supported.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!("the output device wants {:?} samples", supported.sample_format()));
    }
    let config: cpal::StreamConfig = supported.into();
    let rate = config.sample_rate;
    let device_channels = usize::from(config.channels.max(1));
    let shared = Arc::new(AudioShared {
        pcm: Mutex::new(VecDeque::new()),
        played: AtomicU64::new(0),
        playing: AtomicBool::new(false),
        level: AtomicU32::new(0),
    });
    let callback_shared = Arc::clone(&shared);
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                data.fill(0.0);
                if !callback_shared.playing.load(Ordering::Relaxed) {
                    callback_shared.level.store(0, Ordering::Relaxed);
                    return;
                }
                let Ok(mut pcm) = callback_shared.pcm.lock() else { return };
                let mut frames = 0u64;
                let mut peak = 0.0f32;
                for frame in data.chunks_mut(device_channels) {
                    if pcm.len() < 2 {
                        break;
                    }
                    let left = pcm.pop_front().unwrap_or(0.0);
                    let right = pcm.pop_front().unwrap_or(0.0);
                    peak = peak.max(left.abs()).max(right.abs());
                    match frame.len() {
                        1 => frame[0] = (left + right) * 0.5,
                        _ => {
                            frame[0] = left;
                            frame[1] = right;
                        }
                    }
                    frames += 1;
                }
                callback_shared.played.fetch_add(frames, Ordering::Relaxed);
                callback_shared.level.store(peak.to_bits(), Ordering::Relaxed);
            },
            |error| eprintln!("audio output error: {error}"),
            None,
        )
        .map_err(|error| error.to_string())?;
    stream.play().map_err(|error| error.to_string())?;
    Ok(AudioOut { _stream: stream, shared, rate })
}

/// One running playback: the decoder is owned by its audio thread, which drops it
/// (stopping ffmpeg) when the generation changes.
struct Run {
    from: Time,
    rate: f64,
    wall_start: Instant,
    audio_clock: bool,
    video: Receiver<Frame>,
    pending: Option<Frame>,
    video_done: bool,
    audio_done: Arc<AtomicBool>,
}

enum Starting {
    Stream { generation: u64, from: Time, rate: f64, receiver: Receiver<Result<PreviewStream, String>> },
}

#[derive(Clone)]
struct ScrubRequest {
    id: u64,
    project: Arc<Project>,
    at: Time,
    width: u32,
    height: u32,
}

struct ScrubResult {
    id: u64,
    at: Time,
    result: Result<Vec<u8>, String>,
}

/// Renders single frames on its own thread; only the newest request is worked on.
struct ScrubWorker {
    slot: Arc<(Mutex<Option<ScrubRequest>>, Condvar)>,
    results: Receiver<ScrubResult>,
    next_id: u64,
    shown_id: u64,
    pub busy: bool,
}

impl ScrubWorker {
    fn new(media_root: PathBuf, ctx: egui::Context) -> ScrubWorker {
        let slot: Arc<(Mutex<Option<ScrubRequest>>, Condvar)> = Arc::new((Mutex::new(None), Condvar::new()));
        let (sender, results) = mpsc::channel();
        let worker_slot = Arc::clone(&slot);
        std::thread::spawn(move || {
            loop {
                let request = {
                    let (lock, ready) = &*worker_slot;
                    let Ok(mut guard) = lock.lock() else { return };
                    while guard.is_none() {
                        guard = match ready.wait(guard) {
                            Ok(guard) => guard,
                            Err(_) => return,
                        };
                    }
                    guard.take()
                };
                let Some(request) = request else { continue };
                let root = media_root.clone();
                let resolver = move |source: &str| avs_session::resolve_media_source(&root, source).map_err(|error| error.message);
                let result = avs_render::render_frame(&request.project, &resolver, request.at, request.width, request.height).map_err(|error| error.to_string());
                if sender.send(ScrubResult { id: request.id, at: request.at, result }).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        ScrubWorker { slot, results, next_id: 1, shown_id: 0, busy: false }
    }

    fn request(&mut self, project: Arc<Project>, at: Time, width: u32, height: u32) {
        let id = self.next_id;
        self.next_id += 1;
        let (lock, ready) = &*self.slot;
        if let Ok(mut guard) = lock.lock() {
            *guard = Some(ScrubRequest { id, project, at, width, height });
            ready.notify_one();
        }
        self.busy = true;
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Transport {
    Stopped,
    /// Positive: forward at that multiple; negative: reverse.
    Playing(f64),
}

pub struct Player {
    audio: Option<AudioOut>,
    pub audio_problem: Option<String>,
    media_root: PathBuf,
    run: Option<Run>,
    starting: Option<Starting>,
    generation: u64,
    /// The generation that may keep running; decoder threads stop when it moves on.
    live: Arc<AtomicU64>,
    scrub: ScrubWorker,
    /// What the viewer shows.
    pub texture: Option<egui::TextureHandle>,
    pub frame_size: [usize; 2],
    pub shown_at: Option<Time>,
    pub transport: Transport,
    /// Position while stopped, and the start of reverse play.
    position: Time,
    reverse_start: Option<(Instant, Time, f64)>,
    pub last_error: Option<String>,
    pub warnings: Vec<String>,
    scrub_done: Option<(Time, u64, [u32; 2])>,
    ctx: egui::Context,
}

impl Player {
    pub fn new(ctx: &egui::Context, media_root: PathBuf, want_audio: bool) -> Player {
        let (audio, audio_problem) = if want_audio {
            match open_audio() {
                Ok(audio) => (Some(audio), None),
                Err(problem) => (None, Some(problem)),
            }
        } else {
            (None, Some("sound output disabled".into()))
        };
        Player {
            audio,
            audio_problem,
            scrub: ScrubWorker::new(media_root.clone(), ctx.clone()),
            media_root,
            run: None,
            starting: None,
            generation: 0,
            live: Arc::new(AtomicU64::new(0)),
            texture: None,
            frame_size: [0, 0],
            shown_at: None,
            transport: Transport::Stopped,
            position: Time::ZERO,
            reverse_start: None,
            last_error: None,
            warnings: Vec::new(),
            scrub_done: None,
            ctx: ctx.clone(),
        }
    }

    pub fn set_media_root(&mut self, media_root: PathBuf) {
        self.stop();
        self.scrub = ScrubWorker::new(media_root.clone(), self.ctx.clone());
        self.media_root = media_root;
        self.scrub_done = None;
    }

    pub fn is_playing(&self) -> bool {
        matches!(self.transport, Transport::Playing(_))
    }

    pub fn level(&self) -> f32 {
        self.audio.as_ref().map_or(0.0, |audio| f32::from_bits(audio.shared.level.load(Ordering::Relaxed)))
    }

    /// The current playback time (or the stopped position).
    pub fn time(&self) -> Time {
        if let Some((started, from, rate)) = self.reverse_start {
            let elapsed = started.elapsed().as_secs_f64() * rate;
            return Time::from_secs((from.secs() + elapsed).max(0.0)).unwrap_or(Time::ZERO);
        }
        match &self.run {
            Some(run) if run.audio_clock => {
                let audio = self.audio.as_ref().expect("audio clock needs audio");
                let played = audio.shared.played.load(Ordering::Relaxed) as f64 / f64::from(audio.rate);
                run.from + Time::from_secs(played).unwrap_or(Time::ZERO)
            }
            Some(run) => run.from + Time::from_secs(run.wall_start.elapsed().as_secs_f64() * run.rate).unwrap_or(Time::ZERO),
            None => match &self.starting {
                Some(Starting::Stream { from, .. }) => *from,
                None => self.position,
            },
        }
    }

    fn bump(&mut self) -> u64 {
        self.generation += 1;
        self.live.store(self.generation, Ordering::SeqCst);
        if let Some(audio) = &self.audio {
            audio.shared.playing.store(false, Ordering::SeqCst);
            if let Ok(mut pcm) = audio.shared.pcm.lock() {
                pcm.clear();
            }
            audio.shared.played.store(0, Ordering::SeqCst);
        }
        self.generation
    }

    pub fn stop(&mut self) {
        let at = self.time();
        self.bump();
        self.run = None;
        self.starting = None;
        self.reverse_start = None;
        self.transport = Transport::Stopped;
        self.position = at;
    }

    /// Starts (or restarts) playback at `from`. `rate` 1 = normal, 2 = double, -1 = reverse.
    pub fn play(&mut self, project: &Project, from: Time, rate: f64, size: [u32; 2]) {
        self.stop();
        self.position = from;
        self.last_error = None;
        if project.duration() <= from {
            return;
        }
        self.transport = Transport::Playing(rate);
        if rate < 0.0 {
            self.reverse_start = Some((Instant::now(), from, rate));
            return;
        }
        let generation = self.generation;
        let channels = 2;
        let sample_rate = self.audio.as_ref().map_or(project.sample_rate.max(8000), |audio| audio.rate);
        let options = PreviewOptions { from, width: size[0], height: size[1], fps: project.fps, sample_rate, channels };
        let project = project.clone();
        let root = self.media_root.clone();
        let (sender, receiver) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let resolver = move |source: &str| avs_session::resolve_media_source(&root, source).map_err(|error| error.message);
            let result = PreviewStream::start(&project, &resolver, options).map_err(|error| error.to_string());
            let _ = sender.send(result);
            ctx.request_repaint();
        });
        self.starting = Some(Starting::Stream { generation, from, rate, receiver });
    }

    fn begin_run(&mut self, generation: u64, from: Time, rate: f64, mut stream: PreviewStream) {
        self.warnings = stream.warnings.iter().map(|warning| warning.message.clone()).collect();
        let audio_clock = self.audio.is_some() && (rate - 1.0).abs() < 1e-9;
        let (_unused, placeholder) = mpsc::sync_channel::<Frame>(0);
        let video = std::mem::replace(&mut stream.video, placeholder);
        let audio_done = Arc::new(AtomicBool::new(false));
        let done_flag = Arc::clone(&audio_done);
        let shared = self.audio.as_ref().map(|audio| Arc::clone(&audio.shared));
        let live = Arc::clone(&self.live);
        let high_water = (f64::from(self.audio.as_ref().map_or(48_000, |audio| audio.rate)) * 2.0 * AUDIO_HIGH_WATER_SECONDS) as usize;
        std::thread::spawn(move || {
            let mut buffer = vec![0f32; 4096];
            let current = || live.load(Ordering::SeqCst) == generation;
            loop {
                if !current() {
                    return;
                }
                let read = match stream.audio.read_samples(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                let Some(shared) = shared.as_ref().filter(|_| audio_clock) else { continue };
                loop {
                    if !current() {
                        return;
                    }
                    if shared.pcm.lock().map_or(0, |pcm| pcm.len()) < high_water {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(4));
                }
                if let Ok(mut pcm) = shared.pcm.lock() {
                    // Checked under the lock: a stop clears the queue under the same lock.
                    if !current() {
                        return;
                    }
                    pcm.extend(&buffer[..read]);
                }
                shared.playing.store(true, Ordering::SeqCst);
            }
            done_flag.store(true, Ordering::SeqCst);
            // Keep the decoder alive until playback moves on, then drop it.
            while current() {
                std::thread::sleep(Duration::from_millis(50));
            }
            drop(stream);
        });
        self.run = Some(Run { from, rate, wall_start: Instant::now(), audio_clock, video, pending: None, video_done: false, audio_done });
    }

    /// Advances playback: picks up a started stream, shows due frames and stops at
    /// the end. Returns the playback time while playing.
    pub fn tick(&mut self, project: &Project, revision: u64, size: [u32; 2]) -> Option<Time> {
        self.poll_scrub();
        if let Some(Starting::Stream { generation, from, rate, receiver }) = &self.starting {
            match receiver.try_recv() {
                Ok(Ok(stream)) => {
                    let (generation, from, rate) = (*generation, *from, *rate);
                    self.starting = None;
                    if generation == self.generation {
                        self.begin_run(generation, from, rate, stream);
                    }
                }
                Ok(Err(error)) => {
                    self.starting = None;
                    self.last_error = Some(error);
                    self.transport = Transport::Stopped;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.starting = None,
            }
        }
        if let Some((_, _, _)) = self.reverse_start {
            let at = self.time();
            if at <= Time::ZERO {
                self.stop();
                self.position = Time::ZERO;
            }
            self.request_frame(project, revision, self.time(), size, true);
            return Some(self.time());
        }
        let clock = self.time();
        let mut due: Option<Frame> = None;
        if let Some(run) = &mut self.run {
            let half_frame = Time::from_secs(0.5 / project.fps.max(1.0)).unwrap_or(Time::ZERO);
            loop {
                if run.pending.is_none() {
                    match run.video.try_recv() {
                        Ok(frame) => run.pending = Some(frame),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            run.video_done = true;
                            break;
                        }
                    }
                }
                if run.pending.as_ref().is_some_and(|frame| frame.pts <= clock + half_frame) {
                    due = run.pending.take();
                } else {
                    break;
                }
            }
        }
        if let Some(frame) = due {
            self.show(frame.pts, &frame.rgba, size);
        }
        let ended = self.run.as_ref().is_some_and(|run| {
            let audio_drained = !run.audio_clock || (run.audio_done.load(Ordering::SeqCst) && self.audio.as_ref().is_none_or(|audio| audio.shared.pcm.lock().map_or(true, |pcm| pcm.is_empty())));
            run.video_done && run.pending.is_none() && audio_drained
        }) || (self.run.is_some() && clock >= project.duration());
        if ended {
            self.stop();
            self.position = self.position.min(project.duration());
            return None;
        }
        self.is_playing().then_some(clock)
    }

    fn show(&mut self, at: Time, rgba: &[u8], size: [u32; 2]) {
        let [width, height] = [size[0] as usize, size[1] as usize];
        if rgba.len() != width * height * 4 {
            return;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([width, height], rgba);
        match &mut self.texture {
            Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
            None => self.texture = Some(self.ctx.load_texture("viewer", image, egui::TextureOptions::LINEAR)),
        }
        self.frame_size = [width, height];
        self.shown_at = Some(at);
    }

    fn poll_scrub(&mut self) {
        while let Ok(result) = self.scrub.results.try_recv() {
            if result.id < self.scrub.shown_id {
                continue;
            }
            self.scrub.shown_id = result.id;
            self.scrub.busy = result.id + 1 < self.scrub.next_id;
            match result.result {
                Ok(rgba) => {
                    if let Some((_, _, size)) = self.scrub_done {
                        self.show(result.at, &rgba, size);
                    }
                    self.last_error = None;
                }
                Err(error) => self.last_error = Some(error),
            }
        }
    }

    /// While stopped: shows the picture at `at`, rendering it when it is not the
    /// frame already shown for this revision and size.
    pub fn request_frame(&mut self, project: &Project, revision: u64, at: Time, size: [u32; 2], force: bool) {
        if self.is_playing() && self.reverse_start.is_none() {
            return;
        }
        if !force && self.scrub_done == Some((at, revision, size)) {
            return;
        }
        if self.reverse_start.is_some() && self.scrub.busy {
            return;
        }
        if project.duration() == Time::ZERO {
            self.texture = None;
            self.scrub_done = Some((at, revision, size));
            return;
        }
        let at = at.min(project.duration() - Time::from_micros(1)).max(Time::ZERO);
        self.scrub_done = Some((at, revision, size));
        self.scrub.request(Arc::new(project.clone()), at, size[0], size[1]);
    }

    pub fn scrub_busy(&self) -> bool {
        self.scrub.busy || self.starting.is_some()
    }
}

/// Viewer render size: the project aspect inside `available` physical pixels,
/// capped so decoding stays cheap; both sides even.
pub fn render_size(project: &Project, available: egui::Vec2, pixels_per_point: f32) -> [u32; 2] {
    let (pw, ph) = (f64::from(project.resolution.width.max(2)), f64::from(project.resolution.height.max(2)));
    let max_w = (f64::from(available.x * pixels_per_point)).clamp(160.0, 1280.0);
    let max_h = (f64::from(available.y * pixels_per_point)).clamp(90.0, 720.0);
    let scale = (max_w / pw).min(max_h / ph).min(1.0);
    let even = |value: f64| ((value / 2.0).round() as u32 * 2).max(2);
    [even(pw * scale), even(ph * scale)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_size_keeps_aspect_and_is_even() {
        let project = Project::empty("p", "t");
        let size = render_size(&project, egui::vec2(500.0, 500.0), 2.0);
        assert_eq!(size, [1000, 562]);
        let small = render_size(&project, egui::vec2(10.0, 10.0), 1.0);
        assert_eq!(small[0] % 2, 0);
    }
}
