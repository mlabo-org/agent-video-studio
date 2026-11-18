//! Narration recording: one channel of the default input device is captured with
//! cpal, written as a mono 16-bit WAV file and imported like any other file,
//! placed at the playhead position where recording started.
//!
//! An audio interface can deliver many channels at a high rate (16 channels at
//! 192 kHz is real); a narration is one voice, so only the chosen channel is kept
//! and rates above 48 kHz are reduced. The device is listened to from the moment
//! the window opens, so the meters show which channel the microphone is on.

use crate::app::{AvsApp, StatusKind};
use avs_core::{Time, TrackKind};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// State shared with the audio callback.
struct Shared {
    /// Zero-based input channel that is recorded.
    channel: AtomicUsize,
    recording: AtomicBool,
    /// Mono samples at `Recorder::rate`.
    samples: Mutex<Vec<i16>>,
    /// Peak of each input channel in the latest block, for the meters.
    peaks: Mutex<Vec<f32>>,
}

pub struct Recorder {
    stream: Option<cpal::Stream>,
    shared: Arc<Shared>,
    /// Sample rate of the recording (the device rate, reduced to at most 48 kHz
    /// when it is a whole multiple of 44.1 or 48 kHz).
    pub rate: u32,
    /// Input channels of the device.
    pub channels: u16,
    pub device_name: String,
    /// Timeline position the recording will be placed at.
    pub at: Time,
    pub error: Option<String>,
}

/// How many device samples become one recorded sample: 192 kHz → 4, 96 → 2,
/// 88.2 → 2, anything else → 1.
pub fn reduction(device_rate: u32) -> u32 {
    [48_000, 44_100].into_iter().find(|base| device_rate > *base && device_rate.is_multiple_of(*base)).map_or(1, |base| device_rate / base)
}

/// Picks `channel` out of interleaved frames and averages every `factor` of its
/// samples into one. `carry` holds an unfinished group between blocks.
pub fn take_channel(data: &[f32], channels: usize, channel: usize, factor: u32, carry: &mut (f32, u32), out: &mut Vec<i16>) {
    for frame in data.chunks_exact(channels.max(1)) {
        carry.0 += frame.get(channel).copied().unwrap_or(0.0);
        carry.1 += 1;
        if carry.1 >= factor {
            let value = carry.0 / carry.1 as f32;
            out.push((value.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16);
            *carry = (0.0, 0);
        }
    }
}

impl Recorder {
    /// Opens the default input device and starts listening (meters only).
    pub fn open(at: Time, channel: usize) -> Recorder {
        let shared = Arc::new(Shared { channel: AtomicUsize::new(channel), recording: AtomicBool::new(false), samples: Mutex::new(Vec::new()), peaks: Mutex::new(Vec::new()) });
        let mut recorder = Recorder { stream: None, shared, rate: 48_000, channels: 1, device_name: String::new(), at, error: None };
        recorder.error = recorder.listen().err();
        recorder
    }

    fn listen(&mut self) -> Result<(), String> {
        let device = cpal::default_host().default_input_device().ok_or("no microphone (input device) found")?;
        let supported = device.default_input_config().map_err(|error| error.to_string())?;
        if supported.sample_format() != cpal::SampleFormat::F32 {
            return Err(format!("the input device delivers {:?} samples; only 32-bit float input is supported", supported.sample_format()));
        }
        let config: cpal::StreamConfig = supported.into();
        let factor = reduction(config.sample_rate);
        self.rate = config.sample_rate / factor;
        self.channels = config.channels;
        self.device_name = device.description().map(|description| description.name().to_string()).unwrap_or_default();
        let channels = usize::from(config.channels.max(1));
        if self.channel() >= channels {
            self.set_channel(0);
        }
        let shared = Arc::clone(&self.shared);
        let mut carry = (0.0f32, 0u32);
        let stream = device
            .build_input_stream(
                config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if let Ok(mut peaks) = shared.peaks.lock() {
                        peaks.clear();
                        peaks.resize(channels, 0.0);
                        for frame in data.chunks_exact(channels) {
                            for (peak, sample) in peaks.iter_mut().zip(frame) {
                                *peak = peak.max(sample.abs());
                            }
                        }
                    }
                    if shared.recording.load(Ordering::Relaxed)
                        && let Ok(mut samples) = shared.samples.lock()
                    {
                        take_channel(data, channels, shared.channel.load(Ordering::Relaxed), factor, &mut carry, &mut samples);
                    }
                },
                |error| eprintln!("microphone error: {error}"),
                None,
            )
            .map_err(|error| error.to_string())?;
        stream.play().map_err(|error| error.to_string())?;
        self.stream = Some(stream);
        Ok(())
    }

    pub fn is_recording(&self) -> bool {
        self.shared.recording.load(Ordering::Relaxed)
    }

    pub fn channel(&self) -> usize {
        self.shared.channel.load(Ordering::Relaxed)
    }

    pub fn set_channel(&self, channel: usize) {
        self.shared.channel.store(channel, Ordering::Relaxed);
    }

    /// The latest peak of every input channel, 0 to 1.
    pub fn peaks(&self) -> Vec<f32> {
        self.shared.peaks.lock().map(|peaks| peaks.clone()).unwrap_or_default()
    }

    pub fn seconds(&self) -> f64 {
        self.shared.samples.lock().map_or(0.0, |samples| samples.len() as f64 / f64::from(self.rate))
    }

    pub fn start(&mut self) -> Result<(), String> {
        if self.stream.is_none() {
            self.listen()?;
        }
        if let Ok(mut samples) = self.shared.samples.lock() {
            samples.clear();
        }
        self.shared.recording.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Stops and writes the WAV file; returns its path.
    pub fn stop(&mut self, folder: &Path) -> Result<PathBuf, String> {
        self.shared.recording.store(false, Ordering::Relaxed);
        self.stream = None;
        let samples = self.shared.samples.lock().map(|samples| samples.clone()).unwrap_or_default();
        if samples.is_empty() {
            return Err("nothing was recorded".into());
        }
        std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default();
        let path = folder.join(format!("narration-{stamp}.wav"));
        write_wav(&path, &samples, self.rate).map_err(|error| error.to_string())?;
        Ok(path)
    }
}

/// Mono 16-bit PCM WAV.
pub fn write_wav(path: &Path, samples: &[i16], rate: u32) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_len).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&rate.to_le_bytes())?;
    file.write_all(&(rate * 2).to_le_bytes())?;
    file.write_all(&2u16.to_le_bytes())?;
    file.write_all(&16u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_len.to_le_bytes())?;
    for sample in samples {
        file.write_all(&sample.to_le_bytes())?;
    }
    file.flush()
}

