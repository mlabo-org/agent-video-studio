//! Captions as pictures. This ffmpeg has no `drawtext`, so the text is rasterised
//! here with a macOS Japanese font into a transparent PNG that `overlay` places on
//! the video. The same code runs for the preview and the export.

use crate::error::{RenderError, Result};
use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use avs_core::CaptionStyle;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const DEFAULT_TEXT_COLOR: [u8; 4] = [0xf7, 0xfa, 0xfc, 0xff];
const BACKGROUND_OPACITY: f32 = 0.72;
const SHADOW_OPACITY: f32 = 0.55;
/// Share of the picture width a line may use (the text block, before the box padding).
const MAX_TEXT_WIDTH: f64 = 0.86;

/// Where the font is looked for, in order. `AVS_CAPTION_FONT` overrides the list.
const FONT_CANDIDATES: [&str; 5] = [
    "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
    "/System/Library/Fonts/ヒラギノ角ゴシック W4.ttc",
    "/System/Library/Fonts/ヒラギノ丸ゴ ProN W4.ttc",
    "/Library/Fonts/Arial Unicode.ttf",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
];

static FONT: OnceLock<std::result::Result<(FontVec, PathBuf), String>> = OnceLock::new();

fn font() -> Result<&'static (FontVec, PathBuf)> {
    FONT.get_or_init(load_font).as_ref().map_err(|message| RenderError::FontUnavailable(message.clone()))
}

fn load_font() -> std::result::Result<(FontVec, PathBuf), String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("AVS_CAPTION_FONT").filter(|value| !value.is_empty()) {
        candidates.push(PathBuf::from(path));
    }
    candidates.extend(FONT_CANDIDATES.iter().map(PathBuf::from));
    for path in &candidates {
        let Ok(data) = std::fs::read(path) else { continue };
        if let Ok(font) = FontVec::try_from_vec_and_index(data, 0) {
            return Ok((font, path.clone()));
        }
    }
    Err(format!(
        "no caption font could be loaded (tried {}). Set AVS_CAPTION_FONT to a .ttf/.ttc file.",
        candidates.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", ")
    ))
}

/// The font file captions are drawn with.
pub fn caption_font_path() -> Result<&'static Path> {
    font().map(|(_, path)| path.as_path())
}

/// A rendered caption box and where it sits on the picture.
#[derive(Debug, Clone)]
pub struct CaptionImage {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
    /// Straight (non-premultiplied) RGBA, row by row.
    pub rgba: Vec<u8>,
    /// Things the caller should tell the user (unknown colour, missing glyphs).
    pub warnings: Vec<String>,
}

impl CaptionImage {
    pub fn write_png(&self, path: &Path) -> Result<()> {
        let file = std::fs::File::create(path)?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|error| RenderError::Io(error.to_string()))?;
        writer.write_image_data(&self.rgba).map_err(|error| RenderError::Io(error.to_string()))?;
        Ok(())
    }
}

