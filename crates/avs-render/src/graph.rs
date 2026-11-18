//! The composition graph: one generator that turns a project and a time range into
//! ffmpeg inputs and a `-filter_complex`. Export and preview both call it, so the
//! picture and the sound you preview are computed exactly as they are exported.
//!
//! Conventions (all of them follow the earlier implementation unless noted in the
//! crate docs):
//!
//! - Output time 0 is `options.start`. Every clip is clipped to the window
//!   `[start, start + duration)`; a clip that begins before `start` is seeked into.
//! - Video tracks are stacked with the **first track in `project.tracks` on top**.
//!   Caption tracks are drawn above every video track, first caption track on top.
//! - Positions are the clip centre in project pixels; `scale` multiplies the media's
//!   own pixel size. A clip with neither position nor scale is auto-fitted: portrait
//!   video is shown whole (contain), everything else fills the frame (cover).
//! - Only media clips on audio tracks make sound (a video file there plays its audio
//!   only). Clips on video tracks are picture only; their sound is the linked clip.

use crate::caption::{CaptionImage, caption_font_path, render_caption};
use crate::error::{RenderError, Result};
use crate::fmt::{fmt_num, fmt_time};
use crate::media::MediaResolver;
use crate::mutes::mute_intervals_for;
use crate::tool::Toolchain;
use avs_core::{CaptionStyle, Clip, ClipKind, MediaAsset, MediaType, Project, Time, Track, TrackKind};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Streams {
    Both,
    VideoOnly,
    AudioOnly,
}

