//! Right column: the inspector (clip or project properties) and the agent panel
//! (the session's activity log and revision).
//!
//! Numbers change by dragging or typing. A drag only previews; releasing it (or
//! pressing Enter after typing) sends one operation, so one change is one undo step.

use crate::app::AvsApp;
use crate::edit;
use crate::geom;
use crate::theme::palette;
use avs_core::ops::{SetCaptionText, SetClipProperties, SetProjectSettings};
use avs_core::{CaptionStyle, Clip, ClipKind, ClipProperties, Crop, MediaType, Operation, Position, Project, Resolution, Time, TrackKind};
use eframe::egui::{self, RichText};

enum Edit {
    None,
    Preview(f64),
    Commit(f64),
    /// The drag ended: commit whatever is previewed.
    Release,
}

/// Remembers where a named control was drawn (the self-test clicks and drags it by name).
pub fn register_target(ui: &egui::Ui, name: &str, rect: egui::Rect) {
    ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new(("avs-target", name)), rect));
}

fn number(ui: &mut egui::Ui, name: &str, value: f64, range: std::ops::RangeInclusive<f64>, speed: f64, suffix: &str, decimals: usize) -> Edit {
    let mut edited = value;
    let response = ui.add(egui::DragValue::new(&mut edited).range(range).speed(speed).suffix(suffix).max_decimals(decimals).min_decimals(0));
    register_target(ui, &format!("inspector:{name}"), response.rect);
    if response.drag_stopped() {
        return Edit::Release;
    }
    if response.changed() {
        if response.dragged() || response.is_pointer_button_down_on() { Edit::Preview(edited) } else { Edit::Commit(edited) }
    } else {
        Edit::None
    }
}

/// Position and scale the renderer uses when a clip has neither: centre and the
/// auto-fit scale. `None` when the media size is unknown.
fn fitted(project: &Project, clip: &Clip) -> Option<(Position, f64)> {
    let media = project.media(clip.media_id.as_deref()?).ok()?;
    let (width, height) = edit::media_dimensions(media)?;
    let crop = clip.properties.crop.unwrap_or(Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0 });
    let (w, h) = (f64::from(width) * (1.0 - crop.left - crop.right).max(0.01), f64::from(height) * (1.0 - crop.top - crop.bottom).max(0.01));
    let (pw, ph) = (f64::from(project.resolution.width), f64::from(project.resolution.height));
    let scale = avs_render::auto_fit_scale(w, h, pw, ph, media.media_type == MediaType::Video);
    Some((Position { x: pw / 2.0, y: ph / 2.0 }, scale))
}

/// Local time of an ISO-8601 UTC stamp, as HH:MM:SS.
fn local_clock(iso: &str, offset_seconds: i64) -> String {
    let time = iso.split('T').nth(1).unwrap_or_default();
    let mut parts = time.split(':');
    let (Some(h), Some(m), Some(s)) = (parts.next(), parts.next(), parts.next()) else { return iso.to_string() };
    let (Ok(h), Ok(m), Ok(s)) = (h.parse::<i64>(), m.parse::<i64>(), s.get(..2).unwrap_or("0").parse::<i64>()) else { return iso.to_string() };
    let total = (h * 3600 + m * 60 + s + offset_seconds).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", total / 3600, (total / 60) % 60, total % 60)
}

pub fn local_offset_seconds() -> i64 {
    let output = std::process::Command::new("/bin/date").arg("+%z").output().ok();
    let text = output.map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string()).unwrap_or_default();
    if text.len() != 5 {
        return 0;
    }
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let hours: i64 = text[1..3].parse().unwrap_or(0);
    let minutes: i64 = text[3..5].parse().unwrap_or(0);
    sign * (hours * 3600 + minutes * 60)
}

