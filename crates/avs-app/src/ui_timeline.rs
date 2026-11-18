//! The timeline: ruler, track headers and clips, drawn with the painter and hit-
//! tested by hand so only what is visible is drawn and one widget owns the
//! pointer. Gestures are turned into operations by `edit`.

use crate::app::{AvsApp, Gesture, StatusKind};
use crate::edit::{self, DragInput, DragKind, PlaceMode, Tool, Zone};
use crate::geom;
use crate::keys::Command;
use crate::media_cache::{PEAKS_PER_SECOND, STRIP_H, STRIP_W};
use crate::theme::{Palette, palette};
use avs_core::ops::{AddTrack, BladeAtTime, DeleteTrack, ReorderTrack, SetTrackMuted, SetTrackProperties, SplitClip};
use avs_core::{Clip, ClipKind, MediaType, Operation, Project, Time, TrackKind};
use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Vec2};

pub const HEADER_W: f32 = 168.0;
pub const RULER_H: f32 = 26.0;
const EDGE_PX: f32 = 7.0;

/// Payload of a media item dragged from the library.
#[derive(Debug, Clone)]
pub struct MediaDrag(pub String);

/// Where things were drawn last frame (for drops from Finder and the self-test).
#[derive(Debug, Clone)]
pub struct Layout {
    pub tracks_rect: Rect,
    /// Track id and its row rectangle (full width, without the header).
    pub rows: Vec<(String, Rect)>,
}

/// A padlock: body plus shackle (closed when locked, open when not).
fn paint_padlock(painter: &egui::Painter, rect: Rect, color: Color32, locked: bool) {
    let c = rect.center();
    let s = rect.height().min(rect.width()) * 0.5;
    let body = Rect::from_center_size(Pos2::new(c.x, c.y + s * 0.25), Vec2::new(s * 1.1, s * 0.8));
    painter.rect_filled(body, 1.5, color);
    let (left, right) = (body.left() + s * 0.2, body.right() - s * 0.2);
    let top = body.top() - s * 0.55;
    let stroke = Stroke::new(1.6_f32, color);
    let right_bottom = if locked { body.top() } else { body.top() - s * 0.3 };
    painter.line_segment([Pos2::new(left, body.top()), Pos2::new(left, top)], stroke);
    painter.line_segment([Pos2::new(left, top), Pos2::new(right, top)], stroke);
    painter.line_segment([Pos2::new(right, top), Pos2::new(right, right_bottom)], stroke);
}

impl Default for Layout {
    fn default() -> Self {
        Layout { tracks_rect: Rect::NOTHING, rows: Vec::new() }
    }
}

pub fn row_height(kind: TrackKind, compact: bool) -> f32 {
    match (kind, compact) {
        (TrackKind::Video, false) => 66.0,
        (TrackKind::Audio, false) => 50.0,
        (TrackKind::Caption, false) => 34.0,
        (TrackKind::Video, true) => 42.0,
        (TrackKind::Audio, true) => 32.0,
        (TrackKind::Caption, true) => 24.0,
    }
}

struct Hit {
    clip: Option<(String, Zone)>,
    time: Time,
}

fn clip_color(palette: &Palette, project: &Project, clip: &Clip) -> Color32 {
    match clip.kind {
        ClipKind::Caption => palette.caption_clip,
        ClipKind::Media => match project.media_type_of(clip) {
            Some(MediaType::Audio) => palette.audio_clip,
            Some(MediaType::Image | MediaType::Sequence) => palette.still_clip,
            _ if project.track(&clip.track_id).is_ok_and(|track| track.kind == TrackKind::Audio) => palette.audio_clip,
            _ => palette.video_clip,
        },
    }
}

impl AvsApp {
    pub fn timeline_drop_target(&self, pos: Pos2) -> Option<(String, Time)> {
        if !self.layout.tracks_rect.contains(pos) {
            return None;
        }
        let track = self.layout.rows.iter().find(|(_, rect)| rect.y_range().contains(pos.y)).map(|(id, _)| id.clone())?;
        Some((track, geom::quantize(self.scale.time(pos.x), self.fps())))
    }

    fn hit_test(&self, project: &Project, pos: Pos2) -> Hit {
        let time = self.scale.time(pos.x);
        let row = self.layout.rows.iter().find(|(_, rect)| rect.y_range().contains(pos.y));
        let Some((track_id, _)) = row else { return Hit { clip: None, time } };
        let Ok(track) = project.track(track_id) else { return Hit { clip: None, time } };
        // Closest clip edge wins near a cut between two clips.
        let mut best: Option<(String, Zone, f32)> = None;
        for clip in &track.clips {
            let (x0, x1) = (self.scale.x(clip.start_seconds), self.scale.x(clip.end()));
            if pos.x < x0 - EDGE_PX || pos.x > x1 + EDGE_PX {
                continue;
            }
            let edge = EDGE_PX.min((x1 - x0) / 3.0);
            // Just outside an edge also grabs it when that side is empty; where another
            // clip is there, each side of the cut belongs to its own clip.
            let outside = pos.x < x0 || pos.x > x1;
            let on_neighbour = outside && track.clips.iter().any(|other| other.id != clip.id && pos.x >= self.scale.x(other.start_seconds) && pos.x <= self.scale.x(other.end()));
            let reach = if on_neighbour { 1.0 } else { EDGE_PX };
            let candidates = [(Zone::Start, (pos.x - x0).abs()), (Zone::End, (pos.x - x1).abs())];
            for (zone, distance) in candidates {
                let inside_side = match zone {
                    Zone::Start => pos.x >= x0 - reach,
                    Zone::End => pos.x <= x1 + reach,
                    Zone::Body => true,
                };
                let limit = if outside { reach } else { edge };
                if distance <= limit && inside_side && best.as_ref().is_none_or(|(_, _, d)| distance < *d) {
                    best = Some((clip.id.clone(), zone, distance));
                }
            }
            if best.is_none() && pos.x >= x0 && pos.x <= x1 {
                best = Some((clip.id.clone(), Zone::Body, f32::MAX));
            }
        }
        Hit { clip: best.map(|(id, zone, _)| (id, zone)), time }
    }

