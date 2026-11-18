//! Toolbar, status bar and the floating windows (shortcuts, display settings,
//! export, recording, confirmations). The menus are in the macOS menu bar
//! (`menu_model`, `native_menu`).

use crate::app::{AvsApp, StatusKind};
use crate::app_shell_foundation::show_app_shell_preferences;
use crate::edit::Tool;
use crate::export_job::ExportState;
use crate::keys::Command;
use crate::theme::palette;
use avs_core::Operation;
use avs_core::ops::DeleteTrack;
use eframe::egui::{self, RichText};

impl AvsApp {
    pub fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let tools = [(Tool::Select, Command::ToolSelect, "選択", "Select"), (Tool::Trim, Command::ToolTrim, "トリム", "Trim"), (Tool::Blade, Command::ToolBlade, "ブレード", "Blade"), (Tool::Range, Command::ToolRange, "範囲", "Range")];
            for (tool, command, ja, en) in tools {
                let label = format!("{}  {}", self.tr(ja, en), command.shortcut());
                let response = ui.add(egui::Button::new(label).selected(self.tool == tool)).on_hover_text(command.tooltip(self.lang));
                self.targets.insert(format!("tool:{tool:?}"), response.rect);
                if response.clicked() {
                    self.tool = tool;
                }
            }
            ui.separator();
            let snap = ui.add(egui::Button::new(format!("{}  N", self.tr("スナップ", "Snap"))).selected(self.view.snap)).on_hover_text(Command::ToggleSnap.tooltip(self.lang));
            if snap.clicked() {
                self.view.snap = !self.view.snap;
            }
            ui.separator();
            let can_undo = self.store.snapshot.as_ref().is_some_and(|s| s.undo_depth > 0);
            let can_redo = self.store.snapshot.as_ref().is_some_and(|s| s.redo_depth > 0);
            if ui.add_enabled(can_undo, egui::Button::new(self.tr("取り消す", "Undo"))).on_hover_text(Command::Undo.tooltip(self.lang)).clicked() {
                self.undo();
            }
            if ui.add_enabled(can_redo, egui::Button::new(self.tr("やり直す", "Redo"))).on_hover_text(Command::Redo.tooltip(self.lang)).clicked() {
                self.redo();
            }
            ui.separator();
            if ui.button(self.tr("読み込む…", "Import…")).on_hover_text(Command::Import.tooltip(self.lang)).clicked() {
                self.import_dialog();
            }
            if ui.button(self.tr("書き出す…", "Export…")).on_hover_text(Command::Export.tooltip(self.lang)).clicked() {
                self.open_export();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(self.tr("表示設定", "Display")).on_hover_text(Command::ShowSettings.tooltip(self.lang)).clicked() {
                    self.show_settings = !self.show_settings;
                }
                if ui.button("?").on_hover_text(Command::ShowHelp.tooltip(self.lang)).clicked() {
                    self.show_help = !self.show_help;
                }
                if self.external_edit_at.is_some_and(|at| at.elapsed().as_secs_f32() < 6.0) {
                    let pal = palette(ui.visuals().dark_mode);
                    egui::Frame::new().fill(pal.highlight).corner_radius(4.0).inner_margin(egui::Margin::symmetric(8, 2)).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(self.tr("エージェントが編集しました", "An agent edited the project")).strong()).truncate());
                    });
                }
                if let Some(project) = self.store.project() {
                    // Takes only the room left by the tools, so it never covers them.
                    let text = format!("{}   {}×{}  {} fps", project.title, project.resolution.width, project.resolution.height, project.fps);
                    ui.add(egui::Label::new(RichText::new(text).strong()).truncate());
                }
            });
        });
    }

    pub fn status_bar_ui(&mut self, ui: &mut egui::Ui) {
        let pal = palette(ui.visuals().dark_mode);
        let saved = match (&self.store.load_error, self.store.snapshot.as_ref()) {
            (Some(error), _) => RichText::new(format!("{}: {}", self.tr("読み込めません", "Cannot read"), error.code)).color(pal.error),
            (None, Some(snapshot)) => RichText::new(format!("rev {}  ·  {}", snapshot.revision, self.tr("保存済み", "Saved"))).color(pal.ok),
            (None, None) => RichText::new("—"),
        };
        let library = self.store.data_directory();
        let short: Vec<String> = library.components().rev().take(3).map(|part| part.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().into_iter().rev().collect();
        let library_label = format!("…/{}", short.join("/"));
        ui.horizontal(|ui| {
            // Right side first so it always fits; the message takes what is left.
            let right_width = 360.0_f32.min(ui.available_width() * 0.45);
            let left_width = ui.available_width() - right_width - 8.0;
            ui.allocate_ui_with_layout(egui::vec2(left_width, ui.spacing().interact_size.y), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_clip_rect(ui.max_rect());
                for task in &self.imports {
                    ui.spinner();
                    ui.label(format!("{} {}", self.tr("読み込み中:", "Importing:"), task.file_name));
                }
                match &self.status {
                    Some(status) => {
                        let color = match status.kind {
                            StatusKind::Error => pal.error,
                            StatusKind::Ok => pal.ok,
                            StatusKind::Info => ui.visuals().text_color(),
                        };
                        let prefix = if status.kind == StatusKind::Error { self.tr("エラー  ", "Error  ") } else { "" };
                        ui.add(egui::Label::new(RichText::new(format!("{prefix}{}", status.text)).color(color)).truncate()).on_hover_text(&status.text);
                    }
                    None => {
                        ui.label(RichText::new(self.tr("準備完了", "Ready")).weak());
                    }
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(saved).on_hover_text(self.store.session.paths().session_file.display().to_string());
                let mut tip = library.display().to_string();
                if let Some(note) = &self.sandbox_note {
                    tip = format!("{tip}\n{note}");
                }
                ui.add(egui::Label::new(RichText::new(library_label).weak().small()).truncate()).on_hover_text(tip);
            });
        });
    }

    pub fn windows_ui(&mut self, ctx: &egui::Context) {
        self.help_window(ctx);
        self.settings_window(ctx);
        self.export_window(ctx);
        self.recorder_window(ctx);
        self.delete_track_window(ctx);
        self.workspace_windows_ui(ctx);
    }

    fn help_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_help;
        egui::Window::new(self.tr("キーボードショートカット", "Keyboard Shortcuts")).open(&mut open).default_width(560.0).vscroll(true).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).show(ctx, |ui| {
            ui.label(self.tr(
                "Final Cut Pro と同じ配置です。テキスト入力中はショートカットは効きません。",
                "Laid out as in Final Cut Pro. Shortcuts are off while typing in a text field.",
            ));
            for (ja, en, commands) in Command::groups() {
                ui.add_space(6.0);
                ui.label(RichText::new(self.tr(ja, en)).strong());
                egui::Grid::new(("help", en)).num_columns(2).striped(true).spacing([24.0, 4.0]).show(ui, |ui| {
                    for command in commands {
                        ui.label(RichText::new(command.all_shortcuts().join("  /  ")).monospace());
                        ui.label(command.label(self.lang));
                        ui.end_row();
                    }
                });
            }
            ui.add_space(6.0);
            ui.label(RichText::new(self.tr("マウス", "Mouse")).strong());
            let mouse = [
                ("クリップをドラッグ: 移動（別トラックへも。1本かリンクした1組のとき）", "Drag a clip: move it (to another track too, for one clip or one linked pair)"),
                ("クリップの端をドラッグ: トリム。⌥を押すと詰めてトリム（リップル）", "Drag a clip edge: trim. Hold ⌥ to ripple"),
                ("トリムツール: 端=リップル、隣と接する端=ロール、本体=スリップ、⌥+本体=スライド", "Trim tool: edge = ripple, shared edge = roll, body = slip, ⌥ + body = slide"),
                ("⌘クリック / ⇧クリック: 選択に追加、空き地からドラッグ: 矩形選択", "⌘-click / ⇧-click: add to selection; drag from empty space: box select"),
                ("ルーラーをクリック/ドラッグ: 再生ヘッドの移動（スクラブ）", "Click or drag the ruler: move the playhead (scrub)"),
                ("スクロール: 移動、⌘/⌥+スクロールかピンチ: ズーム、⌃を押しながらドラッグ: スナップを一時的に反転", "Scroll: pan; ⌘/⌥ + scroll or pinch: zoom; hold ⌃ while dragging: invert snapping"),
                ("素材をタイムラインへドラッグ: その位置に配置。Finder からのドロップ: 読み込み（タイムライン上ならその位置に配置）", "Drag media to the timeline to place it there. Drop files from Finder to import (on the timeline: placed there)"),
            ];
            for (ja, en) in mouse {
                ui.label(format!("・{}", self.tr(ja, en)));
            }
        });
        self.show_help = open;
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        let mut changed = false;
        egui::Window::new(self.tr("表示設定", "Display Settings")).open(&mut open).resizable(false).collapsible(false).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).show(ctx, |ui| {
            let change = show_app_shell_preferences(ui, &mut self.prefs, self.system_language, "main-settings");
            changed = change.changed;
        });
        if changed {
            self.apply_prefs(ctx);
        }
        self.show_settings = open;
    }

    fn export_window(&mut self, ctx: &egui::Context) {
        let Some(task) = &self.export else { return };
        let mut open = true;
        let mut cancel = false;
        let mut reveal = None;
        let mut close = false;
        let title = self.tr("書き出し", "Export");
        egui::Window::new(title).collapsible(false).resizable(false).open(&mut open).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).show(ctx, |ui| {
            ui.label(task.output.display().to_string());
            match &task.state {
                ExportState::Ready | ExportState::Running => {
                    let ratio = task.ratio();
                    ui.add(egui::ProgressBar::new(ratio as f32).show_percentage().desired_width(360.0));
                    if let Some(job) = &task.job_id {
                        ui.label(RichText::new(format!("job {job}")).small().weak());
                    }
                    let button = ui.button(self.tr("キャンセル", "Cancel"));
                    crate::ui_inspector::register_target(ui, "button:cancel-export", button.rect);
                    if button.clicked() {
                        cancel = true;
                    }
                }
                ExportState::Done(Ok(path)) => {
                    ui.label(RichText::new(self.tr("書き出しが終わりました", "Export finished")).strong());
                    for warning in &task.warnings {
                        ui.label(RichText::new(warning).small());
                    }
                    ui.horizontal(|ui| {
                        if ui.button(self.tr("Finder で表示", "Show in Finder")).clicked() {
                            reveal = Some(path.clone());
                        }
                        if ui.button(self.tr("閉じる", "Close")).clicked() {
                            close = true;
                        }
                    });
                }
                ExportState::Done(Err(_)) if task.was_cancelled() => {
                    ui.label(self.tr("書き出しをキャンセルしました（途中のファイルは残していません）", "Export cancelled (no partial file is left)"));
                    if ui.button(self.tr("閉じる", "Close")).clicked() {
                        close = true;
                    }
                }
                ExportState::Done(Err(error)) => {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    if ui.button(self.tr("閉じる", "Close")).clicked() {
                        close = true;
                    }
                }
            }
        });
        if cancel {
            task.cancel();
        }
        if let Some(path) = reveal {
            let _ = std::process::Command::new("/usr/bin/open").arg("-R").arg(path).spawn();
        }
        let running = matches!(task.state, ExportState::Running);
        if close || (!open && !running) {
            self.export = None;
        } else if !open && running {
            task.cancel();
        }
    }

    fn recorder_window(&mut self, ctx: &egui::Context) {
        let Some(recorder) = &self.recorder else { return };
        let mut open = true;
        let mut start = false;
        let mut stop = false;
        let recording = recorder.is_recording();
        let (seconds, peaks, error) = (recorder.seconds(), recorder.peaks(), recorder.error.clone());
        let (channels, device) = (usize::from(recorder.channels), recorder.device_name.clone());
        let mut channel = recorder.channel();
        let chosen = channel;
        let pal = palette(ctx.global_style().visuals.dark_mode);
        egui::Window::new(self.tr("ナレーションを録音", "Record Narration")).collapsible(false).resizable(false).open(&mut open).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).show(ctx, |ui| {
            ui.label(self.tr("入力の1チャンネルをモノラルで録音し、録音を始めた再生ヘッドの位置に音声トラックへ置きます。", "Records one input channel in mono and places it on an audio track at the playhead position where recording started."));
            if !device.is_empty() {
                ui.label(RichText::new(format!("{}: {device}", self.tr("入力", "Input"))).small().weak());
            }
            if channels > 1 {
                ui.horizontal(|ui| {
                    ui.label(self.tr("チャンネル", "Channel"));
                    ui.add_enabled_ui(!recording, |ui| {
                        egui::ComboBox::from_id_salt("narration-channel").selected_text((channel + 1).to_string()).show_ui(ui, |ui| {
                            for index in 0..channels {
                                ui.selectable_value(&mut channel, index, (index + 1).to_string());
                            }
                        });
                    });
                    // One meter per input channel: the one that moves when you speak is the microphone.
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(channels as f32 * 9.0, 22.0), egui::Sense::hover());
                    for index in 0..channels {
                        let level = peaks.get(index).copied().unwrap_or(0.0).sqrt().min(1.0);
                        let x = rect.left() + index as f32 * 9.0;
                        let slot = egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(x + 7.0, rect.bottom()));
                        ui.painter().rect_filled(slot, 1.0, ui.visuals().extreme_bg_color);
                        let bar = egui::Rect::from_min_max(egui::pos2(x, rect.bottom() - rect.height() * level), slot.max);
                        ui.painter().rect_filled(bar, 1.0, if index == channel { pal.ok } else { ui.visuals().weak_text_color() });
                    }
                });
            }
            let level = peaks.get(channel).copied().unwrap_or(0.0);
            ui.add(egui::ProgressBar::new(level.sqrt().min(1.0)).desired_width(320.0).text(format!("{:.1} s", seconds)));
            ui.horizontal(|ui| {
                if !recording && ui.button(self.tr("録音開始", "Record")).clicked() {
                    start = true;
                }
                if recording && ui.button(self.tr("停止して配置", "Stop and Place")).clicked() {
                    stop = true;
                }
            });
            if let Some(error) = error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        });
        // The meters move while the window is open.
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        if channel != chosen {
            recorder.set_channel(channel);
            self.view.narration_channel = channel;
        }
        if start {
            self.recorder_start();
        }
        if stop {
            self.recorder_stop();
        }
        if !open {
            self.recorder = None;
        }
    }

    fn delete_track_window(&mut self, ctx: &egui::Context) {
        let Some(track_id) = self.confirm_delete_track.clone() else { return };
        let count = self.store.project().and_then(|project| project.track(&track_id).ok()).map_or(0, |track| track.clips.len());
        let mut open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Window::new(self.tr("トラックを削除", "Delete Track")).collapsible(false).resizable(false).open(&mut open).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).show(ctx, |ui| {
            ui.label(format!("{}: {track_id}  ({count} {})", self.tr("トラック", "Track"), self.tr("クリップ", "clips")));
            ui.label(self.tr("トラックと、その上のクリップ（リンクした相方も）を削除します。⌘Z で戻せます。", "Deletes the track and its clips (and their linked partners). ⌘Z brings them back."));
            ui.horizontal(|ui| {
                let button = ui.button(self.tr("削除", "Delete"));
                crate::ui_inspector::register_target(ui, "button:confirm-delete-track", button.rect);
                if button.clicked() {
                    confirmed = true;
                }
                if ui.button(self.tr("キャンセル", "Cancel")).clicked() {
                    cancelled = true;
                }
            });
        });
        if confirmed {
            self.commit(vec![Operation::DeleteTrack(DeleteTrack { track_id, delete_clips: true })]);
        }
        if confirmed || cancelled || !open {
            self.confirm_delete_track = None;
        }
    }
}
