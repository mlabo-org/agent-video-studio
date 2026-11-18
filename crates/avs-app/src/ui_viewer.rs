//! The viewer: the picture at the playhead, timecode and transport controls.

use crate::app::AvsApp;
use crate::geom;
use crate::keys::Command;
use crate::player::Transport;
use crate::theme::palette;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2};

#[derive(Clone, Copy)]
enum Icon {
    Start,
    Reverse,
    Play,
    Pause,
    Forward,
    End,
    StepBack,
    StepForward,
}

fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let c = rect.center();
    let s = rect.height().min(rect.width()) * 0.28;
    let tri = |painter: &egui::Painter, tip_x: f32, back_x: f32| {
        painter.add(Shape::convex_polygon(vec![Pos2::new(back_x, c.y - s), Pos2::new(tip_x, c.y), Pos2::new(back_x, c.y + s)], color, Stroke::NONE));
    };
    let bar = |painter: &egui::Painter, x: f32| {
        painter.rect_filled(Rect::from_center_size(Pos2::new(x, c.y), Vec2::new(s * 0.35, s * 2.0)), 0.0, color);
    };
    match icon {
        Icon::Play => tri(painter, c.x + s, c.x - s * 0.8),
        Icon::Pause => {
            bar(painter, c.x - s * 0.4);
            bar(painter, c.x + s * 0.4);
        }
        Icon::Forward => {
            tri(painter, c.x + s * 0.1, c.x - s);
            tri(painter, c.x + s * 1.1, c.x);
        }
        Icon::Reverse => {
            tri(painter, c.x - s * 0.1, c.x + s);
            tri(painter, c.x - s * 1.1, c.x);
        }
        Icon::Start => {
            bar(painter, c.x - s * 0.9);
            tri(painter, c.x - s * 0.7, c.x + s * 0.8);
        }
        Icon::End => {
            bar(painter, c.x + s * 0.9);
            tri(painter, c.x + s * 0.7, c.x - s * 0.8);
        }
        Icon::StepBack => {
            tri(painter, c.x - s * 0.6, c.x + s * 0.6);
            bar(painter, c.x - s * 0.8);
        }
        Icon::StepForward => {
            tri(painter, c.x + s * 0.6, c.x - s * 0.6);
            bar(painter, c.x + s * 0.8);
        }
    }
}

impl AvsApp {
    fn icon_button(&mut self, ui: &mut egui::Ui, icon: Icon, command: Command, active: bool, name: &str) -> bool {
        let (rect, response) = ui.allocate_exact_size(Vec2::new(34.0, 28.0), Sense::click());
        self.targets.insert(format!("button:{name}"), rect);
        let visuals = ui.style().interact_selectable(&response, active);
        ui.painter().rect(rect, 5.0, visuals.weak_bg_fill, visuals.bg_stroke, egui::StrokeKind::Inside);
        paint_icon(ui.painter(), rect, icon, visuals.fg_stroke.color);
        response.on_hover_text(command.tooltip(self.lang)).clicked()
    }

