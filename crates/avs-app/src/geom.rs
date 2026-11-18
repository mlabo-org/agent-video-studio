//! Pure timeline geometry: time <-> screen x, frame rounding, timecode and ruler
//! steps. No egui types beyond plain floats, so it is unit-tested directly.

use avs_core::Time;

/// Horizontal mapping of the timeline. `scroll` is the timeline time shown at the
/// left edge of the track area (`origin_x`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeScale {
    pub origin_x: f32,
    pub px_per_sec: f64,
    pub scroll: f64,
}

pub const MIN_PX_PER_SEC: f64 = 0.5;
pub const MAX_PX_PER_SEC: f64 = 2000.0;

impl TimeScale {
    pub fn x(&self, t: Time) -> f32 {
        self.x_secs(t.secs())
    }

    pub fn x_secs(&self, seconds: f64) -> f32 {
        self.origin_x + ((seconds - self.scroll) * self.px_per_sec) as f32
    }

    pub fn secs(&self, x: f32) -> f64 {
        self.scroll + f64::from(x - self.origin_x) / self.px_per_sec
    }

    /// Timeline time under a screen x, never negative.
    pub fn time(&self, x: f32) -> Time {
        Time::from_secs(self.secs(x).max(0.0)).unwrap_or(Time::ZERO)
    }

    /// Length in time of `px` pixels.
    pub fn span(&self, px: f32) -> Time {
        Time::from_secs(f64::from(px) / self.px_per_sec).unwrap_or(Time::ZERO)
    }

    /// Zooms by `factor` keeping the time under `anchor_x` fixed.
    pub fn zoom_at(&mut self, factor: f64, anchor_x: f32) {
        let anchor = self.secs(anchor_x);
        self.px_per_sec = (self.px_per_sec * factor).clamp(MIN_PX_PER_SEC, MAX_PX_PER_SEC);
        self.scroll = (anchor - f64::from(anchor_x - self.origin_x) / self.px_per_sec).max(0.0);
    }

    /// Fits `[0, duration]` into `width` pixels with a little room at the end.
    pub fn fit(&mut self, duration: Time, width: f32) {
        let seconds = duration.secs().max(1.0) * 1.05;
        self.px_per_sec = (f64::from(width.max(50.0)) / seconds).clamp(MIN_PX_PER_SEC, MAX_PX_PER_SEC);
        self.scroll = 0.0;
    }
}

fn safe_fps(fps: f64) -> f64 {
    if fps.is_finite() && fps > 0.0 { fps } else { 30.0 }
}

/// Index of the frame that starts at or before `t` (nearest when `round`).
pub fn frame_index(t: Time, fps: f64, round: bool) -> i64 {
    let value = t.secs() * safe_fps(fps);
    if round { value.round() as i64 } else { (value + 1e-6).floor() as i64 }
}

/// Start time of frame `index`. The same index always gives the same micros, so two
/// edges quantised to one frame compare equal.
pub fn frame_time(index: i64, fps: f64) -> Time {
    Time::from_secs(index as f64 / safe_fps(fps)).unwrap_or(Time::ZERO)
}

/// Nearest frame boundary.
pub fn quantize(t: Time, fps: f64) -> Time {
    frame_time(frame_index(t, fps, true), fps)
}

/// `t` moved by `frames` whole frames from its own frame, never below zero.
pub fn step_frames(t: Time, frames: i64, fps: f64) -> Time {
    frame_time((frame_index(t, fps, true) + frames).max(0), fps)
}

/// `HH:MM:SS:FF` (non-drop).
pub fn timecode(t: Time, fps: f64) -> String {
    let fps_int = safe_fps(fps).round().max(1.0) as i64;
    let total = frame_index(t.max(Time::ZERO), fps, true);
    let frames = total % fps_int;
    let seconds = total / fps_int;
    format!("{:02}:{:02}:{:02}:{:02}", seconds / 3600, (seconds / 60) % 60, seconds % 60, frames)
}

/// Short label for a ruler tick: `M:SS` or `H:MM:SS`, frames when zoomed in.
pub fn ruler_label(seconds: f64, step: f64, fps: f64) -> String {
    let fps = safe_fps(fps);
    let whole = seconds.floor() as i64;
    let base = if whole >= 3600 { format!("{}:{:02}:{:02}", whole / 3600, (whole / 60) % 60, whole % 60) } else { format!("{}:{:02}", whole / 60, whole % 60) };
    if step < 1.0 {
        let frame = ((seconds - whole as f64) * fps).round() as i64;
        format!("{base}:{frame:02}")
    } else {
        base
    }
}

/// Major tick spacing (seconds) so labels are at least `min_px` apart.
pub fn ruler_step(px_per_sec: f64, fps: f64, min_px: f64) -> f64 {
    let frame = 1.0 / safe_fps(fps);
    let candidates = [frame, frame * 2.0, frame * 5.0, frame * 10.0, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0];
    for step in candidates {
        if step * px_per_sec >= min_px {
            return step;
        }
    }
    7200.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use avs_core::secs;

    #[test]
    fn x_and_time_round_trip() {
        let scale = TimeScale { origin_x: 100.0, px_per_sec: 50.0, scroll: 2.0 };
        assert_eq!(scale.x(secs(2.0)), 100.0);
        assert_eq!(scale.x(secs(4.0)), 200.0);
        assert_eq!(scale.time(200.0), secs(4.0));
        assert_eq!(scale.time(0.0), Time::ZERO, "never negative");
    }

    #[test]
    fn zoom_keeps_anchor() {
        let mut scale = TimeScale { origin_x: 0.0, px_per_sec: 10.0, scroll: 0.0 };
        let before = scale.secs(300.0);
        scale.zoom_at(2.0, 300.0);
        assert!((scale.secs(300.0) - before).abs() < 1e-9);
        assert_eq!(scale.px_per_sec, 20.0);
    }

    #[test]
    fn quantized_edges_compare_equal() {
        let a = quantize(secs(1.0 / 30.0 * 7.0 + 0.004), 30.0);
        let b = frame_time(7, 30.0);
        assert_eq!(a, b);
        assert_eq!(step_frames(b, -10, 30.0), Time::ZERO);
        assert_eq!(step_frames(b, 1, 30.0), frame_time(8, 30.0));
    }

    #[test]
    fn timecode_format() {
        assert_eq!(timecode(secs(0.0), 30.0), "00:00:00:00");
        assert_eq!(timecode(secs(61.5), 30.0), "00:01:01:15");
        assert_eq!(timecode(secs(3725.0), 25.0), "01:02:05:00");
    }

    #[test]
    fn ruler_steps_grow_with_zoom_out() {
        assert!(ruler_step(1000.0, 30.0, 80.0) < 1.0);
        assert_eq!(ruler_step(10.0, 30.0, 80.0), 10.0);
        assert_eq!(ruler_label(65.0, 5.0, 30.0), "1:05");
    }
}