/// Lays out and draws one caption.
///
/// `project_height` is the project's resolution height: default size, the font-size
/// clamp and the placement margins are fractions of it. `canvas_*` is the picture the
/// caption is drawn on and `scale` is canvas size over project size, so the preview
/// at half resolution draws the same caption at half size.
pub fn render_caption(
    text: &str,
    style: &CaptionStyle,
    project_height: f64,
    canvas_width: u32,
    canvas_height: u32,
    scale: f64,
) -> Result<CaptionImage> {
    let (font, _) = font()?;
    let mut warnings = Vec::new();

    let text_color = parse_color(style.color.as_deref(), DEFAULT_TEXT_COLOR, "color", &mut warnings);
    let background = style
        .background
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| parse_color(Some(value), [0, 0, 0, 0], "background", &mut warnings))
        .filter(|color: &[u8; 4]| color[3] > 0);

    let size_in_project = match style.font_size {
        Some(size) if size.is_finite() && size > 0.0 => size.clamp(12.0, (project_height / 3.0).max(12.0)),
        _ => (project_height * 0.04).round().max(24.0),
    };
    let font_px = (size_in_project * scale).max(6.0);
    let scaled = font.as_scaled(PxScale::from(font_px as f32));
    let line_height = (scaled.ascent() - scaled.descent() + scaled.line_gap()).max(font_px as f32);

    let padding = if background.is_some() { (24.0 * scale).round().max(2.0) } else { (2.0 * scale).ceil().max(1.0) + 1.0 };
    let max_line = (f64::from(canvas_width) * MAX_TEXT_WIDTH - 2.0 * padding).max(font_px);

    let mut missing = false;
    let advance = |c: char, missing: &mut bool| -> f32 {
        let id = font.glyph_id(c);
        if id.0 == 0 && !c.is_whitespace() {
            *missing = true;
        }
        scaled.h_advance(id)
    };
    let lines = wrap(text, max_line as f32, &mut |c| advance(c, &mut missing));
    if missing {
        warnings.push("some characters have no glyph in the caption font and were left blank".to_string());
    }
    let line_widths: Vec<f32> = lines.iter().map(|line| line_width(line, &mut |c| advance(c, &mut missing))).collect();
    let text_width = line_widths.iter().copied().fold(0.0f32, f32::max);
    let text_height = line_height * lines.len() as f32;

    let width = (text_width.ceil() as u32 + 2 * padding as u32).max(1);
    let height = (text_height.ceil() as u32 + 2 * padding as u32).max(1);
    let mut canvas = Canvas::new(width, height);
    if let Some(color) = background {
        canvas.fill([color[0], color[1], color[2], 0], f32::from(color[3]) / 255.0 * BACKGROUND_OPACITY);
    }

    let shadow = (2.0 * scale).round().max(1.0) as f32;
    for (index, line) in lines.iter().enumerate() {
        let baseline = padding as f32 + index as f32 * line_height + scaled.ascent();
        let left = padding as f32 + (text_width - line_widths[index]) / 2.0;
        if background.is_none() {
            draw_line(&mut canvas, font, &scaled, line, (left + shadow, baseline + shadow), Ink { color: [0, 0, 0, 0xff], opacity: SHADOW_OPACITY });
        }
        draw_line(&mut canvas, font, &scaled, line, (left, baseline), Ink { color: text_color, opacity: f32::from(text_color[3]) / 255.0 });
    }

    let (cw, ch) = (f64::from(canvas_width), f64::from(canvas_height));
    let margin_small = (f64::from(canvas_height) * 0.08).round();
    let margin_large = (f64::from(canvas_height) * 0.18).round();
    let text_h = f64::from(text_height);
    let text_top = match style.placement.as_deref() {
        Some("title") => margin_large,
        Some("center") => (ch - text_h) / 2.0,
        Some("subtitle") => ch - (text_h + margin_small),
        _ => ch - (text_h + margin_large),
    };
    let pad = padding;
    let x = ((cw - f64::from(width)) / 2.0).round();
    let y = (text_top - pad).round().clamp(0.0, (ch - f64::from(height)).max(0.0));
    Ok(CaptionImage { x: x as i64, y: y as i64, width, height, rgba: canvas.data, warnings })
}

struct Canvas {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        Canvas { width, height, data: vec![0; (width * height * 4) as usize] }
    }

    /// Covers the whole canvas with a colour at the given opacity.
    fn fill(&mut self, color: [u8; 4], opacity: f32) {
        for y in 0..self.height {
            for x in 0..self.width {
                self.blend(x, y, color, opacity);
            }
        }
    }

    /// "Source over" onto straight-alpha pixels.
    fn blend(&mut self, x: u32, y: u32, color: [u8; 4], opacity: f32) {
        if x >= self.width || y >= self.height || opacity <= 0.0 {
            return;
        }
        let index = ((y * self.width + x) * 4) as usize;
        let destination_alpha = f32::from(self.data[index + 3]) / 255.0;
        let source_alpha = opacity.min(1.0);
        let out_alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
        if out_alpha <= 0.0 {
            return;
        }
        for (slot, source) in self.data[index..index + 3].iter_mut().zip(color) {
            let destination = f32::from(*slot);
            let mixed = (f32::from(source) * source_alpha + destination * destination_alpha * (1.0 - source_alpha)) / out_alpha;
            *slot = mixed.round().clamp(0.0, 255.0) as u8;
        }
        self.data[index + 3] = (out_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    }
}

/// Colour and strength a line is drawn with.
#[derive(Clone, Copy)]
struct Ink {
    color: [u8; 4],
    opacity: f32,
}

fn draw_line(canvas: &mut Canvas, font: &FontVec, scaled: &ab_glyph::PxScaleFont<&FontVec>, line: &str, origin: (f32, f32), ink: Ink) {
    let (left, baseline) = origin;
    let mut cursor = left;
    let mut previous = None;
    for c in line.chars() {
        let id = font.glyph_id(c);
        if let Some(previous) = previous {
            cursor += scaled.kern(previous, id);
        }
        if id.0 != 0 {
            let glyph = id.with_scale_and_position(scaled.scale(), point(cursor, baseline));
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, coverage| {
                    let (x, y) = (bounds.min.x as i64 + i64::from(gx), bounds.min.y as i64 + i64::from(gy));
                    if x >= 0 && y >= 0 {
                        canvas.blend(x as u32, y as u32, ink.color, coverage * ink.opacity);
                    }
                });
            }
        }
        cursor += scaled.h_advance(id);
        previous = Some(id);
    }
}

fn line_width(line: &str, advance: &mut dyn FnMut(char) -> f32) -> f32 {
    line.chars().map(advance).sum()
}

fn is_closing_punctuation(c: char) -> bool {
    "、。，．,.!?！？）)」』】〕］｝》〉ー…‥ゝゞ々".contains(c)
}