impl AvsApp {
    pub fn right_column_ui(&mut self, ui: &mut egui::Ui) {
        let height = ui.available_height();
        egui::Panel::bottom("agent-panel").resizable(true).default_size(height * 0.22).size_range((height * 0.12).min(90.0)..=height * 0.5).show_inside(ui, |ui| self.agent_ui(ui));
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let flash = self.inspector_flash.is_some_and(|at| at.elapsed().as_secs_f32() < 1.2);
            let title = RichText::new(self.tr("インスペクタ", "Inspector")).strong();
            ui.label(if flash { title.color(ui.visuals().selection.bg_fill) } else { title });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let Some(project) = self.view_project().cloned() else { return };
                let clips: Vec<Clip> = self.selection.iter().filter_map(|id| project.clip(id).ok().cloned()).collect();
                if clips.is_empty() {
                    self.project_settings_ui(ui, &project);
                } else {
                    self.clip_inspector_ui(ui, &project, &clips);
                }
            });
        });
    }

    /// Sends (or previews) a property patch for each clip.
    fn patch(&mut self, edit: &Edit, patches: Vec<(String, ClipProperties)>) {
        let operations: Vec<Operation> = patches.into_iter().map(|(clip_id, properties)| Operation::SetClipProperties(SetClipProperties { clip_id, properties })).collect();
        match edit {
            Edit::Preview(_) => {
                if let Err(error) = self.preview_operations(operations) {
                    self.set_error(error.code, &error.message);
                }
            }
            Edit::Commit(_) => {
                self.commit(operations);
            }
            Edit::Release => self.commit_preview(),
            Edit::None => {}
        }
    }

    fn value(edit: &Edit) -> Option<f64> {
        match edit {
            Edit::Preview(value) | Edit::Commit(value) => Some(*value),
            _ => None,
        }
    }

    fn clip_inspector_ui(&mut self, ui: &mut egui::Ui, project: &Project, clips: &[Clip]) {
        let lead = &clips[0];
        // With several clips selected, edit the visible member of each linked pair.
        let ids: Vec<String> = edit::dedupe_pairs(project, &clips.iter().map(|clip| clip.id.clone()).collect::<Vec<_>>());
        let all_ids: Vec<String> = clips.iter().map(|clip| clip.id.clone()).collect();
        let several = ids.len() > 1;
        let fps = project.fps;
        let kind_of = |clip: &Clip| -> (bool, bool) {
            let track = project.track(&clip.track_id).map(|track| track.kind).unwrap_or(TrackKind::Video);
            let media = project.media_type_of(clip);
            let visual = clip.kind == ClipKind::Media && track == TrackKind::Video && media != Some(MediaType::Audio);
            let audible = clip.kind == ClipKind::Media && (track == TrackKind::Audio || media == Some(MediaType::Audio));
            (visual, audible)
        };
        let visual = clips.iter().filter(|clip| ids.contains(&clip.id)).all(|clip| kind_of(clip).0);
        let audible_clips: Vec<&Clip> = clips.iter().filter(|clip| kind_of(clip).1).collect();
        let captions = clips.iter().all(|clip| clip.kind == ClipKind::Caption);
        let media_clips = clips.iter().all(|clip| clip.kind == ClipKind::Media);

        if several {
            ui.label(RichText::new(format!("{} {}", ids.len(), self.tr("個のクリップ（共通の項目のみ）", "clips (shared fields only)"))).strong());
        } else {
            ui.label(RichText::new(&lead.name).strong());
            let media_name = lead.media_id.as_deref().and_then(|id| project.media(id).ok()).map(|media| media.name.clone()).unwrap_or_default();
            ui.label(RichText::new(format!("{}  ·  {}  ·  {}", lead.id, lead.track_id, media_name)).small().weak());
            egui::Grid::new("clip-time").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                ui.label(self.tr("開始", "Start"));
                ui.label(RichText::new(geom::timecode(lead.start_seconds, fps)).monospace());
                ui.end_row();
                ui.label(self.tr("長さ", "Duration"));
                ui.label(RichText::new(geom::timecode(lead.duration_seconds, fps)).monospace());
                ui.end_row();
                if lead.kind == ClipKind::Media {
                    ui.label(self.tr("素材の開始", "Source In"));
                    ui.label(RichText::new(geom::timecode(lead.source_start(), fps)).monospace());
                    ui.end_row();
                }
            });
        }
        ui.add_space(4.0);

        if visual && media_clips {
            egui::CollapsingHeader::new(self.tr("変形", "Transform")).default_open(true).show(ui, |ui| {
                let fit = fitted(project, lead);
                let props = &lead.properties;
                let current_pos = props.position.or(fit.map(|(position, _)| position));
                let current_scale = props.scale.or(fit.map(|(_, scale)| scale));
                let fit_scale = fit.map(|(_, scale)| scale);
                let (pw, ph) = (f64::from(project.resolution.width), f64::from(project.resolution.height));
                let bootstrap = props.position.is_none() && props.scale.is_none();
                let transform_ok = current_pos.is_some() && current_scale.is_some() && fit_scale.is_some();
                egui::Grid::new("transform").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label(self.tr("位置 X", "Position X"));
                    let edit_x = ui.add_enabled_ui(transform_ok && !several, |ui| number(ui, "posx", current_pos.map_or(0.0, |p| p.x - pw / 2.0), -20_000.0..=20_000.0, 1.0, " px", 1)).inner;
                    ui.end_row();
                    ui.label(self.tr("位置 Y", "Position Y"));
                    let edit_y = ui.add_enabled_ui(transform_ok && !several, |ui| number(ui, "posy", current_pos.map_or(0.0, |p| p.y - ph / 2.0), -20_000.0..=20_000.0, 1.0, " px", 1)).inner;
                    ui.end_row();
                    ui.label(self.tr("拡大率", "Scale"));
                    let shown_scale = match (current_scale, fit_scale) {
                        (Some(scale), Some(fit)) if fit > 0.0 => scale / fit * 100.0,
                        _ => 100.0,
                    };
                    let edit_scale = ui.add_enabled_ui(transform_ok && !several, |ui| number(ui, "scale", shown_scale, 1.0..=2000.0, 0.5, " %", 1)).inner;
                    ui.end_row();
                    for (edit, which) in [(edit_x, 0), (edit_y, 1), (edit_scale, 2)] {
                        if matches!(edit, Edit::None) {
                            continue;
                        }
                        let (Some(mut position), Some(mut scale), Some(fit)) = (current_pos, current_scale, fit_scale) else { continue };
                        if let Some(value) = Self::value(&edit) {
                            match which {
                                0 => position.x = value + pw / 2.0,
                                1 => position.y = value + ph / 2.0,
                                _ => scale = value / 100.0 * fit,
                            }
                        }
                        // The first transform change writes both values, so the picture
                        // keeps its auto-fitted size instead of jumping to the media's own.
                        let patch = ClipProperties { position: Some(position), scale: Some(scale), ..ClipProperties::default() };
                        let _ = bootstrap;
                        self.patch(&edit, vec![(lead.id.clone(), patch)]);
                    }
                    ui.label(self.tr("回転", "Rotation"));
                    let edit = number(ui, "rotation", props.rotation_degrees.unwrap_or(0.0), -360.0..=360.0, 0.5, " °", 1);
                    if let Some(value) = Self::value(&edit) {
                        self.patch(&edit, ids.iter().map(|id| (id.clone(), ClipProperties { rotation_degrees: Some(value), ..ClipProperties::default() })).collect());
                    } else if matches!(edit, Edit::Release) {
                        self.commit_preview();
                    }
                    ui.end_row();
                    ui.label(self.tr("不透明度", "Opacity"));
                    let edit = number(ui, "opacity", props.opacity.unwrap_or(1.0) * 100.0, 0.0..=100.0, 0.5, " %", 0);
                    if let Some(value) = Self::value(&edit) {
                        self.patch(&edit, ids.iter().map(|id| (id.clone(), ClipProperties { opacity: Some(value / 100.0), ..ClipProperties::default() })).collect());
                    } else if matches!(edit, Edit::Release) {
                        self.commit_preview();
                    }
                    ui.end_row();
                });
                if fit.is_none() {
                    ui.label(RichText::new(self.tr("素材の大きさが分からないため位置と拡大率は変えられません", "Position and scale are unavailable: the media size is unknown")).small().weak());
                }
                if !several {
                    ui.label(RichText::new(self.tr("切り抜き", "Crop")).strong());
                    let crop = props.crop.unwrap_or(Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0 });
                    egui::Grid::new("crop").num_columns(4).spacing([6.0, 6.0]).show(ui, |ui| {
                        let mut edits = Vec::new();
                        for (index, (ja, en, value)) in [("左", "Left", crop.left), ("右", "Right", crop.right), ("上", "Top", crop.top), ("下", "Bottom", crop.bottom)].into_iter().enumerate() {
                            ui.label(self.tr(ja, en));
                            edits.push((index, number(ui, ["crop-left", "crop-right", "crop-top", "crop-bottom"][index], value * 100.0, 0.0..=95.0, 0.25, " %", 1)));
                            if index % 2 == 1 {
                                ui.end_row();
                            }
                        }
                        for (index, edit) in edits {
                            if let Some(value) = Self::value(&edit) {
                                let mut next = crop;
                                let v = value / 100.0;
                                match index {
                                    0 => next.left = v.min(0.95 - next.right),
                                    1 => next.right = v.min(0.95 - next.left),
                                    2 => next.top = v.min(0.95 - next.bottom),
                                    _ => next.bottom = v.min(0.95 - next.top),
                                }
                                self.patch(&edit, vec![(lead.id.clone(), ClipProperties { crop: Some(next), ..ClipProperties::default() })]);
                            } else if matches!(edit, Edit::Release) {
                                self.commit_preview();
                            }
                        }
                    });
                }
            });
        }

        if media_clips {
            egui::CollapsingHeader::new(self.tr("音声とフェード", "Audio and Fades")).default_open(true).show(ui, |ui| {
                let props = audible_clips.first().map_or(&lead.properties, |clip| &clip.properties).clone();
                egui::Grid::new("audio").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    if !audible_clips.is_empty() {
                        ui.label(self.tr("音量", "Volume"));
                        let edit = number(ui, "volume", props.volume_db.unwrap_or(0.0), -60.0..=12.0, 0.1, " dB", 1);
                        if let Some(value) = Self::value(&edit) {
                            self.patch(&edit, audible_clips.iter().map(|clip| (clip.id.clone(), ClipProperties { volume_db: Some(value), ..ClipProperties::default() })).collect());
                        } else if matches!(edit, Edit::Release) {
                            self.commit_preview();
                        }
                        ui.end_row();
                    }
                    ui.label(self.tr("ミュート", "Mute"));
                    let mut muted = clips.iter().all(|clip| clip.properties.muted == Some(true));
                    if ui.checkbox(&mut muted, "").on_hover_text(crate::keys::Command::ToggleClipEnabled.tooltip(self.lang)).changed() {
                        let ops = project.with_companions(&all_ids).into_iter().map(|clip_id| Operation::SetClipProperties(SetClipProperties { clip_id, properties: ClipProperties { muted: Some(muted), ..ClipProperties::default() } })).collect();
                        self.commit(ops);
                    }
                    ui.end_row();
                    let max_fade = lead.duration_seconds.secs();
                    for (ja, en, current, fade_in) in [("フェードイン", "Fade In", lead.fade_in(), true), ("フェードアウト", "Fade Out", lead.fade_out(), false)] {
                        ui.label(self.tr(ja, en));
                        let edit = number(ui, if fade_in { "fadein" } else { "fadeout" }, current.secs(), 0.0..=max_fade, 0.02, " s", 2);
                        if let Some(value) = Self::value(&edit) {
                            let time = Time::from_secs(value).unwrap_or(Time::ZERO);
                            // Fades go on both halves of a linked pair, so picture and sound fade together.
                            let patches = project
                                .with_companions(&ids)
                                .into_iter()
                                .map(|id| (id, if fade_in { ClipProperties { fade_in_seconds: Some(time), ..ClipProperties::default() } } else { ClipProperties { fade_out_seconds: Some(time), ..ClipProperties::default() } }))
                                .collect();
                            self.patch(&edit, patches);
                        } else if matches!(edit, Edit::Release) {
                            self.commit_preview();
                        }
                        ui.end_row();
                    }
                    ui.label(self.tr("速度", "Speed"));
                    let edit = number(ui, "speed", lead.speed() * 100.0, 10.0..=1000.0, 1.0, " %", 0);
                    if let Some(value) = Self::value(&edit) {
                        self.patch(&edit, ids.iter().map(|id| (id.clone(), ClipProperties { speed: Some(value / 100.0), ..ClipProperties::default() })).collect());
                    } else if matches!(edit, Edit::Release) {
                        self.commit_preview();
                    }
                    ui.end_row();
                });
                ui.label(RichText::new(self.tr("速度を変えると長さは変わらず、使う素材の範囲が変わります。", "Changing speed keeps the clip length and changes how much media it uses.")).small().weak());
            });
        }

        if captions && !several {
            egui::CollapsingHeader::new(self.tr("字幕", "Caption")).default_open(true).show(ui, |ui| {
                let key = ui.id().with(("caption-text", &lead.id));
                let mut text: String = ui.data(|data| data.get_temp::<String>(key)).unwrap_or_else(|| lead.caption.as_ref().map(|caption| caption.text.clone()).unwrap_or_default());
                let response = ui.add(egui::TextEdit::multiline(&mut text).desired_rows(3).desired_width(f32::INFINITY).hint_text(self.tr("字幕の文言", "Caption text")));
                self.targets.insert("caption-text".into(), response.rect);
                if response.changed() {
                    ui.data_mut(|data| data.insert_temp(key, text.clone()));
                }
                let enter = response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter) && input.modifiers.command);
                if response.lost_focus() || enter {
                    ui.data_mut(|data| data.remove::<String>(key));
                    let stored = lead.caption.as_ref().map(|caption| caption.text.clone()).unwrap_or_default();
                    if text != stored {
                        self.commit(vec![Operation::SetCaptionText(SetCaptionText { clip_id: lead.id.clone(), text: text.clone() })]);
                    }
                    if enter {
                        response.surrender_focus();
                    }
                }
                ui.label(RichText::new(self.tr("入力欄から離れるか ⌘Enter で確定します", "Leave the field or press ⌘Enter to apply")).small().weak());
                let style = lead.properties.caption_style.clone().unwrap_or_default();
                egui::Grid::new("caption-style").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label(self.tr("位置", "Placement"));
                    let placement = style.placement.clone().unwrap_or_else(|| "bottom".into());
                    // The renderer's placements: title (top), center, subtitle (low), anything else (bottom).
                    let label = |value: &str, lang: crate::i18n::Lang| match value {
                        "title" => lang.pick("上", "Top"),
                        "center" => lang.pick("中央", "Center"),
                        "subtitle" => lang.pick("下（字幕の位置）", "Low (subtitle)"),
                        _ => lang.pick("下", "Bottom"),
                    };
                    let mut chosen = placement.clone();
                    egui::ComboBox::from_id_salt("caption-placement").selected_text(label(&placement, self.lang)).show_ui(ui, |ui| {
                        for value in ["title", "center", "bottom", "subtitle"] {
                            ui.selectable_value(&mut chosen, value.to_string(), label(value, self.lang));
                        }
                    });
                    if chosen != placement {
                        let next = CaptionStyle { placement: Some(chosen), ..style.clone() };
                        self.commit(vec![Operation::SetClipProperties(SetClipProperties { clip_id: lead.id.clone(), properties: ClipProperties { caption_style: Some(next), ..ClipProperties::default() } })]);
                    }
                    ui.end_row();
                    ui.label(self.tr("文字の大きさ", "Font Size"));
                    let edit = number(ui, "fontsize", style.font_size.unwrap_or((f64::from(project.resolution.height) * 0.04).round().max(24.0)), 12.0..=600.0, 0.5, " px", 0);
                    if let Some(value) = Self::value(&edit) {
                        let next = CaptionStyle { font_size: Some(value), ..style.clone() };
                        self.patch(&edit, vec![(lead.id.clone(), ClipProperties { caption_style: Some(next), ..ClipProperties::default() })]);
                    } else if matches!(edit, Edit::Release) {
                        self.commit_preview();
                    }
                    ui.end_row();
                    for (ja, en, current, is_color) in [("文字色", "Text Color", style.color.clone(), true), ("背景", "Background", style.background.clone(), false)] {
                        ui.label(self.tr(ja, en));
                        let mut rgba = parse_color(current.as_deref()).unwrap_or(if is_color { [255, 255, 255, 255] } else { [0, 0, 0, 160] });
                        let mut color = egui::Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
                        let response = egui::color_picker::color_edit_button_srgba(ui, &mut color, egui::color_picker::Alpha::OnlyBlend);
                        if response.changed() {
                            rgba = color.to_srgba_unmultiplied();
                            let hex = format!("#{:02x}{:02x}{:02x}{:02x}", rgba[0], rgba[1], rgba[2], rgba[3]);
                            let next = if is_color { CaptionStyle { color: Some(hex), ..style.clone() } } else { CaptionStyle { background: Some(hex), ..style.clone() } };
                            let operation = vec![Operation::SetClipProperties(SetClipProperties { clip_id: lead.id.clone(), properties: ClipProperties { caption_style: Some(next), ..ClipProperties::default() } })];
                            // A colour picker has no release event of its own; preview while it is open, commit when it closes.
                            let _ = self.preview_operations(operation);
                        }
                        ui.end_row();
                    }
                });
            });
        }
    }

    fn project_settings_ui(&mut self, ui: &mut egui::Ui, project: &Project) {
        ui.label(RichText::new(self.tr("プロジェクト", "Project")).strong());
        ui.label(RichText::new(self.tr("クリップを選ぶとその設定を表示します", "Select a clip to see its settings")).small().weak());
        ui.add_space(4.0);
        egui::Grid::new("project-settings").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label(self.tr("タイトル", "Title"));
            let key = ui.id().with("project-title");
            let mut title: String = ui.data(|data| data.get_temp::<String>(key)).unwrap_or_else(|| project.title.clone());
            let response = ui.add(egui::TextEdit::singleline(&mut title).desired_width(180.0));
            self.targets.insert("project-title".into(), response.rect);
            if response.changed() {
                ui.data_mut(|data| data.insert_temp(key, title.clone()));
            }
            if response.lost_focus() {
                ui.data_mut(|data| data.remove::<String>(key));
                if title.trim() != project.title && !title.trim().is_empty() {
                    self.commit(vec![Operation::SetProjectSettings(SetProjectSettings { title: Some(title.trim().to_string()), fps: None, resolution: None, sample_rate: None, metadata: None })]);
                }
            }
            ui.end_row();

            ui.label(self.tr("解像度", "Resolution"));
            let current = project.resolution;
            let presets = [(1920, 1080, "1080p (16:9)"), (1280, 720, "720p (16:9)"), (3840, 2160, "4K UHD"), (1080, 1920, "1080×1920 (9:16)"), (1080, 1080, "1080×1080 (1:1)")];
            let label = presets.iter().find(|(w, h, _)| *w == current.width && *h == current.height).map(|(_, _, label)| label.to_string()).unwrap_or_else(|| format!("{}×{}", current.width, current.height));
            let mut chosen = current;
            egui::ComboBox::from_id_salt("resolution").selected_text(label).show_ui(ui, |ui| {
                for (width, height, label) in presets {
                    ui.selectable_value(&mut chosen, Resolution { width, height }, label);
                }
            });
            if chosen != current {
                self.commit(vec![Operation::SetProjectSettings(SetProjectSettings { title: None, fps: None, resolution: Some(chosen), sample_rate: None, metadata: None })]);
            }
            ui.end_row();
            ui.label(self.tr("幅 × 高さ", "Width × Height"));
            ui.horizontal(|ui| {
                for which in 0..2 {
                    let value = if which == 0 { current.width } else { current.height };
                    let edit = number(ui, if which == 0 { "width" } else { "height" }, f64::from(value), 16.0..=8192.0, 2.0, "", 0);
                    if let Edit::Commit(value) | Edit::Preview(value) = edit {
                        let even = ((value / 2.0).round() * 2.0) as u32;
                        let resolution = if which == 0 { Resolution { width: even, ..current } } else { Resolution { height: even, ..current } };
                        let operation = vec![Operation::SetProjectSettings(SetProjectSettings { title: None, fps: None, resolution: Some(resolution), sample_rate: None, metadata: None })];
                        if matches!(edit, Edit::Commit(_)) {
                            self.commit(operation);
                        } else {
                            let _ = self.preview_operations(operation);
                        }
                    } else if matches!(edit, Edit::Release) {
                        self.commit_preview();
                    }
                }
            });
            ui.end_row();

            ui.label(self.tr("フレームレート", "Frame Rate"));
            let rates = [23.976, 24.0, 25.0, 29.97, 30.0, 50.0, 59.94, 60.0];
            let mut fps = project.fps;
            egui::ComboBox::from_id_salt("fps").selected_text(format!("{} fps", project.fps)).show_ui(ui, |ui| {
                for rate in rates {
                    ui.selectable_value(&mut fps, rate, format!("{rate} fps"));
                }
            });
            if (fps - project.fps).abs() > 1e-9 {
                self.commit(vec![Operation::SetProjectSettings(SetProjectSettings { title: None, fps: Some(fps), resolution: None, sample_rate: None, metadata: None })]);
            }
            ui.end_row();
            ui.label(self.tr("長さ", "Duration"));
            ui.label(RichText::new(geom::timecode(project.duration(), project.fps)).monospace());
            ui.end_row();
            ui.label(self.tr("素材 / クリップ", "Media / Clips"));
            ui.label(format!("{} / {}", project.media.len(), project.clips().count()));
            ui.end_row();
        });
    }

    fn agent_ui(&mut self, ui: &mut egui::Ui) {
        let pal = palette(ui.visuals().dark_mode);
        let Some(snapshot) = self.store.snapshot.clone() else {
            ui.label(self.tr("セッションを読み込めません", "The session could not be read"));
            return;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.tr("エージェントの操作", "Agent Activity")).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format!("rev {}", snapshot.revision)).monospace().strong()).on_hover_text(format!(
                    "{}: {}\n{}: {}  /  {}: {}",
                    self.tr("更新", "Updated"),
                    snapshot.updated_at,
                    self.tr("取り消し", "Undo"),
                    snapshot.undo_depth,
                    self.tr("やり直し", "Redo"),
                    snapshot.redo_depth
                ));
            });
        });
        ui.label(RichText::new(self.tr("MCP 経由の編集もこの画面の編集も、同じセッションに記録されます。", "Edits through MCP and in this window go to the same session.")).small().weak());
        ui.separator();
        let fresh = self.external_edit_at.is_some_and(|at| at.elapsed().as_secs_f32() < 6.0);
        let offset = self.local_offset;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if snapshot.activity.is_empty() {
                ui.label(RichText::new(self.tr("まだ操作はありません", "No activity yet")).weak());
            }
            for entry in snapshot.activity.iter().rev().take(200) {
                let own = entry.revision.is_some_and(|revision| self.store.is_own(revision));
                let highlight = fresh && !own && entry.revision.is_some_and(|revision| revision + 3 > snapshot.revision);
                let frame = egui::Frame::new().inner_margin(egui::Margin::symmetric(4, 2)).corner_radius(3.0).fill(if highlight { pal.highlight } else { egui::Color32::TRANSPARENT });
                frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(local_clock(&entry.at, offset)).monospace().small().weak());
                        ui.label(RichText::new(&entry.tool).monospace().small().strong());
                        if let Some(revision) = entry.revision {
                            ui.label(RichText::new(format!("rev {revision}")).monospace().small().weak());
                        }
                        // Entries from before this window opened are not attributed.
                        let external = !own && entry.revision.is_some_and(|revision| revision > self.store.opened_revision);
                        if own {
                            ui.label(RichText::new(self.tr("この画面", "editor")).small().color(pal.weak_text));
                        } else if external {
                            ui.label(RichText::new(self.tr("外部", "external")).small().color(ui.visuals().selection.bg_fill));
                        }
                        if !entry.ok {
                            ui.label(RichText::new(self.tr("失敗", "failed")).small().color(pal.error));
                        }
                    });
                    ui.label(RichText::new(&entry.summary).small());
                });
            }
        });
    }
}

fn parse_color(text: Option<&str>) -> Option<[u8; 4]> {
    let hex = text?.trim().strip_prefix('#')?;
    let byte = |index: usize| u8::from_str_radix(hex.get(index..index + 2)?, 16).ok();
    match hex.len() {
        6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
        8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_and_colours() {
        assert_eq!(local_clock("2026-11-17T11:22:33.123Z", 9 * 3600), "20:22:33");
        assert_eq!(local_clock("2026-11-17T23:00:00Z", 2 * 3600), "01:00:00");
        assert_eq!(parse_color(Some("#ff000080")), Some([255, 0, 0, 128]));
        assert_eq!(parse_color(Some("#00ff00")), Some([0, 255, 0, 255]));
        assert_eq!(parse_color(Some("red")), None);
    }
}