    pub fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        let Some(project) = self.view_project().cloned() else {
            ui.centered_and_justified(|ui| ui.label(self.tr("プロジェクトを読み込めません", "The project could not be read")));
            return;
        };
        let palette = palette(ui.visuals().dark_mode);
        self.timeline_controls_ui(ui, &project);
        let canvas = ui.available_rect_before_wrap();
        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, 0.0, palette.timeline_bg);

        let tracks_left = canvas.left() + HEADER_W;
        self.scale.origin_x = tracks_left;
        if self.fit_requested {
            self.scale.fit(project.duration(), canvas.width() - HEADER_W - 20.0);
            self.view.px_per_sec = self.scale.px_per_sec;
            self.fit_requested = false;
        }

        let ruler_rect = Rect::from_min_max(Pos2::new(tracks_left, canvas.top()), Pos2::new(canvas.right(), canvas.top() + RULER_H));
        let tracks_rect = Rect::from_min_max(Pos2::new(tracks_left, canvas.top() + RULER_H), canvas.max);
        let header_rect = Rect::from_min_max(Pos2::new(canvas.left(), canvas.top() + RULER_H), Pos2::new(tracks_left, canvas.bottom()));

        // Rows
        let total_height: f32 = project.tracks.iter().map(|track| row_height(track.kind, self.view.compact_tracks)).sum();
        let max_scroll = (total_height - tracks_rect.height() + 24.0).max(0.0);
        self.track_scroll = self.track_scroll.clamp(0.0, max_scroll);
        let mut y = tracks_rect.top() - self.track_scroll;
        let mut rows: Vec<(String, Rect)> = Vec::new();
        for track in &project.tracks {
            let height = row_height(track.kind, self.view.compact_tracks);
            rows.push((track.id.clone(), Rect::from_min_size(Pos2::new(tracks_left, y), Vec2::new(tracks_rect.width(), height))));
            y += height;
        }
        self.layout = Layout { tracks_rect, rows: rows.clone() };
        self.targets.insert("ruler".into(), ruler_rect);
        self.targets.insert("tracks".into(), tracks_rect);
        for (id, rect) in &rows {
            self.targets.insert(format!("track:{id}"), *rect);
        }

        // Interaction (before drawing, so the drawn frame shows this frame's result).
        let tracks_response = ui.interact(tracks_rect, ui.id().with("tracks"), Sense::click_and_drag());
        let ruler_response = ui.interact(ruler_rect, ui.id().with("ruler"), Sense::click_and_drag());
        self.handle_scroll_and_zoom(ui, canvas);
        self.handle_ruler(&ruler_response);
        self.handle_tracks(ui, &tracks_response);
        let drop_ghost = self.handle_media_drop(&tracks_response);
        self.timeline_context_menu(&tracks_response);

        // Keep the playhead visible while playing and whenever it moved (a key, a
        // paste, an undo that shortened the project). Scrolling the timeline away
        // from a resting playhead stays possible.
        let moved = std::mem::replace(&mut self.shown_playhead, self.playhead) != self.playhead;
        // Not while the pointer itself is placing the playhead (scrubbing, a drag).
        let moved = moved && !self.scrubbing && self.gesture.is_none();
        if self.player.is_playing() || moved {
            let x = self.scale.x(self.playhead);
            let width = f64::from(tracks_rect.width());
            if x < tracks_left || (self.player.is_playing() && x > tracks_rect.right() - 30.0) {
                self.scale.scroll = (self.playhead.secs() - width * 0.1 / self.scale.px_per_sec).max(0.0);
            } else if x > tracks_rect.right() {
                // A jump to the right keeps what is before the playhead in view.
                self.scale.scroll = (self.playhead.secs() - width * 0.85 / self.scale.px_per_sec).max(0.0);
            }
        }

        let project = self.view_project().cloned().unwrap_or(project);
        let painter = painter.with_clip_rect(canvas);
        let visible = (self.scale.time(tracks_left), self.scale.time(canvas.right()));

        // Track backgrounds and clips.
        for (index, (track_id, row)) in rows.iter().enumerate() {
            if row.bottom() < tracks_rect.top() || row.top() > tracks_rect.bottom() {
                continue;
            }
            let Ok(track) = project.track(track_id) else { continue };
            let row_painter = painter.with_clip_rect(row.intersect(tracks_rect));
            row_painter.rect_filled(*row, 0.0, if index % 2 == 0 { palette.track_bg } else { palette.track_bg_alt });
            row_painter.line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(1.0_f32, palette.timeline_bg));
            for clip in track.clips.iter().filter(|clip| clip.end() >= visible.0 && clip.start_seconds <= visible.1) {
                let shown = self.draw_clip(&row_painter, &project, clip, *row, &palette, track.is_muted());
                // Registered for the self-test (only the part on screen can be clicked).
                if shown.is_positive() {
                    self.targets.insert(format!("clip:{}", clip.id), shown);
                }
            }
            if track.is_locked() {
                // Diagonal stripes over the whole row: nothing here can be changed.
                row_painter.rect_filled(*row, 0.0, palette.locked_overlay);
                let mut x = row.left() - row.height();
                let stripes = Stroke::new(1.0_f32, palette.locked_overlay.gamma_multiply(2.0));
                while x < row.right() {
                    row_painter.line_segment([Pos2::new(x, row.bottom()), Pos2::new(x + row.height(), row.top())], stripes);
                    x += 12.0;
                }
            }
        }

        // Range, marquee, drop ghost, snap line.
        let overlay = painter.with_clip_rect(tracks_rect.union(ruler_rect));
        if let Some((start, end)) = self.range() {
            let rect = Rect::from_x_y_ranges(self.scale.x(start)..=self.scale.x(end), tracks_rect.y_range());
            overlay.rect_filled(rect, 0.0, palette.range_fill);
            for x in [rect.left(), rect.right()] {
                overlay.line_segment([Pos2::new(x, ruler_rect.top()), Pos2::new(x, tracks_rect.bottom())], Stroke::new(1.5_f32, palette.range_edge));
            }
        } else if let Some(start) = self.mark_in {
            let x = self.scale.x(start);
            overlay.line_segment([Pos2::new(x, ruler_rect.top()), Pos2::new(x, tracks_rect.bottom())], Stroke::new(1.5_f32, palette.range_edge));
        }
        if let Some((a, b)) = self.marquee {
            let rect = Rect::from_two_pos(a, b);
            overlay.rect_filled(rect, 2.0, palette.marquee);
            overlay.rect_stroke(rect, 2.0, Stroke::new(1.0_f32, ui.visuals().selection.bg_fill), StrokeKind::Inside);
        }
        if let Some(rect) = drop_ghost {
            overlay.rect_filled(rect, 4.0, palette.drop_target);
            overlay.rect_stroke(rect, 4.0, Stroke::new(1.5_f32, ui.visuals().selection.bg_fill), StrokeKind::Inside);
        }
        if let Some(at) = self.gesture.as_ref().and_then(|gesture| gesture.snapped) {
            let x = self.scale.x(at);
            overlay.line_segment([Pos2::new(x, ruler_rect.top()), Pos2::new(x, tracks_rect.bottom())], Stroke::new(1.5_f32, palette.snap_line));
        }

        self.draw_ruler(&painter, ruler_rect, &palette, project.fps);
        self.track_headers_ui(ui, &project, header_rect, &rows, &palette);

        // Playhead on top of everything.
        let x = self.scale.x(self.playhead);
        if x >= tracks_left - 1.0 && x <= canvas.right() + 1.0 {
            painter.line_segment([Pos2::new(x, ruler_rect.top()), Pos2::new(x, canvas.bottom())], Stroke::new(1.5_f32, palette.playhead));
            let head = vec![Pos2::new(x - 6.0, ruler_rect.top()), Pos2::new(x + 6.0, ruler_rect.top()), Pos2::new(x, ruler_rect.top() + 9.0)];
            painter.add(Shape::convex_polygon(head, palette.playhead, Stroke::NONE));
        }
        // The corner above the headers: the playhead timecode.
        let corner = Rect::from_min_max(canvas.min, Pos2::new(tracks_left, canvas.top() + RULER_H));
        painter.rect_filled(corner, 0.0, palette.header_bg);
        painter.text(corner.center(), Align2::CENTER_CENTER, geom::timecode(self.playhead, project.fps), FontId::monospace(13.0), palette.text);

        // Cursor feedback.
        if let Some(pos) = tracks_response.hover_pos() {
            let hit = self.hit_test(&project, pos);
            let icon = match (self.tool, hit.clip.as_ref().map(|(_, zone)| *zone)) {
                (Tool::Blade, Some(_)) => CursorIcon::Crosshair,
                (Tool::Range, _) => CursorIcon::Text,
                (_, Some(Zone::Start | Zone::End)) => CursorIcon::ResizeHorizontal,
                (Tool::Trim, Some(Zone::Body)) => CursorIcon::ResizeColumn,
                (_, Some(Zone::Body)) if self.gesture.is_some() => CursorIcon::Grabbing,
                _ => CursorIcon::Default,
            };
            ui.ctx().set_cursor_icon(icon);
            if self.tool == Tool::Blade && hit.clip.is_some() {
                let x = self.scale.x(geom::quantize(hit.time, project.fps));
                if let Some((_, row)) = self.layout.rows.iter().find(|(_, rect)| rect.y_range().contains(pos.y)) {
                    painter.line_segment([Pos2::new(x, row.top()), Pos2::new(x, row.bottom())], Stroke::new(1.0_f32, palette.playhead));
                }
            }
        }
        ui.allocate_rect(canvas, Sense::hover());
    }

    fn timeline_controls_ui(&mut self, ui: &mut egui::Ui, project: &Project) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(self.tr("タイムライン", "Timeline")).strong());
            ui.separator();
            for (kind, ja, en) in [(TrackKind::Video, "＋映像トラック", "+ Video Track"), (TrackKind::Audio, "＋音声トラック", "+ Audio Track"), (TrackKind::Caption, "＋字幕トラック", "+ Caption Track")] {
                let button = ui.button(self.tr(ja, en)).on_hover_text(self.tr("トラックを追加します", "Adds a track"));
                self.targets.insert(format!("button:add-{kind:?}-track").to_lowercase(), button.rect);
                if button.clicked() {
                    let first_video = project.tracks.iter().find(|track| track.kind == TrackKind::Video).map(|track| track.id.clone());
                    let insert_before = if kind == TrackKind::Video { first_video } else { None };
                    self.commit(vec![Operation::AddTrack(AddTrack { id: None, kind, name: None, insert_before_track_id: insert_before, insert_after_track_id: None, locked: None, muted: None })]);
                }
            }
            ui.separator();
            if ui.button(self.tr("字幕を追加", "Add Caption")).on_hover_text(Command::AddCaption.tooltip(self.lang)).clicked() {
                self.add_caption_at_playhead();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let compact = ui.add(egui::Button::new(self.tr("低く", "Compact")).selected(self.view.compact_tracks)).on_hover_text(Command::ToggleTrackHeight.label(self.lang));
                self.targets.insert("button:compact-tracks".into(), compact.rect);
                if compact.clicked() {
                    self.view.compact_tracks = !self.view.compact_tracks;
                }
                if ui.button(self.tr("全体", "Fit")).on_hover_text(Command::ZoomToFit.tooltip(self.lang)).clicked() {
                    self.fit_requested = true;
                }
                if ui.button("＋").on_hover_text(Command::ZoomIn.tooltip(self.lang)).clicked() {
                    self.zoom(1.5);
                }
                let mut zoom = self.scale.px_per_sec.log10();
                let slider = ui.add(egui::Slider::new(&mut zoom, geom::MIN_PX_PER_SEC.log10()..=geom::MAX_PX_PER_SEC.log10()).show_value(false));
                if slider.changed() {
                    let factor = 10f64.powf(zoom) / self.scale.px_per_sec;
                    self.zoom(factor);
                }
                if ui.button("−").on_hover_text(Command::ZoomOut.tooltip(self.lang)).clicked() {
                    self.zoom(1.0 / 1.5);
                }
                ui.label(egui::RichText::new(format!("{}  {}", self.tr("長さ", "Duration"), geom::timecode(project.duration(), project.fps))).monospace());
            });
        });
    }

    fn handle_scroll_and_zoom(&mut self, ui: &egui::Ui, canvas: Rect) {
        let hovered = ui.rect_contains_pointer(canvas);
        if !hovered {
            return;
        }
        let (scroll, zoom, modifiers, pointer) = ui.input(|input| (input.smooth_scroll_delta, input.zoom_delta(), input.modifiers, input.pointer.hover_pos()));
        let anchor = pointer.map_or(self.scale.x(self.playhead), |pos| pos.x.max(self.scale.origin_x));
        if (zoom - 1.0).abs() > 1e-4 {
            self.scale.zoom_at(f64::from(zoom), anchor);
            self.view.px_per_sec = self.scale.px_per_sec;
        } else if modifiers.command || modifiers.alt {
            if scroll.y.abs() > 0.0 {
                self.scale.zoom_at(f64::from((scroll.y / 200.0).exp()), anchor);
                self.view.px_per_sec = self.scale.px_per_sec;
            }
        } else {
            let (dx, dy) = if modifiers.shift { (scroll.y, 0.0) } else { (scroll.x, scroll.y) };
            if dx.abs() > 0.0 {
                self.scale.scroll = (self.scale.scroll - f64::from(dx) / self.scale.px_per_sec).max(0.0);
            }
            if dy.abs() > 0.0 {
                self.track_scroll -= dy;
            }
        }
    }

    fn handle_ruler(&mut self, response: &egui::Response) {
        if response.is_pointer_button_down_on() {
            if let Some(pos) = response.interact_pointer_pos() {
                if self.player.is_playing() {
                    self.player.stop();
                }
                self.scrubbing = true;
                self.playhead = geom::quantize(self.scale.time(pos.x), self.fps()).min(self.store.project().map_or(Time::ZERO, Project::duration));
            }
        } else {
            self.scrubbing = false;
        }
    }

    fn handle_tracks(&mut self, ui: &egui::Ui, response: &egui::Response) {
        let Some(project) = self.store.project().cloned() else { return };
        let modifiers = ui.input(|input| input.modifiers);
        let fps = project.fps;

        if response.drag_started() {
            let origin = ui.input(|input| input.pointer.press_origin()).or(response.interact_pointer_pos());
            if let Some(origin) = origin {
                let hit = self.hit_test(&project, origin);
                match (self.tool, &hit.clip) {
                    (Tool::Range, _) => {
                        self.mark_in = Some(geom::quantize(hit.time, fps));
                        self.mark_out = None;
                        self.marquee = Some((origin, origin));
                    }
                    (Tool::Select | Tool::Trim, Some((clip_id, zone))) => {
                        if !self.selection.contains(clip_id) && *zone == Zone::Body && self.tool == Tool::Select {
                            self.select_clip(clip_id, false, modifiers.shift || modifiers.command);
                        }
                        let locked = project.clip(clip_id).ok().and_then(|clip| project.track(&clip.track_id).ok()).is_some_and(|track| track.is_locked());
                        if locked {
                            self.set_status(StatusKind::Info, self.tr("ロックされたトラックのクリップは動かせません", "Clips on a locked track cannot be changed"));
                        } else if let Some(kind) = edit::drag_kind(&project, self.tool, modifiers.alt, clip_id, *zone, &self.selection) {
                            self.gesture = Some(Gesture { kind, origin_x: origin.x, operation: None, snapped: None });
                        }
                    }
                    (Tool::Select | Tool::Trim, None) => {
                        if !(modifiers.shift || modifiers.command) {
                            self.selection.clear();
                        }
                        self.marquee = Some((origin, origin));
                    }
                    (Tool::Blade, _) => {}
                }
            }
        }

        if response.dragged()
            && let Some(pos) = response.interact_pointer_pos() {
                if let Some(gesture) = self.gesture.clone() {
                    let raw_delta = Time::from_secs(f64::from(pos.x - gesture.origin_x) / self.scale.px_per_sec).unwrap_or(Time::ZERO);
                    let hover_track = self.layout.rows.iter().find(|(_, rect)| rect.y_range().contains(pos.y)).map(|(id, _)| id.clone());
                    let range = self.range();
                    let input = DragInput {
                        project: &project,
                        kind: &gesture.kind,
                        raw_delta,
                        hover_track: hover_track.as_deref(),
                        fps,
                        snap: self.view.snap != modifiers.ctrl,
                        snap_threshold: self.scale.span(9.0),
                        playhead: self.playhead,
                        range,
                    };
                    match edit::drag_operation(&input) {
                        Some((operations, snapped)) => {
                            let mut attempt = (operations.clone(), snapped);
                            let mut result = self.preview_operations(attempt.0.clone());
                            // Dropped onto packed clips of its own track: reorder instead.
                            if let (Err(error), DragKind::Move { clip_ids, anchor }, [Operation::MoveClips(moved)]) = (&result, &gesture.kind, operations.as_slice())
                                && error.code == "CLIP_OVERLAP" {
                                    let proposed = project.clip(anchor).map(|clip| clip.start_seconds + moved.delta_seconds.unwrap_or(Time::ZERO));
                                    if let Some((operations, at)) = proposed.ok().and_then(|start| edit::reorder_operations(&project, clip_ids, anchor, start)) {
                                        attempt = (operations, Some(at));
                                        result = self.preview_operations(attempt.0.clone());
                                    }
                                }
                            match result {
                                Ok(()) => {
                                    if let Some(current) = &mut self.gesture {
                                        current.operation = Some(attempt.0);
                                        current.snapped = attempt.1;
                                    }
                                }
                                Err(error) => {
                                    // Say why it stops here, and keep showing the last allowed position.
                                    self.set_status(StatusKind::Info, format!("{}: {}", error.code, error.message));
                                    match gesture.operation.clone() {
                                        Some(last) => {
                                            let _ = self.preview_operations(last);
                                        }
                                        None => self.cancel_preview(),
                                    }
                                }
                            }
                        }
                        None => {
                            self.cancel_preview();
                            if let Some(current) = &mut self.gesture {
                                current.operation = None;
                                current.snapped = None;
                            }
                        }
                    }
                } else if let Some((origin, _)) = self.marquee {
                    self.marquee = Some((origin, pos));
                    if self.tool == Tool::Range {
                        let (a, b) = (self.scale.time(origin.x.min(pos.x)), self.scale.time(origin.x.max(pos.x)));
                        self.mark_in = Some(geom::quantize(a, fps));
                        self.mark_out = Some(geom::quantize(b, fps));
                    } else {
                        let rect = Rect::from_two_pos(origin, pos);
                        let tracks: Vec<String> = self.layout.rows.iter().filter(|(_, row)| row.y_range().intersects(rect.y_range())).map(|(id, _)| id.clone()).collect();
                        let found = edit::clips_in_box(&project, self.scale.time(rect.left()), self.scale.time(rect.right()), &tracks);
                        let mut selection = project.with_companions(&found);
                        if modifiers.shift || modifiers.command {
                            for id in std::mem::take(&mut self.selection) {
                                if !selection.contains(&id) {
                                    selection.push(id);
                                }
                            }
                        }
                        self.selection = selection;
                    }
                }
            }

        if response.drag_stopped() {
            if let Some(gesture) = self.gesture.take() {
                self.preview = None;
                match gesture.operation {
                    Some(operations) => {
                        self.preview_ops = None;
                        self.commit(operations);
                    }
                    None => self.cancel_preview(),
                }
            }
            self.marquee = None;
        }

        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos() {
                let hit = self.hit_test(&project, pos);
                match (self.tool, hit.clip) {
                    (Tool::Blade, Some((clip_id, _))) => {
                        let at = geom::quantize(hit.time, fps);
                        let operation = if modifiers.shift {
                            Operation::BladeAtTime(BladeAtTime { at_seconds: at, track_ids: None, clip_ids: None })
                        } else {
                            Operation::SplitClip(SplitClip { clip_id, at_seconds: at })
                        };
                        self.commit(vec![operation]);
                    }
                    (Tool::Range, _) => {
                        self.mark_in = None;
                        self.mark_out = None;
                    }
                    (_, Some((clip_id, _))) => {
                        self.select_clip(&clip_id, modifiers.command, modifiers.shift);
                        // As in Final Cut Pro with skimming off, a click also puts the playhead there.
                        if !(modifiers.command || modifiers.shift) {
                            if self.player.is_playing() {
                                self.player.stop();
                            }
                            self.playhead = geom::quantize(hit.time, fps).min(project.duration());
                        }
                    }
                    (_, None) => {
                        if !(modifiers.command || modifiers.shift) {
                            self.selection.clear();
                        }
                        if self.player.is_playing() {
                            self.player.stop();
                        }
                        self.playhead = geom::quantize(hit.time, fps).min(project.duration());
                    }
                }
            }

        if response.secondary_clicked()
            && let Some(pos) = response.interact_pointer_pos() {
                let hit = self.hit_test(&project, pos);
                if let Some((clip_id, _)) = &hit.clip
                    && !self.selection.contains(clip_id) {
                        self.select_clip(clip_id, false, false);
                    }
                self.context_at = Some((hit.clip.map(|(id, _)| id), geom::quantize(hit.time, fps)));
            }
    }

    fn timeline_context_menu(&mut self, response: &egui::Response) {
        let lang = self.lang;
        response.context_menu(|ui| {
            let Some((clip, at)) = self.context_at.clone() else {
                ui.close();
                return;
            };
            let has_clip = clip.is_some();
            if ui.add_enabled(has_clip, egui::Button::new(lang.pick("ここで分割", "Split Here"))).clicked() {
                if let Some(clip_id) = &clip {
                    self.commit(vec![Operation::SplitClip(SplitClip { clip_id: clip_id.clone(), at_seconds: at })]);
                }
                ui.close();
            }
            let items = [Command::BladeAtPlayhead, Command::DeleteLeaveGap, Command::DeleteRipple, Command::ToggleClipEnabled, Command::Copy, Command::Duplicate];
            for command in items {
                let enabled = has_clip || command == Command::BladeAtPlayhead;
                if ui.add_enabled(enabled, egui::Button::new(command.label(lang)).shortcut_text(command.shortcut())).clicked() {
                    let ctx = ui.ctx().clone();
                    self.run(command, &ctx);
                    ui.close();
                }
            }
            ui.separator();
            if ui.add_enabled(has_clip, egui::Button::new(lang.pick("プロパティ（インスペクタ）", "Properties (Inspector)"))).clicked() {
                self.inspector_flash = Some(std::time::Instant::now());
                ui.close();
            }
            if ui.button(Command::Paste.label(lang)).clicked() {
                let ctx = ui.ctx().clone();
                self.run(Command::Paste, &ctx);
                ui.close();
            }
        });
    }

    /// Media dragged from the library: a ghost while hovering, placement on release.
    fn handle_media_drop(&mut self, response: &egui::Response) -> Option<Rect> {
        let project = self.store.project()?.clone();
        let pos = response.hover_pos()?;
        let hit_track = self.layout.rows.iter().find(|(_, rect)| rect.y_range().contains(pos.y)).cloned();
        let fps = project.fps;
        if let Some(payload) = response.dnd_release_payload::<MediaDrag>() {
            let media = project.media(&payload.0).ok()?.clone();
            let kind = edit::track_kind_for(media.media_type);
            let track = hit_track.as_ref().map(|(id, _)| id.clone()).filter(|id| project.track(id).is_ok_and(|track| track.kind == kind));
            let at = geom::quantize(self.scale.time(pos.x), fps);
            self.selected_media = Some(media.id.clone());
            self.place_media(&media.id, PlaceMode::At, track, at);
            return None;
        }
        let payload = response.dnd_hover_payload::<MediaDrag>()?;
        let media = project.media(&payload.0).ok()?;
        let (_, row) = hit_track?;
        let start = self.scale.x(geom::quantize(self.scale.time(pos.x), fps));
        let end = self.scale.x(geom::quantize(self.scale.time(pos.x), fps) + edit::media_length(media));
        Some(Rect::from_x_y_ranges(start..=end, row.y_range()))
    }

    /// Draws one clip and returns the part of it that is on screen.
    fn draw_clip(&self, painter: &egui::Painter, project: &Project, clip: &Clip, row: Rect, palette: &Palette, track_muted: bool) -> Rect {
        let x0 = self.scale.x(clip.start_seconds);
        let x1 = self.scale.x(clip.end()).max(x0 + 2.0);
        let rect = Rect::from_min_max(Pos2::new(x0, row.top() + 2.0), Pos2::new(x1, row.bottom() - 2.0));
        let visible = rect.intersect(painter.clip_rect());
        let color = clip_color(palette, project, clip);
        painter.rect_filled(rect, 4.0, color);
        let clip_painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
        let name_h = 16.0_f32.min(rect.height() * 0.45);
        let body = Rect::from_min_max(Pos2::new(rect.left(), rect.top() + name_h), rect.max);
        let media = clip.media_id.as_deref().and_then(|id| project.media(id).ok());
        let on_audio_track = project.track(&clip.track_id).is_ok_and(|track| track.kind == TrackKind::Audio);

        match (clip.kind, media) {
            (ClipKind::Media, Some(media)) if !on_audio_track && media.media_type != MediaType::Audio => {
                // Filmstrip: tiles of the picture at the time each tile starts.
                let tile_w = (body.height() * STRIP_W as f32 / STRIP_H as f32).max(8.0);
                let first = ((clip_painter.clip_rect().left() - body.left()) / tile_w).floor().max(0.0) as i32;
                let last = ((clip_painter.clip_rect().right() - body.left()) / tile_w).ceil() as i32;
                for index in first..=last {
                    let left = body.left() + index as f32 * tile_w;
                    if left > body.right() {
                        break;
                    }
                    let tile = Rect::from_min_size(Pos2::new(left, body.top()), Vec2::new(tile_w, body.height()));
                    let timeline = self.scale.secs(left).max(clip.start_seconds.secs());
                    let media_time = clip.source_start().secs() + (timeline - clip.start_seconds.secs()) * clip.speed();
                    if let Some(texture) = self.cache.frame_at(&media.id, media_time) {
                        clip_painter.image(texture.id(), tile, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                    }
                }
            }
            (ClipKind::Media, Some(media)) if media.media_type != MediaType::Image => {
                if let Some(peaks) = self.cache.wave(&media.id) {
                    let mid = body.center().y;
                    let half = body.height() * 0.46;
                    let gain = 10f32.powf(clip.properties.volume_db.unwrap_or(0.0) as f32 / 20.0).min(2.0);
                    let left = clip_painter.clip_rect().left().max(body.left()).floor() as i32;
                    let right = clip_painter.clip_rect().right().min(body.right()).ceil() as i32;
                    let speed = clip.speed();
                    let source = clip.source_start().secs();
                    let start = clip.start_seconds.secs();
                    let mut shapes = Vec::new();
                    let mut x = left;
                    while x < right {
                        let a = source + (self.scale.secs(x as f32) - start) * speed;
                        let b = source + (self.scale.secs(x as f32 + 2.0) - start) * speed;
                        let (i0, i1) = ((a * f64::from(PEAKS_PER_SECOND)).max(0.0) as usize, ((b * f64::from(PEAKS_PER_SECOND)).ceil().max(0.0) as usize).max(1));
                        let peak = peaks.get(i0.min(peaks.len())..i1.min(peaks.len())).and_then(|slice| slice.iter().max().copied()).unwrap_or(0);
                        let height = (f32::from(peak) / 255.0 * gain).min(1.0) * half;
                        if height > 0.5 {
                            shapes.push(Shape::line_segment([Pos2::new(x as f32, mid - height), Pos2::new(x as f32, mid + height)], Stroke::new(1.6_f32, palette.waveform)));
                        }
                        x += 2;
                    }
                    clip_painter.extend(shapes);
                }
            }
            (ClipKind::Caption, _) => {
                let text = clip.caption.as_ref().map(|caption| caption.text.clone()).unwrap_or_default();
                clip_painter.text(Pos2::new(visible.left().max(rect.left()) + 6.0, body.center().y), Align2::LEFT_CENTER, text.replace('\n', " "), FontId::proportional(12.0), palette.clip_text);
            }
            _ => {}
        }

        // Name strip.
        let name_rect = Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.top() + name_h));
        clip_painter.rect_filled(name_rect, egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, color.gamma_multiply(0.75));
        let mut label = clip.name.clone();
        if (clip.speed() - 1.0).abs() > 1e-6 {
            label = format!("{label}  ×{:.2}", clip.speed());
        }
        if clip.kind == ClipKind::Caption {
            label = String::new();
        }
        clip_painter.text(Pos2::new(visible.left() + 5.0, name_rect.center().y), Align2::LEFT_CENTER, label, FontId::proportional(11.0), palette.clip_text);

        // Fades: a darkened corner from the clip edge to where the fade ends.
        let fade_in = self.scale.px_per_sec as f32 * clip.fade_in().secs() as f32;
        let fade_out = self.scale.px_per_sec as f32 * clip.fade_out().secs() as f32;
        if fade_in > 1.0 {
            let points = vec![Pos2::new(rect.left(), body.top()), Pos2::new(rect.left() + fade_in, body.top()), Pos2::new(rect.left(), rect.bottom())];
            clip_painter.add(Shape::convex_polygon(points, palette.fade, Stroke::NONE));
            clip_painter.line_segment([Pos2::new(rect.left(), rect.bottom()), Pos2::new(rect.left() + fade_in, body.top())], Stroke::new(1.0_f32, palette.clip_text));
        }
        if fade_out > 1.0 {
            let points = vec![Pos2::new(rect.right() - fade_out, body.top()), Pos2::new(rect.right(), body.top()), Pos2::new(rect.right(), rect.bottom())];
            clip_painter.add(Shape::convex_polygon(points, palette.fade, Stroke::NONE));
            clip_painter.line_segment([Pos2::new(rect.right() - fade_out, body.top()), Pos2::new(rect.right(), rect.bottom())], Stroke::new(1.0_f32, palette.clip_text));
        }

        if clip.properties.muted == Some(true) || track_muted {
            clip_painter.rect_filled(rect, 4.0, palette.muted_overlay);
            if clip.properties.muted == Some(true) {
                clip_painter.text(Pos2::new(visible.left() + 6.0, body.center().y), Align2::LEFT_CENTER, self.tr("ミュート", "Muted"), FontId::proportional(11.0), palette.text);
            }
        }
        let selected = self.selection.contains(&clip.id);
        let stroke = if selected { Stroke::new(2.5_f32, palette.selected) } else { Stroke::new(1.0_f32, palette.clip_border) };
        painter.rect_stroke(rect, 4.0, stroke, StrokeKind::Inside);
        visible
    }

    fn draw_ruler(&self, painter: &egui::Painter, rect: Rect, palette: &Palette, fps: f64) {
        let painter = painter.with_clip_rect(rect);
        painter.rect_filled(rect, 0.0, palette.ruler_bg);
        let step = geom::ruler_step(self.scale.px_per_sec, fps, 90.0);
        let minor = step / 5.0;
        let first = (self.scale.secs(rect.left()) / minor).floor().max(0.0) as i64;
        let last = (self.scale.secs(rect.right()) / minor).ceil() as i64;
        for index in first..=last {
            let seconds = index as f64 * minor;
            let x = self.scale.x_secs(seconds);
            let major = index % 5 == 0;
            let height = if major { rect.height() * 0.5 } else { rect.height() * 0.22 };
            painter.line_segment([Pos2::new(x, rect.bottom() - height), Pos2::new(x, rect.bottom())], Stroke::new(1.0_f32, palette.ruler_tick));
            if major {
                painter.text(Pos2::new(x + 3.0, rect.top() + 2.0), Align2::LEFT_TOP, geom::ruler_label(seconds, step, fps), FontId::monospace(10.5), palette.ruler_text);
            }
        }
        painter.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0_f32, palette.ruler_tick));
    }

    fn track_headers_ui(&mut self, ui: &mut egui::Ui, project: &Project, header_rect: Rect, rows: &[(String, Rect)], palette: &Palette) {
        let painter = ui.painter_at(header_rect);
        painter.rect_filled(header_rect, 0.0, palette.header_bg);
        let lang = self.lang;
        for (index, (track_id, row)) in rows.iter().enumerate() {
            let rect = Rect::from_min_max(Pos2::new(header_rect.left(), row.top()), Pos2::new(header_rect.right(), row.bottom()));
            if rect.bottom() < header_rect.top() || rect.top() > header_rect.bottom() {
                continue;
            }
            let Ok(track) = project.track(track_id) else { continue };
            let targeted = match track.kind {
                TrackKind::Video => self.target_track(TrackKind::Video).as_deref() == Some(track_id),
                TrackKind::Audio => self.target_track(TrackKind::Audio).as_deref() == Some(track_id),
                TrackKind::Caption => false,
            };
            let clipped = rect.intersect(header_rect);
            self.targets.insert(format!("header:{track_id}"), clipped);
            let response = ui.interact(clipped, ui.id().with(("header", track_id)), Sense::click());
            if response.clicked() && track.kind != TrackKind::Caption {
                match track.kind {
                    TrackKind::Video => self.target_video = Some(track_id.clone()),
                    _ => self.target_audio = Some(track_id.clone()),
                }
            }
            let kind_label = match track.kind {
                TrackKind::Video => lang.pick("映像", "Video"),
                TrackKind::Audio => lang.pick("音声", "Audio"),
                TrackKind::Caption => lang.pick("字幕", "Captions"),
            };
            let tooltip = if track.kind == TrackKind::Caption {
                format!("{} ({})", track.name, track.id)
            } else {
                format!("{} ({})\n{}", track.name, track.id, lang.pick("クリックで素材の配置先（対象トラック）にします。右クリックで並べ替え・削除。", "Click to make this the target track for new media. Right-click to reorder or delete."))
            };
            let response = response.on_hover_text(tooltip);
            let header_painter = painter.with_clip_rect(clipped);
            header_painter.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0_f32, palette.timeline_bg));
            if targeted {
                header_painter.rect_filled(Rect::from_min_size(rect.min, Vec2::new(4.0, rect.height())), 0.0, ui.visuals().selection.bg_fill);
            }
            // Name: double-click to rename (Enter applies, Esc cancels).
            let name_rect = Rect::from_min_max(Pos2::new(rect.left() + 8.0, rect.top() + 3.0), Pos2::new(rect.right() - 74.0, (rect.top() + 21.0).min(rect.bottom())));
            self.targets.insert(format!("name:{track_id}"), name_rect.intersect(header_rect));
            let renaming = self.renaming.as_ref().is_some_and(|(id, _)| id == track_id);
            if renaming {
                let mut text = self.renaming.as_ref().map(|(_, text)| text.clone()).unwrap_or_default();
                let edit = ui.put(name_rect, egui::TextEdit::singleline(&mut text).font(FontId::proportional(12.5)).margin(egui::Margin::symmetric(2, 0)));
                if self.rename_focus {
                    edit.request_focus();
                    self.rename_focus = false;
                }
                let (enter, escape) = ui.input(|input| (input.key_pressed(egui::Key::Enter), input.key_pressed(egui::Key::Escape)));
                if escape {
                    self.renaming = None;
                } else if edit.lost_focus() || enter {
                    self.renaming = None;
                    let name = text.trim().to_string();
                    if !name.is_empty() && name != track.name {
                        self.commit(vec![Operation::SetTrackProperties(SetTrackProperties { track_id: track_id.clone(), name: Some(name), locked: None })]);
                    }
                } else if let Some((_, stored)) = &mut self.renaming {
                    *stored = text;
                }
            } else {
                header_painter.text(Pos2::new(rect.left() + 10.0, rect.top() + 5.0), Align2::LEFT_TOP, &track.name, FontId::proportional(12.5), palette.text);
                if response.double_clicked() && response.interact_pointer_pos().is_some_and(|pos| name_rect.contains(pos)) {
                    self.renaming = Some((track_id.clone(), track.name.clone()));
                    self.rename_focus = true;
                }
            }
            header_painter.text(Pos2::new(rect.left() + 10.0, rect.top() + 22.0), Align2::LEFT_TOP, format!("{kind_label} · {}", track.id), FontId::proportional(10.5), palette.weak_text);
            // Lock and mute toggles.
            let button_h = 20.0_f32.min(rect.height() - 6.0).max(12.0);
            let mute_rect = Rect::from_min_size(Pos2::new(rect.right() - 32.0, rect.top() + 3.0), Vec2::new(26.0, button_h));
            let lock_rect = Rect::from_min_size(Pos2::new(rect.right() - 62.0, rect.top() + 3.0), Vec2::new(26.0, button_h));
            if mute_rect.bottom() <= header_rect.bottom() && mute_rect.top() >= header_rect.top() {
                let mute = ui.put(mute_rect, egui::Button::new(egui::RichText::new("M").strong()).selected(track.is_muted()));
                self.targets.insert(format!("mute:{track_id}"), mute_rect);
                let mute_tip = if track.kind == TrackKind::Video { lang.pick("トラックを無効化（映像を隠す）", "Disable track (hide picture)") } else { lang.pick("トラックをミュート", "Mute track") };
                if mute.on_hover_text(mute_tip).clicked() {
                    self.commit(vec![Operation::SetTrackMuted(SetTrackMuted { track_id: track_id.clone(), muted: !track.is_muted() })]);
                }
                let lock = ui.put(lock_rect, egui::Button::new("").selected(track.is_locked()));
                self.targets.insert(format!("lock:{track_id}"), lock_rect);
                let color = if track.is_locked() { ui.visuals().selection.stroke.color } else { palette.weak_text };
                paint_padlock(ui.painter(), lock_rect, color, track.is_locked());
                let lock_tip = if track.is_locked() { lang.pick("ロック中: クリックで解除（ロック中のクリップは変更できません）", "Locked: click to unlock (clips on a locked track cannot change)") } else { lang.pick("トラックをロック", "Lock track") };
                if lock.on_hover_text(lock_tip).clicked() {
                    self.commit(vec![Operation::SetTrackProperties(SetTrackProperties { track_id: track_id.clone(), name: None, locked: Some(!track.is_locked()) })]);
                }
            }
            if track.is_locked() {
                header_painter.rect_filled(Rect::from_min_size(Pos2::new(rect.right() - 3.0, rect.top()), Vec2::new(3.0, rect.height())), 0.0, palette.range_edge);
            }
            let up = index.checked_sub(1).map(|i| rows[i].0.clone());
            let down = rows.get(index + 1).map(|(id, _)| id.clone());
            response.context_menu(|ui| {
                let up_button = ui.add_enabled(up.is_some(), egui::Button::new(lang.pick("上へ移動", "Move Up")));
                crate::ui_inspector::register_target(ui, "header-menu:up", up_button.rect);
                if up_button.clicked() {
                    self.commit(vec![Operation::ReorderTrack(ReorderTrack { track_id: track_id.clone(), insert_before_track_id: up.clone(), insert_after_track_id: None })]);
                    ui.close();
                }
                let down_button = ui.add_enabled(down.is_some(), egui::Button::new(lang.pick("下へ移動", "Move Down")));
                crate::ui_inspector::register_target(ui, "header-menu:down", down_button.rect);
                if down_button.clicked() {
                    self.commit(vec![Operation::ReorderTrack(ReorderTrack { track_id: track_id.clone(), insert_before_track_id: None, insert_after_track_id: down.clone() })]);
                    ui.close();
                }
                if ui.button(lang.pick("名前を変更", "Rename")).clicked() {
                    self.renaming = Some((track_id.clone(), track.name.clone()));
                    self.rename_focus = true;
                    ui.close();
                }
                let lock_label = if track.is_locked() { lang.pick("ロックを解除", "Unlock") } else { lang.pick("ロック", "Lock") };
                if ui.button(lock_label).clicked() {
                    self.commit(vec![Operation::SetTrackProperties(SetTrackProperties { track_id: track_id.clone(), name: None, locked: Some(!track.is_locked()) })]);
                    ui.close();
                }
                let mute_label = if track.is_muted() { lang.pick("ミュートを解除", "Unmute") } else { lang.pick("ミュート", "Mute") };
                if ui.button(mute_label).clicked() {
                    self.commit(vec![Operation::SetTrackMuted(SetTrackMuted { track_id: track_id.clone(), muted: !track.is_muted() })]);
                    ui.close();
                }
                ui.separator();
                let delete = ui.button(lang.pick("トラックを削除…", "Delete Track…"));
                crate::ui_inspector::register_target(ui, "header-menu:delete", delete.rect);
                if delete.clicked() {
                    if track.clips.is_empty() {
                        self.commit(vec![Operation::DeleteTrack(DeleteTrack { track_id: track_id.clone(), delete_clips: false })]);
                    } else {
                        self.confirm_delete_track = Some(track_id.clone());
                    }
                    ui.close();
                }
            });
        }
    }
}