/// Greedy line breaking: explicit newlines are kept, a Latin word stays whole, and
/// any other character may start a new line. A closing punctuation mark never
/// starts a line; it hangs past the margin instead.
fn wrap(text: &str, max_width: f32, advance: &mut dyn FnMut(char) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        let mut current = String::new();
        let mut current_width = 0.0f32;
        for token in tokens(paragraph) {
            let token_width = line_width(&token, advance);
            let is_space = token.chars().all(char::is_whitespace);
            let hangs = token.chars().next().is_some_and(is_closing_punctuation);
            if current_width + token_width > max_width && !current.is_empty() && !is_space && !hangs {
                lines.push(std::mem::take(&mut current).trim_end().to_string());
                current_width = 0.0;
            }
            if current.is_empty() && is_space {
                continue;
            }
            if token_width > max_width && !is_space {
                // A single run longer than a line: break it character by character.
                for c in token.chars() {
                    let w = advance(c);
                    if current_width + w > max_width && !current.is_empty() {
                        lines.push(std::mem::take(&mut current));
                        current_width = 0.0;
                    }
                    current.push(c);
                    current_width += w;
                }
            } else {
                current.push_str(&token);
                current_width += token_width;
            }
        }
        lines.push(current.trim_end().to_string());
    }
    while lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// Splits into breakable units: runs of spaces, runs of ASCII word characters, and
/// every other character on its own.
fn tokens(paragraph: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut kind = 0u8; // 1 = space run, 2 = ascii word
    for c in paragraph.chars() {
        let next = if c.is_whitespace() { 1 } else if c.is_ascii() { 2 } else { 0 };
        if next != 0 && next == kind {
            out.last_mut().expect("a run is open").push(c);
        } else {
            out.push(c.to_string());
        }
        kind = next;
    }
    out
}

/// `#rgb`, `#rrggbb`, `#rrggbbaa`, `0xrrggbb`, or a basic colour name. Anything else
/// falls back to `fallback` and records a warning.
fn parse_color(value: Option<&str>, fallback: [u8; 4], what: &str, warnings: &mut Vec<String>) -> [u8; 4] {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else { return fallback };
    let hex = value.strip_prefix('#').or_else(|| value.strip_prefix("0x")).or_else(|| value.strip_prefix("0X"));
    let parsed = match hex {
        Some(digits) if digits.chars().all(|c| c.is_ascii_hexdigit()) => match digits.len() {
            3 => {
                let n = |i: usize| u8::from_str_radix(&digits[i..=i], 16).map(|v| v * 17).ok();
                n(0).zip(n(1)).zip(n(2)).map(|((r, g), b)| [r, g, b, 0xff])
            }
            6 | 8 => {
                let n = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
                let alpha = if digits.len() == 8 { n(6) } else { Some(0xff) };
                n(0).zip(n(2)).zip(n(4)).zip(alpha).map(|(((r, g), b), a)| [r, g, b, a])
            }
            _ => None,
        },
        Some(_) => None,
        None => match value.to_ascii_lowercase().as_str() {
            "white" => Some([0xff, 0xff, 0xff, 0xff]),
            "black" => Some([0, 0, 0, 0xff]),
            "red" => Some([0xff, 0, 0, 0xff]),
            "green" => Some([0, 0x80, 0, 0xff]),
            "blue" => Some([0, 0, 0xff, 0xff]),
            "yellow" => Some([0xff, 0xff, 0, 0xff]),
            "orange" => Some([0xff, 0xa5, 0, 0xff]),
            "gray" | "grey" => Some([0x80, 0x80, 0x80, 0xff]),
            _ => None,
        },
    };
    parsed.unwrap_or_else(|| {
        warnings.push(format!("caption {what} {value:?} is not a colour this renderer understands; the default was used"));
        fallback
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_parse() {
        let mut warnings = Vec::new();
        assert_eq!(parse_color(Some("#fff"), [0; 4], "color", &mut warnings), [255, 255, 255, 255]);
        assert_eq!(parse_color(Some("0x111827"), [0; 4], "color", &mut warnings), [0x11, 0x18, 0x27, 255]);
        assert_eq!(parse_color(Some("#11182780"), [0; 4], "color", &mut warnings), [0x11, 0x18, 0x27, 0x80]);
        assert!(warnings.is_empty());
        assert_eq!(parse_color(Some("chartreuse"), [1, 2, 3, 4], "color", &mut warnings), [1, 2, 3, 4]);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn wraps_japanese_and_keeps_words() {
        let mut advance = |c: char| if c.is_ascii() { 5.0 } else { 10.0 };
        let lines = wrap("あいうえおかきくけこ", 50.0, &mut advance);
        assert_eq!(lines, vec!["あいうえお", "かきくけこ"]);
        let lines = wrap("hello world foo", 30.0, &mut advance);
        assert_eq!(lines, vec!["hello", "world", "foo"]);
        // A closing mark hangs instead of starting a line.
        let lines = wrap("あいうえお。", 50.0, &mut advance);
        assert_eq!(lines, vec!["あいうえお。"]);
        let lines = wrap("一行目\n二行目", 500.0, &mut advance);
        assert_eq!(lines, vec!["一行目", "二行目"]);
    }
}