impl Streams {
    fn video(self) -> bool {
        self != Streams::AudioOnly
    }
    fn audio(self) -> bool {
        self != Streams::VideoOnly
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoFormat {
    /// Encoder-ready (export).
    Yuv420p,
    /// Raw frames for the preview.
    Rgba,
}

#[derive(Debug, Clone)]
pub struct GraphOptions {
    /// Timeline position that becomes output time 0.
    pub start: Time,
    /// Length to render; `None` means up to the end of the timeline.
    pub duration: Option<Time>,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub sample_rate: u32,
    /// 1 or 2.
    pub channels: u32,
    pub streams: Streams,
    pub video_format: VideoFormat,
}

impl GraphOptions {
    /// The project's own resolution, fps and sample rate, whole timeline, both
    /// streams, encoder-ready pixels.
    pub fn for_project(project: &Project) -> Self {
        GraphOptions {
            start: Time::ZERO,
            duration: None,
            width: project.resolution.width,
            height: project.resolution.height,
            fps: project.fps,
            sample_rate: project.sample_rate,
            channels: 2,
            streams: Streams::Both,
            video_format: VideoFormat::Yuv420p,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceRole {
    Video,
    Audio,
}

/// A media file the render reads, with the stretch of it that is used.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsedSource {
    pub input_index: usize,
    pub role: SourceRole,
    pub clip_id: String,
    pub track_id: String,
    pub media_id: String,
    pub media_type: MediaType,
    pub source: String,
    pub path: PathBuf,
    /// Where the clip's used stretch starts and lasts on the timeline.
    pub start_seconds: Time,
    pub duration_seconds: Time,
    /// Where in the media file that stretch starts.
    pub source_start_seconds: Time,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderWarning {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphInput {
    /// Options placed before `-i` (seek, loop, duration).
    pub options: Vec<String>,
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct RenderGraph {
    pub inputs: Vec<GraphInput>,
    pub filter_complex: String,
    /// Filter-graph labels to `-map`, present when that stream was requested.
    pub video_label: Option<String>,
    pub audio_label: Option<String>,
    pub start: Time,
    pub duration: Time,
    /// The size of the picture that leaves the graph.
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub sample_rate: u32,
    pub channels: u32,
    pub sources: Vec<UsedSource>,
    /// Caption clips drawn into the picture.
    pub caption_clip_ids: Vec<String>,
    pub warnings: Vec<RenderWarning>,
    /// Holds the caption images the inputs point at; must outlive the ffmpeg process.
    _assets: Option<tempfile::TempDir>,
}

impl RenderGraph {
    /// The `-i` arguments for every input, in order.
    pub fn input_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for input in &self.inputs {
            args.extend(input.options.iter().cloned());
            args.push("-i".to_string());
            args.push(input.path.to_string_lossy().into_owned());
        }
        args
    }

    /// `-filter_complex <graph> -map [..]` for the requested streams.
    pub fn graph_args(&self) -> Vec<String> {
        let mut args = vec!["-filter_complex".to_string(), self.filter_complex.clone()];
        for label in [&self.video_label, &self.audio_label].into_iter().flatten() {
            args.push("-map".to_string());
            args.push(format!("[{label}]"));
        }
        args
    }

    pub fn build(project: &Project, resolver: &dyn MediaResolver, options: &GraphOptions) -> Result<RenderGraph> {
        Builder::new(project, resolver, options)?.build()
    }
}

/// Where a clip sits relative to the render window.
#[derive(Debug, Clone, Copy)]
struct Placement {
    /// First and last moment shown, on the timeline.
    start: Time,
    end: Time,
    /// How far into the clip the window cuts (timeline time).
    offset: Time,
    /// Where in the media file the shown stretch starts and how much of it is read.
    source_start: Time,
    source_len: Time,
}

#[derive(Debug, Clone, Copy)]
struct Geometry {
    /// Canvas (the composed picture) size and the project-to-canvas factor.
    canvas_w: u32,
    canvas_h: u32,
    k: f64,
    out_w: u32,
    out_h: u32,
}

fn even(value: f64) -> u32 {
    (((value / 2.0).round() as i64) * 2).max(2) as u32
}

struct Builder<'a> {
    project: &'a Project,
    resolver: &'a dyn MediaResolver,
    options: &'a GraphOptions,
    toolchain: &'static Toolchain,
    from: Time,
    to: Time,
    geometry: Geometry,
    inputs: Vec<GraphInput>,
    sources: Vec<UsedSource>,
    warnings: Vec<RenderWarning>,
    resolved: HashMap<String, PathBuf>,
    audio_probe: HashMap<PathBuf, bool>,
    needed: BTreeSet<&'static str>,
    assets: Option<tempfile::TempDir>,
}

impl<'a> Builder<'a> {
    fn new(project: &'a Project, resolver: &'a dyn MediaResolver, options: &'a GraphOptions) -> Result<Self> {
        let bad = |message: String| Err(RenderError::InvalidOptions(message));
        if options.width == 0 || options.height == 0 {
            return bad("output width and height must be positive".into());
        }
        if !(options.fps.is_finite() && options.fps > 0.0 && options.fps <= 240.0) {
            return bad(format!("fps must be between 0 and 240, got {}", options.fps));
        }
        if options.sample_rate < 8_000 || options.sample_rate > 192_000 {
            return bad(format!("sample rate must be between 8000 and 192000, got {}", options.sample_rate));
        }
        if !(1..=2).contains(&options.channels) {
            return bad(format!("channels must be 1 or 2, got {}", options.channels));
        }
        if options.video_format == VideoFormat::Yuv420p && options.streams.video() && (options.width % 2 != 0 || options.height % 2 != 0) {
            return bad(format!("yuv420p output needs an even width and height, got {}x{}", options.width, options.height));
        }
        if project.resolution.width == 0 || project.resolution.height == 0 {
            return bad("the project resolution must be non-zero".into());
        }
        if options.start.is_negative() {
            return bad("the start time must not be negative".into());
        }
        let toolchain = Toolchain::get()?;

        let from = options.start;
        let to = match options.duration {
            Some(duration) if duration.is_positive() => from + duration,
            Some(duration) => return Err(RenderError::EmptyRange(format!("the render duration must be positive, got {duration}s"))),
            None => {
                let end = project.duration();
                if end <= from {
                    return Err(RenderError::EmptyRange(format!("the timeline is {end}s long; there is nothing to render from {from}s")));
                }
                end
            }
        };

        let (pw, ph) = (f64::from(project.resolution.width), f64::from(project.resolution.height));
        let k = (f64::from(options.width) / pw).min(f64::from(options.height) / ph);
        let geometry = Geometry { canvas_w: even(pw * k), canvas_h: even(ph * k), k, out_w: options.width, out_h: options.height };
        Ok(Builder {
            project,
            resolver,
            options,
            toolchain,
            from,
            to,
            geometry,
            inputs: Vec::new(),
            sources: Vec::new(),
            warnings: Vec::new(),
            resolved: HashMap::new(),
            audio_probe: HashMap::new(),
            needed: BTreeSet::new(),
            assets: None,
        })
    }

    fn warn(&mut self, code: &str, message: String, clip: Option<&Clip>) {
        self.warnings.push(RenderWarning { code: code.to_string(), message, clip_id: clip.map(|clip| clip.id.clone()) });
    }

    fn duration(&self) -> Time {
        self.to - self.from
    }

    fn placement(&self, clip: &Clip, speed: f64) -> Option<Placement> {
        let start = clip.start_seconds.max(self.from);
        let end = clip.end().min(self.to);
        if end <= start {
            return None;
        }
        let offset = start - clip.start_seconds;
        Some(Placement {
            start,
            end,
            offset,
            source_start: clip.source_start() + offset.scale(speed),
            source_len: (end - start).scale(speed),
        })
    }

    fn resolve(&mut self, media: &MediaAsset) -> Result<PathBuf> {
        let unavailable = |reason: String| RenderError::MediaUnavailable {
            media_id: media.id.clone(),
            source: media.source.clone().unwrap_or_default(),
            reason,
        };
        let source = media.source.as_deref().filter(|source| !source.is_empty()).ok_or_else(|| unavailable("the media has no source".into()))?;
        if let Some(path) = self.resolved.get(source) {
            return Ok(path.clone());
        }
        let path = self.resolver.resolve(source).map_err(unavailable)?;
        if !path.is_absolute() {
            return Err(unavailable(format!("the resolver returned a relative path: {}", path.display())));
        }
        if !path.is_file() {
            return Err(unavailable(format!("no file at {}", path.display())));
        }
        self.resolved.insert(source.to_string(), path.clone());
        Ok(path)
    }

    fn media_of(&self, clip: &Clip) -> Result<&'a MediaAsset> {
        let media_id = clip.media_id.as_deref().unwrap_or_default();
        self.project.media.iter().find(|media| media.id == media_id).ok_or_else(|| RenderError::MediaUnavailable {
            media_id: media_id.to_string(),
            source: String::new(),
            reason: format!("clip {} refers to media that is not registered", clip.id),
        })
    }

    fn push_input(&mut self, options: Vec<String>, path: PathBuf) -> usize {
        self.inputs.push(GraphInput { options, path });
        self.inputs.len() - 1
    }

    fn build(mut self) -> Result<RenderGraph> {
        let mut filters: Vec<String> = Vec::new();
        let (mut video_label, mut audio_label) = (None, None);
        let mut caption_clip_ids = Vec::new();
        if self.options.streams.video() {
            video_label = Some(self.video_graph(&mut filters, &mut caption_clip_ids)?);
        }
        if self.options.streams.audio() {
            audio_label = Some(self.audio_graph(&mut filters)?);
        }
        let needed: Vec<&str> = self.needed.iter().copied().collect();
        self.toolchain.require_filters(&needed)?;
        Ok(RenderGraph {
            inputs: self.inputs,
            filter_complex: filters.join(";"),
            video_label,
            audio_label,
            start: self.from,
            duration: self.to - self.from,
            width: self.geometry.out_w,
            height: self.geometry.out_h,
            fps: self.options.fps,
            sample_rate: self.options.sample_rate,
            channels: self.options.channels,
            sources: self.sources,
            caption_clip_ids,
            warnings: self.warnings,
            _assets: self.assets,
        })
    }

    // ---- video ---------------------------------------------------------------

    fn video_graph(&mut self, filters: &mut Vec<String>, caption_ids: &mut Vec<String>) -> Result<String> {
        let g = self.geometry;
        let fps = fmt_num(self.options.fps);
        self.needed.extend(["color", "format", "overlay", "setpts"]);
        filters.push(format!("color=c=black:s={}x{}:r={fps}:d={},format=rgba[base0]", g.canvas_w, g.canvas_h, fmt_time(self.duration())));
        let mut current = "base0".to_string();
        let mut layer = 0usize;

        let project = self.project;
        // Lower tracks first, so the first track ends up on top.
        for track in project.tracks.iter().rev().filter(|track| track.kind == TrackKind::Video && !track.is_muted()) {
            for clip in sorted_clips(track) {
                // A clip's `muted` flag is about sound; the picture stays. (Hiding the
                // picture is what disabling the whole track does.)
                if clip.kind != ClipKind::Media {
                    continue;
                }
                let media = self.media_of(clip)?;
                match media.media_type {
                    MediaType::Video | MediaType::Image => {}
                    MediaType::Sequence => {
                        if self.placement(clip, 1.0).is_some() {
                            self.warn(
                                "SEQUENCE_MEDIA_UNSUPPORTED",
                                format!("clip {} uses image-sequence media {}, which this renderer cannot read; the clip is left out", clip.id, media.id),
                                Some(clip),
                            );
                        }
                        continue;
                    }
                    MediaType::Audio => {
                        self.warn("AUDIO_MEDIA_ON_VIDEO_TRACK", format!("clip {} puts audio media on a video track; it shows nothing", clip.id), Some(clip));
                        continue;
                    }
                }
                let is_video = media.media_type == MediaType::Video;
                let speed = if is_video { clip.speed() } else { 1.0 };
                let Some(placement) = self.placement(clip, speed) else { continue };
                let path = self.resolve(media)?;
                let input_options = if is_video {
                    vec!["-ss".to_string(), fmt_time(placement.source_start), "-t".to_string(), fmt_time(placement.source_len)]
                } else {
                    still_options(&fmt_num(self.options.fps), placement.end - placement.start)
                };
                let index = self.push_input(input_options, path.clone());
                self.sources.push(UsedSource {
                    input_index: index,
                    role: SourceRole::Video,
                    clip_id: clip.id.clone(),
                    track_id: track.id.clone(),
                    media_id: media.id.clone(),
                    media_type: media.media_type,
                    source: media.source.clone().unwrap_or_default(),
                    path,
                    start_seconds: placement.start,
                    duration_seconds: placement.end - placement.start,
                    source_start_seconds: placement.source_start,
                });
                let LayerFilters { chain, x, y, needs } = layer_filters(clip, is_video, speed, &placement, self.from, &g);
                self.needed.extend(needs);
                let visual = format!("vis{layer}");
                let next = format!("base{}", layer + 1);
                filters.push(format!("[{index}:v]{chain}[{visual}]"));
                filters.push(format!(
                    "[{current}][{visual}]overlay=x={x}:y={y}:eof_action=pass:format=auto:enable='{}'[{next}]",
                    window_enable(placement.start - self.from, placement.end - self.from)
                ));
                current = next;
                layer += 1;
            }
        }

        // Captions go above every video track.
        for track in project.tracks.iter().rev().filter(|track| track.kind == TrackKind::Caption && !track.is_muted()) {
            for clip in sorted_clips(track) {
                if clip.kind != ClipKind::Caption || clip.properties.muted == Some(true) {
                    continue;
                }
                let Some(text) = clip.caption.as_ref().map(|caption| caption.text.trim()).filter(|text| !text.is_empty()) else { continue };
                let Some(placement) = self.placement(clip, 1.0) else { continue };
                let style = clip.properties.caption_style.clone().unwrap_or_else(CaptionStyle::default);
                let image = render_caption(text, &style, f64::from(project.resolution.height), g.canvas_w, g.canvas_h, g.k)?;
                for message in &image.warnings {
                    self.warn("CAPTION_STYLE", format!("caption {}: {message}", clip.id), Some(clip));
                }
                let path = self.write_caption_image(&image)?;
                let index = self.push_input(still_options(&fmt_num(self.options.fps), placement.end - placement.start), path);
                let visual = format!("cap{layer}");
                let next = format!("base{}", layer + 1);
                filters.push(format!("[{index}:v]format=rgba,setpts=PTS-STARTPTS+({})/TB[{visual}]", fmt_time(placement.start - self.from)));
                filters.push(format!(
                    "[{current}][{visual}]overlay=x={}:y={}:eof_action=pass:format=auto:enable='{}'[{next}]",
                    image.x,
                    image.y,
                    window_enable(placement.start - self.from, placement.end - self.from)
                ));
                caption_ids.push(clip.id.clone());
                current = next;
                layer += 1;
            }
        }

        let mut tail = Vec::new();
        if (g.canvas_w, g.canvas_h) != (g.out_w, g.out_h) {
            self.needed.insert("pad");
            tail.push(format!("pad={}:{}:(ow-iw)/2:(oh-ih)/2:color=black", g.out_w, g.out_h));
        }
        tail.push(match self.options.video_format {
            VideoFormat::Yuv420p => "format=yuv420p".to_string(),
            VideoFormat::Rgba => "format=rgba".to_string(),
        });
        filters.push(format!("[{current}]{}[vout]", tail.join(",")));
        Ok("vout".to_string())
    }

    fn write_caption_image(&mut self, image: &CaptionImage) -> Result<PathBuf> {
        // Resolve the font first so a missing font is reported as such, not as a file error.
        caption_font_path()?;
        if self.assets.is_none() {
            self.assets = Some(tempfile::Builder::new().prefix("avs-render-").tempdir()?);
        }
        let dir = self.assets.as_ref().expect("created above").path();
        let path = dir.join(format!("caption-{}.png", self.inputs.len()));
        image.write_png(&path)?;
        Ok(path)
    }

    // ---- audio ---------------------------------------------------------------

    fn audio_graph(&mut self, filters: &mut Vec<String>) -> Result<String> {
        let (sample_rate, layout) = (self.options.sample_rate, if self.options.channels == 1 { "mono" } else { "stereo" });
        let length = fmt_time(self.duration());
        self.needed.extend(["anullsrc", "aformat", "amix", "asetpts", "atrim", "adelay"]);
        filters.push(format!("anullsrc=r={sample_rate}:cl={layout}:d={length}[abase]"));
        let mut mix_inputs = vec!["[abase]".to_string()];

        let project = self.project;
        for track in project.tracks.iter().filter(|track| track.kind == TrackKind::Audio && !track.is_muted()) {
            for clip in sorted_clips(track) {
                if clip.kind != ClipKind::Media || clip.properties.muted == Some(true) {
                    continue;
                }
                let media = self.media_of(clip)?;
                if !matches!(media.media_type, MediaType::Audio | MediaType::Video) {
                    if self.placement(clip, 1.0).is_some() {
                        self.warn(
                            "NO_SOUND_IN_MEDIA",
                            format!("clip {} on audio track {} uses {:?} media, which has no sound", clip.id, track.id, media.media_type),
                            Some(clip),
                        );
                    }
                    continue;
                }
                let speed = clip.speed();
                let Some(placement) = self.placement(clip, speed) else { continue };
                let path = self.resolve(media)?;
                if media.media_type == MediaType::Video && !self.video_has_audio(media, &path)? {
                    self.warn("NO_AUDIO_STREAM", format!("clip {} uses a video file without an audio stream; it stays silent", clip.id), Some(clip));
                    continue;
                }
                let index = self.push_input(vec!["-ss".into(), fmt_time(placement.source_start), "-t".into(), fmt_time(placement.source_len)], path.clone());
                self.sources.push(UsedSource {
                    input_index: index,
                    role: SourceRole::Audio,
                    clip_id: clip.id.clone(),
                    track_id: track.id.clone(),
                    media_id: media.id.clone(),
                    media_type: media.media_type,
                    source: media.source.clone().unwrap_or_default(),
                    path,
                    start_seconds: placement.start,
                    duration_seconds: placement.end - placement.start,
                    source_start_seconds: placement.source_start,
                });
                let label = format!("a{}", mix_inputs.len() - 1);
                let mutes = mute_intervals_for(project, clip, media);
                let (chain, needs) = audio_filters(clip, speed, &placement, self.from, &mutes, sample_rate, layout);
                self.needed.extend(needs);
                filters.push(format!("[{index}:a]{chain}[{label}]"));
                mix_inputs.push(format!("[{label}]"));
            }
        }

        // The silent base is always the first input and sets the length, so the audio
        // always lasts as long as the render; `normalize=0` keeps every clip at its
        // own level however many clips there are.
        filters.push(format!(
            "{}amix=inputs={}:duration=first:dropout_transition=0:normalize=0,atrim=end={length},asetpts=N/SR/TB[aout]",
            mix_inputs.concat(),
            mix_inputs.len()
        ));
        Ok("aout".to_string())
    }

    fn video_has_audio(&mut self, media: &MediaAsset, path: &Path) -> Result<bool> {
        let declared = media
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("mediaProbe")?.get("audio")?.get("hasAudio")?.as_bool());
        if let Some(declared) = declared {
            return Ok(declared);
        }
        if let Some(known) = self.audio_probe.get(path) {
            return Ok(*known);
        }
        let found = self.toolchain.has_audio_stream(path).map_err(|error| match error {
            RenderError::MediaUnavailable { source, reason, .. } => RenderError::MediaUnavailable { media_id: media.id.clone(), source, reason },
            other => other,
        })?;
        self.audio_probe.insert(path.to_path_buf(), found);
        Ok(found)
    }
}

fn sorted_clips(track: &Track) -> Vec<&Clip> {
    let mut clips: Vec<&Clip> = track.clips.iter().collect();
    clips.sort_by(|a, b| a.start_seconds.cmp(&b.start_seconds).then_with(|| a.id.cmp(&b.id)));
    clips
}

fn still_options(fps: &str, length: Time) -> Vec<String> {
    ["-loop", "1", "-framerate", fps, "-t", &fmt_time(length)].iter().map(ToString::to_string).collect()
}

/// `[a, b)` in output time. Half-open, so a clip never shows one frame too many.
fn window_enable(start: Time, end: Time) -> String {
    format!("gte(t,{})*lt(t,{})", fmt_time(start), fmt_time(end))
}

struct LayerFilters {
    chain: String,
    x: String,
    y: String,
    needs: Vec<&'static str>,
}

/// The filters that turn one decoded clip into an RGBA layer at the right size and
/// time, plus the overlay position. Pure: depends only on its arguments.
fn layer_filters(clip: &Clip, is_video: bool, speed: f64, placement: &Placement, from: Time, g: &Geometry) -> LayerFilters {
    let props = &clip.properties;
    let mut chain: Vec<String> = Vec::new();
    let mut needs: Vec<&'static str> = vec!["scale", "setpts"];

    if let Some(crop) = props.crop.filter(|crop| {
        crop.left >= 0.0 && crop.right >= 0.0 && crop.top >= 0.0 && crop.bottom >= 0.0 && crop.left + crop.right < 1.0 && crop.top + crop.bottom < 1.0
    }) {
        if crop.left + crop.right + crop.top + crop.bottom > 0.0 {
            needs.push("crop");
            chain.push(format!(
                "crop=w=iw*{}:h=ih*{}:x=iw*{}:y=ih*{}",
                fmt_num(1.0 - crop.left - crop.right),
                fmt_num(1.0 - crop.top - crop.bottom),
                fmt_num(crop.left),
                fmt_num(crop.top)
            ));
        }
    }

    let (cw, ch) = (g.canvas_w, g.canvas_h);
    match (props.scale.filter(|scale| scale.is_finite() && *scale > 0.0), props.position) {
        (Some(scale), _) => chain.extend(natural_size(scale * g.k)),
        (None, Some(_)) => chain.extend(natural_size(g.k)),
        (None, None) => {
            // Cover the frame; a portrait video is shown whole instead.
            let ratio = if is_video {
                format!("if(gt(ih,iw),min({cw}/iw,{ch}/ih),max({cw}/iw,{ch}/ih))")
            } else {
                format!("max({cw}/iw,{ch}/ih)")
            };
            needs.push("crop");
            chain.push(format!("scale=w='ceil(iw*{ratio}/2)*2':h='ceil(ih*{ratio}/2)*2'"));
            chain.push(format!("crop=w='min(iw,{cw})':h='min(ih,{ch})'"));
        }
    }

    chain.push("format=rgba".to_string());
    let degrees = props.rotation_degrees.filter(|degrees| degrees.is_finite() && degrees.abs() > 1e-9);
    if let Some(degrees) = degrees {
        needs.push("rotate");
        let radians = fmt_num(degrees.to_radians());
        chain.push(format!("rotate=a={radians}:ow='rotw({radians})':oh='roth({radians})':c=none"));
    }
    let opacity = props.opacity.filter(|opacity| opacity.is_finite()).map(|opacity| opacity.clamp(0.0, 1.0)).unwrap_or(1.0);
    if opacity < 1.0 {
        needs.push("colorchannelmixer");
        chain.push(format!("colorchannelmixer=aa={}", fmt_num(opacity)));
    }

    // Timestamps: first onto the clip's own time (so fades use the clip's own
    // start even when the window cuts into it), then onto the output time.
    let duration = clip.duration_seconds;
    let fade_in = clip.fade_in().min(duration).max(Time::ZERO);
    let fade_out = clip.fade_out().min(duration).max(Time::ZERO);
    let speed_text = fmt_num(speed);
    if fade_in.is_positive() || fade_out.is_positive() {
        needs.push("fade");
        chain.push(format!("setpts=(PTS-STARTPTS)/{speed_text}+({})/TB", fmt_time(placement.offset)));
        if fade_in.is_positive() {
            chain.push(format!("fade=t=in:st=0:d={}:alpha=1", fmt_time(fade_in)));
        }
        if fade_out.is_positive() {
            chain.push(format!("fade=t=out:st={}:d={}:alpha=1", fmt_time(duration - fade_out), fmt_time(fade_out)));
        }
        chain.push(format!("setpts=PTS+({})/TB", fmt_time(clip.start_seconds - from)));
    } else {
        chain.push(format!("setpts=(PTS-STARTPTS)/{speed_text}+({})/TB", fmt_time(placement.start - from)));
    }

    let centre_x = props.position.map_or(f64::from(cw) / 2.0, |p| p.x * g.k);
    let centre_y = props.position.map_or(f64::from(ch) / 2.0, |p| p.y * g.k);
    LayerFilters { chain: chain.join(","), x: format!("{}-w/2", fmt_num(centre_x)), y: format!("{}-h/2", fmt_num(centre_y)), needs }
}

/// The `scale` that reproduces the auto-fit of a clip with neither `position` nor
/// `scale` (the `(None, None)` branch of [`layer_filters`]), in project pixels:
/// cover the frame, or contain it for portrait video. `media_*` is the picture size
/// after crop; the auto-fitted clip is centred, i.e. at position (width/2, height/2).
/// An editor writes this pair when the user first changes a clip's transform, so
/// the picture does not jump to its natural size.
pub fn auto_fit_scale(media_width: f64, media_height: f64, project_width: f64, project_height: f64, is_video: bool) -> f64 {
    if !(media_width > 0.0 && media_height > 0.0 && project_width > 0.0 && project_height > 0.0) {
        return 1.0;
    }
    let (fit_w, fit_h) = (project_width / media_width, project_height / media_height);
    if is_video && media_height > media_width { fit_w.min(fit_h) } else { fit_w.max(fit_h) }
}

/// Resizes to `factor` times the media's own pixel size (nothing to do at 1).
fn natural_size(factor: f64) -> Option<String> {
    if (factor - 1.0).abs() < 1e-9 {
        return None;
    }
    let f = fmt_num(factor);
    Some(format!("scale=w='max(2,trunc(iw*{f}/2)*2)':h='max(2,trunc(ih*{f}/2)*2)'"))
}

/// The per-clip audio chain: unify the format, change speed, fade, trim, set the
/// level, apply BGM stops and delay the clip to its place in the window.
fn audio_filters(
    clip: &Clip,
    speed: f64,
    placement: &Placement,
    from: Time,
    mutes: &[crate::mutes::MuteInterval],
    sample_rate: u32,
    layout: &str,
) -> (String, Vec<&'static str>) {
    let mut chain: Vec<String> = vec![format!("aformat=sample_fmts=fltp:sample_rates={sample_rate}:channel_layouts={layout}")];
    let mut needs: Vec<&'static str> = vec!["aformat", "asetpts", "atrim"];

    if (speed - 1.0).abs() > 1e-9 {
        needs.push("atempo");
        let mut remaining = speed;
        while remaining > 2.0 {
            chain.push("atempo=2".to_string());
            remaining /= 2.0;
        }
        while remaining < 0.5 {
            chain.push("atempo=0.5".to_string());
            remaining /= 0.5;
        }
        chain.push(format!("atempo={}", fmt_num(remaining)));
    }

    let duration = clip.duration_seconds;
    let fade_in = clip.fade_in().min(duration).max(Time::ZERO);
    let fade_out = clip.fade_out().min(duration).max(Time::ZERO);
    // The samples carry the clip's own time while the fades run, then restart at 0.
    if fade_in.is_positive() || fade_out.is_positive() {
        needs.push("afade");
        chain.push(format!("asetpts=N/SR/TB+({})/TB", fmt_time(placement.offset)));
        if fade_in.is_positive() {
            chain.push(format!("afade=t=in:st=0:d={}", fmt_time(fade_in)));
        }
        if fade_out.is_positive() {
            chain.push(format!("afade=t=out:st={}:d={}", fmt_time(duration - fade_out), fmt_time(fade_out)));
        }
    }
    chain.push("asetpts=N/SR/TB".to_string());
    chain.push(format!("atrim=end={}", fmt_time(placement.end - placement.start)));

    if let Some(db) = clip.properties.volume_db.filter(|db| db.is_finite() && db.abs() > 1e-9) {
        needs.push("volume");
        chain.push(format!("volume={}dB", fmt_num(db)));
    }
    for interval in mutes {
        let start = (interval.start - placement.start).max(Time::ZERO);
        let end = (interval.end - placement.start).min(placement.end - placement.start);
        if end > start {
            needs.push("volume");
            chain.push(format!("volume=0:enable='{}'", window_enable(start, end)));
        }
    }
    let delay = (placement.start - from).secs() * f64::from(sample_rate);
    let delay = delay.round() as i64;
    if delay > 0 {
        chain.push(format!("adelay={delay}S:all=1"));
    }
    (chain.join(","), needs)
}
