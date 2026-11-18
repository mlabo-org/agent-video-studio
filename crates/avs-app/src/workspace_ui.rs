//! Library (workspace) management: new, open, recent, delete to `.deleted`, and
//! restore. Everything goes through `avs_session` so the editor and the MCP
//! server apply the same rules (blockers, confirmations, the recent list).

use crate::app::{AvsApp, StatusKind};
use avs_session::workspace::DELETED_SUFFIX;
use avs_session::{ConfirmedInput, CreateInput, RecentList, SwitchInput, WorkspacePlan};
use eframe::egui;

#[derive(Default)]
pub struct WorkspaceDialogs {
    /// A delete waiting for the user's confirmation.
    pub delete_plan: Option<WorkspacePlan>,
    /// A restore waiting for the user's confirmation.
    pub restore_plan: Option<WorkspacePlan>,
    pub recent: Option<RecentList>,
}

impl AvsApp {
    pub fn refresh_recent(&mut self) {
        self.workspace.recent = avs_session::recent_list(&self.store.env).ok();
    }

    pub fn new_library(&mut self) {
        let Some(folder) = rfd::FileDialog::new().set_title(self.tr("新しいライブラリを作る場所", "Where to create the new library")).pick_folder() else { return };
        self.create_library(&folder.display().to_string());
    }

    pub fn create_library(&mut self, target: &str) {
        let input = CreateInput { target_path: target.to_string(), title: None, expected_settings_revision: None };
        match avs_session::create_workspace(&self.store.env, &input) {
            Ok(result) => {
                self.switched_library();
                let text = format!("{} {}", self.tr("新しいライブラリを作りました:", "Created a new library:"), result.settings.data_directory.display());
                self.set_status(StatusKind::Ok, text);
            }
            Err(error) => self.set_error(&error.code, &error.message),
        }
    }

    pub fn open_library_dialog(&mut self) {
        let Some(folder) = rfd::FileDialog::new().set_title(self.tr("開くライブラリのフォルダ", "Library folder to open")).pick_folder() else { return };
        self.open_library(&folder.display().to_string());
    }

    pub fn open_library(&mut self, target: &str) {
        let input = SwitchInput { target_path: target.to_string(), expected_settings_revision: None };
        match avs_session::switch_workspace(&self.store.env, &input) {
            Ok(result) => {
                self.switched_library();
                let text = format!("{} {}", self.tr("ライブラリを開きました:", "Opened library:"), result.settings.data_directory.display());
                self.set_status(StatusKind::Ok, text);
            }
            Err(error) => self.set_error(&error.code, &error.message),
        }
    }

    pub fn begin_delete_library(&mut self, target: &str) {
        let plan = avs_session::inspect_delete(&self.store.env, target);
        self.workspace.delete_plan = Some(plan);
    }

    pub fn begin_restore_library(&mut self) {
        let Some(folder) = rfd::FileDialog::new().set_title(self.tr("復元する .deleted フォルダ", "The .deleted folder to restore")).pick_folder() else { return };
        self.begin_restore_path(&folder.display().to_string());
    }

    /// `path` may name the original folder or its `.deleted` copy.
    pub fn begin_restore_path(&mut self, path: &str) {
        let original = path.strip_suffix(DELETED_SUFFIX).unwrap_or(path).to_string();
        self.workspace.restore_plan = Some(avs_session::inspect_restore(&self.store.env, &original));
    }