impl AvsApp {
    pub fn open_recorder(&mut self) {
        if self.recorder.is_none() {
            self.recorder = Some(Recorder::open(self.playhead, self.view.narration_channel));
        }
    }

    pub fn recorder_start(&mut self) {
        let at = self.playhead;
        if let Some(recorder) = &mut self.recorder {
            recorder.at = at;
            recorder.error = recorder.start().err();
        }
    }

    pub fn recorder_stop(&mut self) {
        let folder = std::env::temp_dir().join("agent-video-studio-narration");
        let Some(recorder) = &mut self.recorder else { return };
        let at = recorder.at;
        match recorder.stop(&folder) {
            Ok(path) => {
                let track = self.target_track(TrackKind::Audio).unwrap_or_default();
                self.start_import(path, Some((track, at)));
                self.set_status(StatusKind::Info, self.tr("録音を読み込んでいます…", "Importing the recording…"));
                self.recorder = None;
            }
            Err(error) => recorder.error = Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn wav_header_is_44_bytes() {
        let dir = std::env::temp_dir().join(format!("avs-wav-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        super::write_wav(&path, &[0, 16_384, -16_384, i16::MAX], 48_000).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + 8);
        assert_eq!(&bytes[..4], b"RIFF");
        // One channel.
        assert_eq!(&bytes[22..24], &1u16.to_le_bytes());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A 16-channel 192 kHz interface: only the chosen channel is kept, at 48 kHz.
    #[test]
    fn one_channel_is_kept_and_high_rates_are_reduced() {
        assert_eq!(super::reduction(192_000), 4);
        assert_eq!(super::reduction(96_000), 2);
        assert_eq!(super::reduction(88_200), 2);
        assert_eq!(super::reduction(48_000), 1);
        assert_eq!(super::reduction(44_100), 1);
        let channels = 16;
        // Channel 2 carries 0.5 on every frame; the others are silent or loud.
        let mut data = Vec::new();
        for _ in 0..6 {
            let mut frame = vec![1.0f32; channels];
            frame[2] = 0.5;
            data.extend(frame);
        }
        let (mut carry, mut out) = ((0.0, 0), Vec::new());
        // Split across two blocks to check the unfinished group is carried over.
        super::take_channel(&data[..channels * 3], channels, 2, 4, &mut carry, &mut out);
        super::take_channel(&data[channels * 3..], channels, 2, 4, &mut carry, &mut out);
        assert_eq!(out, vec![16_384]);
        assert_eq!(carry.1, 2);
    }
}