    pub fn viewer_ui(&mut self, ui: &mut egui::Ui) {
        let pal = palette(ui.visuals().dark_mode);
        let Some(project) = self.store.project().cloned() else {
            ui.centered_and_justified(|ui| {
                let text = self.store.load_error.as_ref().map(|error| format!("{}: {}", error.code, error.message)).unwrap_or_default();
                ui.label(RichText::new(text).color(pal.error));
            });
            return;
        };
        let controls_h = 40.0;
        let full = ui.available_rect_before_wrap();
        let area = Rect::from_min_max(full.min, Pos2::new(full.right(), full.bottom() - controls_h));
        self.viewer_avail = area.size();
        self.targets.insert("viewer".into(), area);
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 0.0, pal.viewer_bg);
        let aspect = project.resolution.width.max(1) as f32 / project.resolution.height.max(1) as f32;
        let mut picture = area.shrink(8.0);
        if picture.width() / picture.height() > aspect {
            picture = Rect::from_center_size(picture.center(), Vec2::new(picture.height() * aspect, picture.height()));
        } else {
            picture = Rect::from_center_size(picture.center(), Vec2::new(picture.width(), picture.width() / aspect));
        }
        painter.rect_filled(picture, 0.0, Color32::BLACK);
        if let Some(texture) = &self.player.texture {
            painter.image(texture.id(), picture, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        } else if project.duration() == avs_core::Time::ZERO {
            painter.text(picture.center(), Align2::CENTER_CENTER, self.tr("素材をタイムラインに置くとここに映ります", "Put media on the timeline to see it here"), FontId::proportional(14.0), Color32::from_gray(170));
        }
        if let Some(error) = &self.player.last_error {
            let text: String = error.lines().next().unwrap_or_default().chars().take(160).collect();
            painter.text(Pos2::new(picture.left() + 8.0, picture.bottom() - 8.0), Align2::LEFT_BOTTOM, text, FontId::proportional(11.0), pal.error);
        }
        if self.player.scrub_busy() {
            painter.circle_filled(Pos2::new(area.right() - 12.0, area.top() + 12.0), 4.0, Color32::from_gray(150));
        }
        // Level meter on the right edge.
        let level = self.player.level().clamp(0.0, 1.0).sqrt();
        if level > 0.0 {
            let meter = Rect::from_min_max(Pos2::new(area.right() - 7.0, area.bottom() - area.height() * level), Pos2::new(area.right() - 3.0, area.bottom()));
            painter.rect_filled(meter, 1.0, if level > 0.95 { pal.error } else { pal.ok });
        }

        // Transport.
        let controls = Rect::from_min_max(Pos2::new(full.left(), area.bottom()), full.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(controls.shrink2(Vec2::new(6.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let ui = &mut child;
        let roomy = ui.available_width() > 7.0 * 40.0 + 260.0;
        ui.label(RichText::new(geom::timecode(self.playhead, project.fps)).monospace().size(15.0).strong());
        if roomy {
            ui.label(RichText::new(format!("/ {}", geom::timecode(project.duration(), project.fps))).monospace().weak());
        }
        let speed = match self.player.transport {
            Transport::Playing(rate) if (rate - 1.0).abs() > 1e-6 => format!("{rate:+.0}×"),
            _ => String::new(),
        };
        ui.label(RichText::new(speed).monospace());
        let width = 8.0 * 34.0 + 7.0 * ui.spacing().item_spacing.x;
        let gap = if roomy { ((ui.available_width() - width) / 2.0 - 120.0).max(4.0) } else { 4.0 };
        ui.add_space(gap);
        let ctx = ui.ctx().clone();
        let playing = self.player.is_playing();
        let forward = matches!(self.player.transport, Transport::Playing(rate) if rate > 1.0);
        let reverse = matches!(self.player.transport, Transport::Playing(rate) if rate < 0.0);
        let buttons = [
            (Icon::Start, Command::GoToStart, false, "start"),
            (Icon::StepBack, Command::FrameBack, false, "frame-back"),
            (Icon::Reverse, Command::PlayReverse, reverse, "reverse"),
            (if playing { Icon::Pause } else { Icon::Play }, Command::PlayPause, playing && !reverse && !forward, "play"),
            (Icon::Forward, Command::PlayForward, forward, "forward"),
            (Icon::StepForward, Command::FrameForward, false, "frame-forward"),
            (Icon::End, Command::GoToEnd, false, "end"),
        ];
        for (icon, command, active, name) in buttons {
            if self.icon_button(ui, icon, command, active, name) {
                self.run(command, &ctx);
            }
        }
        if !self.player.warnings.is_empty() {
            ui.label(RichText::new(format!("{} {}", self.tr("警告", "Warnings"), self.player.warnings.len())).color(pal.error)).on_hover_text(self.player.warnings.join("\n"));
        }
    }
}
