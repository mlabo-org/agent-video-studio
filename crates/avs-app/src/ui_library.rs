//! The media library: thumbnails, search, kind filter, import, and removing
//! unused media. Items drag onto the timeline; E / W / D / Q place the selected one.

use crate::app::AvsApp;
use crate::edit::PlaceMode;
use crate::geom;
use crate::keys::Command;
use crate::theme::palette;
use crate::ui_timeline::MediaDrag;
use avs_core::ops::DeleteMedia;
use avs_core::{MediaAsset, MediaType, Operation};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, StrokeKind, Vec2};

const CARD_W: f32 = 132.0;
const THUMB_H: f32 = 74.0;

impl AvsApp {
    pub fn library_ui(&mut self, ui: &mut egui::Ui) {
        let Some(project) = self.store.project().cloned() else { return };
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.tr("素材ライブラリ", "Media")).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(self.tr("読み込む…", "Import…")).on_hover_text(Command::Import.tooltip(self.lang)).clicked() {
                    self.import_dialog();
                }
            });
        });
        ui.horizontal(|ui| {
            let hint = self.tr("名前で検索", "Search by name");
            let search = ui.add(egui::TextEdit::singleline(&mut self.library_search).hint_text(hint).desired_width(ui.available_width() - 96.0));
            self.targets.insert("library-search".into(), search.rect);
            let kind_label = |kind: Option<MediaType>, lang: crate::i18n::Lang| match kind {
                None => lang.pick("すべて", "All"),
                Some(MediaType::Video) => lang.pick("映像", "Video"),
                Some(MediaType::Audio) => lang.pick("音声", "Audio"),
                Some(_) => lang.pick("画像", "Images"),
            };
            egui::ComboBox::from_id_salt("library-kind").selected_text(kind_label(self.library_kind, self.lang)).width(80.0).show_ui(ui, |ui| {
                for kind in [None, Some(MediaType::Video), Some(MediaType::Audio), Some(MediaType::Image)] {
                    ui.selectable_value(&mut self.library_kind, kind, kind_label(kind, self.lang));
                }
            });
        });
        let query = self.library_search.trim().to_lowercase();
        let items: Vec<MediaAsset> = project
            .media
            .iter()
            .filter(|media| query.is_empty() || media.name.to_lowercase().contains(&query) || media.tags.as_ref().is_some_and(|tags| tags.iter().any(|tag| tag.to_lowercase().contains(&query))))
            .filter(|media| match self.library_kind {
                None => true,
                Some(MediaType::Image) => media.media_type.is_still(),
                Some(kind) => media.media_type == kind,
            })
            .cloned()
            .collect();

        // The selected item's actions.
        let selected = self.selected_media.clone().and_then(|id| project.media(&id).ok().cloned());
        if let Some(media) = &selected {
            let uses = project.clips().filter(|clip| clip.media_id.as_deref() == Some(&media.id)).count();
            ui.horizontal_wrapped(|ui| {
                for (command, ja, en) in [(Command::AppendMedia, "末尾", "Append"), (Command::InsertMedia, "挿入", "Insert"), (Command::OverwriteMedia, "上書き", "Overwrite"), (Command::ConnectMedia, "重ねる", "Connect")] {
                    let label = format!("{} {}", self.tr(ja, en), command.shortcut());
                    if ui.small_button(label).on_hover_text(command.tooltip(self.lang)).clicked() {
                        let ctx = ui.ctx().clone();
                        self.run(command, &ctx);
                    }
                }
                let delete = ui.add_enabled(uses == 0, egui::Button::new(self.tr("削除", "Delete")).small());
                let delete = if uses == 0 { delete.on_hover_text(self.tr("未使用の素材をライブラリから外します（⌘Z で戻せます）", "Removes this unused item (⌘Z brings it back)")) } else { delete.on_disabled_hover_text(self.tr("タイムラインで使われている素材は削除できません", "Media used on the timeline cannot be deleted")) };
                if delete.clicked() {
                    self.commit(vec![Operation::DeleteMedia(DeleteMedia { media_id: media.id.clone() })]);
                }
            });
        }
        let unused: Vec<String> = project.media.iter().filter(|media| !project.clips().any(|clip| clip.media_id.as_deref() == Some(&media.id))).map(|media| media.id.clone()).collect();
        if !unused.is_empty() {
            let label = format!("{} ({})", self.tr("未使用の素材をすべて削除", "Delete All Unused Media"), unused.len());
            if ui.small_button(label).on_hover_text(self.tr("タイムラインで使っていない素材をまとめて外します（⌘Z で戻せます）", "Removes every item not on the timeline (⌘Z brings them back)")).clicked() {
                let operations = unused.into_iter().map(|media_id| Operation::DeleteMedia(DeleteMedia { media_id })).collect();
                self.commit(operations);
            }
        }
        ui.separator();

        if project.media.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(self.tr("素材がまだありません", "No media yet"));
                ui.label(RichText::new(self.tr("「読み込む…」か、Finder からこの窓へファイルをドロップしてください", "Use Import… or drop files from Finder onto this window")).weak());
            });
            return;
        }
        let pal = palette(ui.visuals().dark_mode);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let columns = ((ui.available_width() + 8.0) / (CARD_W + 8.0)).floor().max(1.0) as usize;
            let width = ((ui.available_width() - 8.0 * (columns as f32 - 1.0)) / columns as f32).floor();
            for row in items.chunks(columns) {
                ui.horizontal(|ui| {
                    for media in row {
                        self.media_card(ui, &project, media, width, &pal);
                    }
                });
            }
        });
    }

    fn media_card(&mut self, ui: &mut egui::Ui, project: &avs_core::Project, media: &MediaAsset, width: f32, pal: &crate::theme::Palette) {
        let size = Vec2::new(width, (THUMB_H * width / CARD_W).round() + 36.0);
        let id = ui.id().with(("media", &media.id));
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let response = ui.interact(rect, id, Sense::click_and_drag());
        self.targets.insert(format!("media:{}", media.id), rect);
        if response.drag_started() {
            response.dnd_set_drag_payload(MediaDrag(media.id.clone()));
            self.selected_media = Some(media.id.clone());
        }
        if response.clicked() {
            self.selected_media = Some(media.id.clone());
        }
        if response.double_clicked() {
            self.selected_media = Some(media.id.clone());
            self.place_media(&media.id.clone(), PlaceMode::Append, None, self.playhead);
        }
        let selected = self.selected_media.as_deref() == Some(&media.id);
        let painter = ui.painter_at(rect);
        let thumb = Rect::from_min_size(rect.min, Vec2::new(width, size.y - 36.0));
        painter.rect_filled(thumb, 4.0, pal.viewer_bg);
        let at = (media.duration_seconds.secs() * 0.1).min(2.0);
        if let Some(texture) = self.cache.frame_at(&media.id, at) {
            painter.image(texture.id(), thumb, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        } else if media.media_type == MediaType::Audio {
            if let Some(peaks) = self.cache.wave(&media.id) {
                let step = (peaks.len() as f32 / thumb.width()).max(1.0);
                let mid = thumb.center().y;
                for x in 0..thumb.width() as usize {
                    let index = (x as f32 * step) as usize;
                    let peak = peaks.get(index..(index + step as usize).min(peaks.len())).and_then(|s| s.iter().max().copied()).unwrap_or(0);
                    let h = f32::from(peak) / 255.0 * thumb.height() * 0.45;
                    painter.line_segment([Pos2::new(thumb.left() + x as f32, mid - h), Pos2::new(thumb.left() + x as f32, mid + h)], egui::Stroke::new(1.0_f32, pal.audio_clip));
                }
            } else {
                painter.text(thumb.center(), Align2::CENTER_CENTER, self.tr("音声", "Audio"), FontId::proportional(13.0), pal.weak_text);
            }
        }
        let kind = match media.media_type {
            MediaType::Video => self.tr("映像", "Video"),
            MediaType::Audio => self.tr("音声", "Audio"),
            MediaType::Image => self.tr("画像", "Image"),
            MediaType::Sequence => self.tr("連番", "Sequence"),
        };
        let uses = project.clips().filter(|clip| clip.media_id.as_deref() == Some(&media.id)).count();
        let badge = if media.media_type.is_still() { kind.to_string() } else { format!("{kind} {}", geom::timecode(media.duration_seconds, project.fps)) };
        let badge_rect = Rect::from_min_size(Pos2::new(thumb.left() + 4.0, thumb.bottom() - 18.0), Vec2::new(thumb.width() - 8.0, 15.0));
        painter.rect_filled(Rect::from_min_size(badge_rect.min, Vec2::new((badge.chars().count() as f32 * 6.8 + 8.0).min(badge_rect.width()), 15.0)), 3.0, Color32::from_black_alpha(170));
        painter.text(Pos2::new(badge_rect.left() + 4.0, badge_rect.center().y), Align2::LEFT_CENTER, badge, FontId::proportional(10.0), Color32::WHITE);
        if uses > 0 {
            let text = format!("{} {uses}", self.tr("使用中", "In use"));
            let used = Rect::from_min_size(Pos2::new(thumb.right() - 62.0, thumb.top() + 4.0), Vec2::new(58.0, 15.0));
            painter.rect_filled(used, 3.0, pal.ok.gamma_multiply(0.9));
            painter.text(used.center(), Align2::CENTER_CENTER, text, FontId::proportional(10.0), Color32::WHITE);
        }
        let name_rect = Rect::from_min_max(Pos2::new(rect.left() + 2.0, thumb.bottom() + 3.0), rect.max);
        let name_painter = painter.with_clip_rect(name_rect);
        name_painter.text(name_rect.left_top(), Align2::LEFT_TOP, &media.name, FontId::proportional(11.5), pal.text);
        if selected {
            painter.rect_stroke(rect.expand(1.0), 5.0, egui::Stroke::new(2.5_f32, ui.visuals().selection.bg_fill), StrokeKind::Inside);
        } else if response.hovered() {
            painter.rect_stroke(rect, 5.0, egui::Stroke::new(1.0_f32, pal.weak_text), StrokeKind::Inside);
        }
        let tip = format!("{}\n{}\n{}", media.name, media.id, self.tr("ダブルクリックで末尾に追加、タイムラインへドラッグでその位置に配置", "Double-click to append; drag onto the timeline to place it there"));
        let response = response.on_hover_text(tip);
        let lang = self.lang;
        let media_id = media.id.clone();
        response.context_menu(|ui| {
            self.selected_media = Some(media_id.clone());
            for command in [Command::AppendMedia, Command::InsertMedia, Command::OverwriteMedia, Command::ConnectMedia] {
                if ui.add(egui::Button::new(command.label(lang)).shortcut_text(command.shortcut())).clicked() {
                    let ctx = ui.ctx().clone();
                    self.run(command, &ctx);
                    ui.close();
                }
            }
            ui.separator();
            if ui.add_enabled(uses == 0, egui::Button::new(lang.pick("ライブラリから削除", "Delete from Library"))).clicked() {
                self.commit(vec![Operation::DeleteMedia(DeleteMedia { media_id: media_id.clone() })]);
                ui.close();
            }
            if uses > 0 {
                ui.label(RichText::new(lang.pick("使用中のため削除できません", "In use; cannot delete")).small());
            }
        });
    }
}