    fn plan_ui(&self, ui: &mut egui::Ui, plan: &WorkspacePlan) {
        egui::Grid::new("workspace-plan").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label(self.tr("フォルダ", "Folder"));
            ui.label(plan.data_directory.as_ref().map(|d| d.path.display().to_string()).unwrap_or_default());
            ui.end_row();
            if let Some(summary) = &plan.summary {
                ui.label(self.tr("プロジェクト", "Project"));
                ui.label(format!("{}  (rev {}, {} clips, {} media)", summary.title, summary.revision, summary.clip_count, summary.media_count));
                ui.end_row();
            }
            if let Some(moved) = &plan.deleted_data_directory_path {
                ui.label(self.tr("移動先", "Moves to"));
                ui.label(moved.display().to_string());
                ui.end_row();
            }
        });
        for blocker in &plan.blockers {
            ui.colored_label(ui.visuals().error_fg_color, format!("{}: {}", blocker.code, blocker.message));
        }
        for warning in &plan.warnings {
            ui.label(format!("{}: {}", warning.code, warning.message));
        }
    }

    pub fn workspace_windows_ui(&mut self, ctx: &egui::Context) {
        if let Some(plan) = self.workspace.delete_plan.clone() {
            let mut open = true;
            let mut confirmed = false;
            let mut cancelled = false;
            egui::Window::new(self.tr("ライブラリを削除", "Delete Library")).collapsible(false).resizable(false).open(&mut open).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).show(ctx, |ui| {
                ui.label(self.tr(
                    "フォルダは消さずに「.deleted」の付いた名前へ移し、最近使った項目から外します。あとで「削除したライブラリを復元」で戻せます。",
                    "The folder is not erased: it is renamed with “.deleted” and removed from the recent list. Restore it later with “Restore Deleted Library”.",
                ));
                ui.separator();
                self.plan_ui(ui, &plan);
                ui.separator();
                ui.horizontal(|ui| {
                    let can = plan.blockers.is_empty();
                    let button = ui.add_enabled(can, egui::Button::new(self.tr("削除する", "Delete")));
                    crate::ui_inspector::register_target(ui, "button:confirm-delete-library", button.rect);
                    if button.clicked() {
                        confirmed = true;
                    }
                    if ui.button(self.tr("キャンセル", "Cancel")).clicked() {
                        cancelled = true;
                    }
                });
            });
            if confirmed {
                let data = plan.data_directory.as_ref().map(|d| d.path.display().to_string()).unwrap_or_default();
                let input = ConfirmedInput { target_path: plan.target.input.clone(), confirm_data_directory: data, expected_recent_revision: None };
                match avs_session::delete_workspace(&self.store.env, &input) {
                    Ok(result) => {
                        let text = format!("{} {}", self.tr("削除しました（移動先）:", "Deleted (moved to):"), result.moved_to.display());
                        self.set_status(StatusKind::Ok, text);
                    }
                    Err(error) => self.set_error(&error.code, &error.message),
                }
                self.refresh_recent();
            }
            if confirmed || cancelled || !open {
                self.workspace.delete_plan = None;
            }
        }
        if let Some(plan) = self.workspace.restore_plan.clone() {
            let mut open = true;
            let mut confirmed = false;
            let mut cancelled = false;
            egui::Window::new(self.tr("削除したライブラリを復元", "Restore Deleted Library")).collapsible(false).resizable(false).open(&mut open).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).show(ctx, |ui| {
                self.plan_ui(ui, &plan);
                ui.separator();
                ui.horizontal(|ui| {
                    let button = ui.add_enabled(plan.blockers.is_empty(), egui::Button::new(self.tr("復元する", "Restore")));
                    crate::ui_inspector::register_target(ui, "button:confirm-restore-library", button.rect);
                    if button.clicked() {
                        confirmed = true;
                    }
                    if ui.button(self.tr("キャンセル", "Cancel")).clicked() {
                        cancelled = true;
                    }
                });
            });
            if confirmed {
                let data = plan.data_directory.as_ref().map(|d| d.path.display().to_string()).unwrap_or_default();
                let input = ConfirmedInput { target_path: plan.target.input.clone(), confirm_data_directory: data, expected_recent_revision: None };
                match avs_session::restore_workspace(&self.store.env, &input) {
                    Ok(result) => {
                        let text = format!("{} {}", self.tr("復元しました:", "Restored:"), result.restored_data_directory.display());
                        self.set_status(StatusKind::Ok, text);
                    }
                    Err(error) => self.set_error(&error.code, &error.message),
                }
                self.refresh_recent();
            }
            if confirmed || cancelled || !open {
                self.workspace.restore_plan = None;
            }
        }
    }
}
