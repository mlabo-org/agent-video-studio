//! The editor window: state, the frame loop and command dispatch. Each screen
//! area lives in its own `ui_*` module as an `impl AvsApp` block.

use crate::app_shell_foundation::{
    AppShellLanguage, AppShellPreferences, apply_app_shell_preferences, install_macos_system_fonts, load_app_shell_preferences, save_app_shell_preferences,
};
use crate::edit::{self, ClipboardClips, DragKind, PlaceMode, Tool};
use crate::export_job::ExportTask;
use crate::geom::{self, TimeScale};
use crate::i18n::{self, Lang};
use crate::import_job::ImportTask;
use crate::keys::{self, Command};
use crate::media_cache::MediaCache;
use crate::menu_model::{Action, MenuState, RecentLibrary};
use crate::native_menu::{NativeMenu, SAME_PRESS, Source};
use crate::player::{self, Player, Transport};
use crate::recorder::Recorder;
use crate::selftest::SelfTest;
use crate::store::{Reload, Store};
use crate::workspace_ui::WorkspaceDialogs;
use avs_core::{Edge, MediaType, Operation, Project, Time, TrackKind};
use eframe::egui;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const APP_SHELL_STORAGE_KEY: &str = "agent-video-studio.app-shell-preferences.v1";
const VIEW_STORAGE_KEY: &str = "agent-video-studio.view.v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Ok,
    Error,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub kind: StatusKind,
    pub text: String,
    pub at: Instant,
}

/// A timeline drag in progress.
#[derive(Debug, Clone)]
pub struct Gesture {
    pub kind: DragKind,
    pub origin_x: f32,
    /// The last operations that applied cleanly, and the snap line they used.
    pub operation: Option<Vec<Operation>>,
    pub snapped: Option<Time>,
}

/// View settings kept between launches (eframe storage, its own key; the App Shell
/// preferences have theirs). Panel sizes are in points; `None` means "first launch".
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ViewPrefs {
    pub snap: bool,
    pub px_per_sec: f64,
    pub timeline_height: Option<f32>,
    pub library_width: Option<f32>,
    pub inspector_width: Option<f32>,
    /// Lower track rows, to see more tracks at once.
    pub compact_tracks: bool,
    /// Zero-based input channel the narration recorder takes.
    pub narration_channel: usize,
}

impl Default for ViewPrefs {
    fn default() -> Self {
        ViewPrefs { snap: true, px_per_sec: 40.0, timeline_height: None, library_width: None, inspector_width: None, compact_tracks: false, narration_channel: 0 }
    }
}

pub struct Options {
    pub audio: bool,
    pub selftest: Option<SelfTest>,
    pub sandbox_note: Option<String>,
}

pub struct AvsApp {
    pub prefs: AppShellPreferences,
    pub system_language: AppShellLanguage,
    pub lang: Lang,
    pub store: Store,
    pub player: Player,
    pub cache: MediaCache,
    pub view: ViewPrefs,

    pub playhead: Time,
    pub selection: Vec<String>,
    pub selected_media: Option<String>,
    pub mark_in: Option<Time>,
    pub mark_out: Option<Time>,
    pub tool: Tool,
    pub scale: TimeScale,
    pub track_scroll: f32,
    pub target_video: Option<String>,
    pub target_audio: Option<String>,

    pub gesture: Option<Gesture>,
    /// Result of an unfinished drag or slider move; drawn instead of the stored project.
    pub preview: Option<Project>,
    /// Operations that make `preview`, committed when the control is released.
    pub preview_ops: Option<Vec<Operation>>,
    pub marquee: Option<(egui::Pos2, egui::Pos2)>,
    pub scrubbing: bool,

    pub status: Option<Status>,
    pub external_edit_at: Option<Instant>,
    pub clipboard: Option<ClipboardClips>,
    pub show_help: bool,
    pub show_settings: bool,
    pub confirm_delete_track: Option<String>,
    pub export: Option<ExportTask>,
    pub imports: Vec<ImportTask>,
    pub recorder: Option<Recorder>,
    pub workspace: WorkspaceDialogs,
    pub library_search: String,
    pub library_kind: Option<MediaType>,
    pub selftest: Option<SelfTest>,
    /// Named screen rectangles of this frame (self-test clicks find widgets by name).
    pub targets: HashMap<String, egui::Rect>,
    pub viewer_size: [u32; 2],
    /// Space the viewer had last frame (points).
    pub viewer_avail: egui::Vec2,
    /// Bumped on each drag/slider preview so the viewer renders the previewed picture.
    pub preview_gen: u64,
    pub fit_requested: bool,
    pub sandbox_note: Option<String>,
    pub quit_requested: bool,
    /// Where the playhead was when the timeline last made sure it was visible.
    pub shown_playhead: Time,
    /// The macOS menu bar.
    pub native_menu: NativeMenu,
    /// The last command a key or the menu bar ran, to drop its echo from the other.
    last_command: Option<(Command, Source, Instant)>,
    pub layout: crate::ui_timeline::Layout,
    /// Clip and time under the last right-click in the timeline.
    pub context_at: Option<(Option<String>, Time)>,
    /// When the inspector was last asked to draw attention to itself.
    pub inspector_flash: Option<Instant>,
    pub ctx: egui::Context,
    /// Local time zone offset, for showing activity times.
    pub local_offset: i64,
    /// Track being renamed in its header, with the text typed so far.
    pub renaming: Option<(String, String)>,
    pub rename_focus: bool,
    /// Milliseconds spent building recent frames (newest last).
    pub frame_ms: std::collections::VecDeque<f32>,
}

impl AvsApp {
    pub fn new(cc: &eframe::CreationContext<'_>, store: Store, options: Options) -> AvsApp {
        install_macos_system_fonts(&cc.egui_ctx).expect("the managed macOS UI font must be available");
        let prefs = load_app_shell_preferences(cc.storage, APP_SHELL_STORAGE_KEY);
        apply_app_shell_preferences(&cc.egui_ctx, prefs);
        let view: ViewPrefs = cc.storage.and_then(|storage| eframe::get_value(storage, VIEW_STORAGE_KEY)).unwrap_or_default();
        let system_language = i18n::system_language();
        let lang = Lang::from_shell(prefs.language.resolve(system_language));
        let player = Player::new(&cc.egui_ctx, store.media_root(), options.audio);
        let cache = MediaCache::new(&store.data_directory(), &store.media_root());
        let scale = TimeScale { origin_x: 0.0, px_per_sec: view.px_per_sec, scroll: 0.0 };
        let mut app = AvsApp {
            prefs,
            system_language,
            lang,
            store,
            player,
            cache,
            view,
            playhead: Time::ZERO,
            selection: Vec::new(),
            selected_media: None,
            mark_in: None,
            mark_out: None,
            tool: Tool::Select,
            scale,
            track_scroll: 0.0,
            target_video: None,
            target_audio: None,
            gesture: None,
            preview: None,
            preview_ops: None,
            marquee: None,
            scrubbing: false,
            status: None,
            external_edit_at: None,
            clipboard: None,
            show_help: false,
            show_settings: false,
            confirm_delete_track: None,
            export: None,
            imports: Vec::new(),
            recorder: None,
            workspace: WorkspaceDialogs::default(),
            library_search: String::new(),
            library_kind: None,
            selftest: options.selftest,
            targets: HashMap::new(),
            viewer_size: [640, 360],
            viewer_avail: egui::vec2(640.0, 360.0),
            preview_gen: 0,
            fit_requested: true,
            sandbox_note: options.sandbox_note,
            quit_requested: false,
            shown_playhead: Time::ZERO,
            native_menu: NativeMenu::install(&cc.egui_ctx),
            last_command: None,
            layout: Default::default(),
            context_at: None,
            inspector_flash: None,
            ctx: cc.egui_ctx.clone(),
            local_offset: crate::ui_inspector::local_offset_seconds(),
            frame_ms: Default::default(),
            renaming: None,
            rename_focus: false,
        };
        if let Some(error) = app.store.load_error.clone() {
            app.set_error(&error.code, &error.message);
        } else if let Some(problem) = app.player.audio_problem.clone() {
            app.set_status(StatusKind::Info, format!("{}: {problem}", app.tr("音声出力なしで再生します", "Playing without sound")));
        }
        app
    }

    pub fn tr(&self, ja: &'static str, en: &'static str) -> &'static str {
        self.lang.pick(ja, en)
    }

    pub fn set_status(&mut self, kind: StatusKind, text: impl Into<String>) {
        self.status = Some(Status { kind, text: text.into(), at: Instant::now() });
    }

    pub fn set_error(&mut self, code: &str, message: &str) {
        self.set_status(StatusKind::Error, format!("{code}: {message}"));
    }

    /// The project as drawn: the live result of a drag, else the stored copy.
    pub fn view_project(&self) -> Option<&Project> {
        self.preview.as_ref().or_else(|| self.store.project())
    }

    pub fn fps(&self) -> f64 {
        self.store.project().map_or(30.0, |project| project.fps)
    }

    pub fn range(&self) -> Option<(Time, Time)> {
        match (self.mark_in, self.mark_out) {
            (Some(start), Some(end)) if end > start => Some((start, end)),
            _ => None,
        }
    }

    // ------------------------------------------------------------ editing

    /// Sends operations to the session as one undo step and adopts the result.
    pub fn commit(&mut self, operations: Vec<Operation>) -> bool {
        self.preview = None;
        self.preview_ops = None;
        if operations.is_empty() {
            return false;
        }
        match self.store.apply(&operations) {
            Ok(applied) => {
                // The operation's own summary is English text for agents; a Japanese
                // window names the edit itself.
                let japanese = if self.lang == Lang::Ja { operations.last().and_then(|operation| i18n::edit_label_ja(&operation.tool_name())) } else { None };
                let summary = match japanese {
                    Some(label) => label.to_string(),
                    None => applied.outcomes.last().map(|outcome| outcome.summary.clone()).unwrap_or_default(),
                };
                self.set_status(StatusKind::Ok, format!("{summary}  ({} rev {})", self.tr("保存済み", "saved"), applied.revision));
                self.after_project_change();
                true
            }
            Err(error) => {
                self.set_error(&error.code, &error.message);
                false
            }
        }
    }

    /// Applies operations to the drawn copy only (drag and slider previews).
    pub fn preview_operations(&mut self, operations: Vec<Operation>) -> Result<(), avs_core::EditError> {
        let Some(base) = self.store.project() else { return Ok(()) };
        let mut project = base.clone();
        for operation in &operations {
            project = avs_core::apply(&project, operation)?.0;
        }
        self.preview = Some(project);
        self.preview_ops = Some(operations);
        self.preview_gen += 1;
        Ok(())
    }

    pub fn cancel_preview(&mut self) {
        self.preview = None;
        self.preview_ops = None;
    }

    pub fn commit_preview(&mut self) {
        if let Some(operations) = self.preview_ops.take() {
            self.commit(operations);
        }
        self.preview = None;
    }

    /// Keeps selection and targets valid and restarts playback on the new project.
    fn after_project_change(&mut self) {
        let Some(project) = self.store.project() else { return };
        self.selection.retain(|id| project.clip(id).is_ok());
        if let Some(media) = &self.selected_media
            && project.media(media).is_err() {
                self.selected_media = None;
            }
        for target in [&mut self.target_video, &mut self.target_audio] {
            if target.as_ref().is_some_and(|id| project.track(id).is_err()) {
                *target = None;
            }
        }
        if let Transport::Playing(rate) = self.player.transport
            && rate > 0.0 {
                let at = self.player.time();
                let project = project.clone();
                self.player.play(&project, at, rate, self.viewer_size);
            }
    }

    pub fn undo(&mut self) {
        self.cancel_preview();
        match self.store.undo() {
            Ok(applied) if applied.changed => {
                self.set_status(StatusKind::Ok, format!("{}  (rev {})", self.tr("取り消しました", "Undone"), applied.revision));
                self.after_project_change();
            }
            Ok(_) => self.set_status(StatusKind::Info, self.tr("取り消せる操作がありません", "Nothing to undo")),
            Err(error) => self.set_error(&error.code, &error.message),
        }
    }

    pub fn redo(&mut self) {
        self.cancel_preview();
        match self.store.redo() {
            Ok(applied) if applied.changed => {
                self.set_status(StatusKind::Ok, format!("{}  (rev {})", self.tr("やり直しました", "Redone"), applied.revision));
                self.after_project_change();
            }
            Ok(_) => self.set_status(StatusKind::Info, self.tr("やり直せる操作がありません", "Nothing to redo")),
            Err(error) => self.set_error(&error.code, &error.message),
        }
    }

    pub fn select_clip(&mut self, clip_id: &str, toggle: bool, add: bool) {
        let Some(project) = self.store.project() else { return };
        let pair = project.with_companions(&[clip_id.to_string()]);
        if toggle {
            if self.selection.iter().any(|id| id == clip_id) {
                self.selection.retain(|id| !pair.contains(id));
            } else {
                self.selection.extend(pair);
            }
        } else if add {
            for id in pair {
                if !self.selection.contains(&id) {
                    self.selection.push(id);
                }
            }
        } else {
            self.selection = pair;
        }
    }

    pub fn set_playhead(&mut self, at: Time) {
        let end = self.store.project().map_or(Time::ZERO, Project::duration);
        let at = at.max(Time::ZERO).min(end.max(Time::ZERO));
        if self.player.is_playing() {
            let rate = match self.player.transport {
                Transport::Playing(rate) => rate,
                Transport::Stopped => 1.0,
            };
            if let Some(project) = self.store.project().cloned() {
                self.player.play(&project, at, rate, self.viewer_size);
            }
        }
        self.playhead = at;
    }

    /// Target track for a media kind: the one the user picked, else the default.
    pub fn target_track(&self, kind: TrackKind) -> Option<String> {
        let project = self.store.project()?;
        let picked = match kind {
            TrackKind::Audio => self.target_audio.clone(),
            _ => self.target_video.clone(),
        };
        picked.filter(|id| project.track(id).is_ok()).or_else(|| edit::default_target(project, kind))
    }

    /// E / W / D / Q with the selected media item.
    pub fn place_selected_media(&mut self, mode: PlaceMode) {
        let Some(media_id) = self.selected_media.clone() else {
            self.set_status(StatusKind::Info, self.tr("先に素材ライブラリで素材を選んでください", "Select a media item in the library first"));
            return;
        };
        self.place_media(&media_id, mode, None, self.playhead);
    }

    pub fn place_media(&mut self, media_id: &str, mode: PlaceMode, track: Option<String>, at: Time) {
        let Some(project) = self.store.project().cloned() else { return };
        let Ok(media) = project.media(media_id).cloned() else { return };
        let kind = edit::track_kind_for(media.media_type);
        let Some(target) = track.or_else(|| self.target_track(kind)) else {
            self.set_error("NO_TARGET_TRACK", self.tr("この素材を置けるトラックがありません", "There is no track for this media"));
            return;
        };
        match edit::place_media(&project, &media, mode, &target, at) {
            Ok(operations) => {
                if self.commit(operations)
                    && let Some(project) = self.store.project() {
                        // Select what was just placed: the newest clips of that media.
                        let placed: Vec<String> = project.clips().filter(|clip| clip.media_id.as_deref() == Some(media_id) && clip.start_seconds >= at.min(project.duration())).map(|clip| clip.id.clone()).collect();
                        if let Some(last) = placed.last() {
                            self.selection = project.with_companions(std::slice::from_ref(last));
                        }
                    }
            }
            Err(error) => self.set_error(error.code, &error.message),
        }
    }

    pub fn add_caption_at_playhead(&mut self) {
        let Some(project) = self.store.project() else { return };
        let track = project.tracks.iter().find(|track| track.kind == TrackKind::Caption && !track.is_locked() && !track.clips.iter().any(|clip| clip.intersects(self.playhead, self.playhead + Time::from_micros(3_000_000))));
        let mut operations = Vec::new();
        let track_id = match track {
            Some(track) => track.id.clone(),
            None => {
                let id = project.fresh_id("C", |candidate| project.tracks.iter().any(|track| track.id == candidate));
                operations.push(Operation::AddTrack(avs_core::ops::AddTrack {
                    id: Some(id.clone()),
                    kind: TrackKind::Caption,
                    name: None,
                    insert_before_track_id: None,
                    insert_after_track_id: None,
                    locked: None,
                    muted: None,
                }));
                id
            }
        };
        let text = self.tr("字幕", "Caption").to_string();
        operations.push(Operation::AddCaptions(avs_core::ops::AddCaptions {
            track_id: Some(track_id),
            captions: vec![avs_core::ops::CaptionInput {
                id: None,
                track_id: None,
                text,
                start_seconds: self.playhead,
                duration_seconds: None,
                end_seconds: None,
                speaker: None,
                language: None,
                properties: None,
            }],
        }));
        if self.commit(operations)
            && let Some(project) = self.store.project()
                && let Some(clip) = project.clips().filter(|clip| clip.kind == avs_core::ClipKind::Caption && clip.start_seconds == self.playhead).last() {
                    self.selection = vec![clip.id.clone()];
                }
    }

    // ------------------------------------------------------------ commands

    pub fn run(&mut self, command: Command, ctx: &egui::Context) {
        let Some(project) = self.store.project().cloned() else {
            if matches!(command, Command::ShowHelp | Command::ShowSettings) {
                self.toggle_window(command);
            }
            return;
        };
        let fps = project.fps;
        match command {
            Command::PlayPause => {
                if self.player.is_playing() {
                    self.player.stop();
                } else {
                    let from = if self.playhead >= project.duration() { Time::ZERO } else { self.playhead };
                    self.playhead = from;
                    self.player.play(&project, from, 1.0, self.viewer_size);
                }
            }
            Command::PlayForward => {
                let rate = match self.player.transport {
                    Transport::Playing(rate) if rate > 0.0 => (rate * 2.0).min(8.0),
                    _ => 1.0,
                };
                self.player.play(&project, self.playhead, rate, self.viewer_size);
            }
            Command::PlayReverse => {
                let rate = match self.player.transport {
                    Transport::Playing(rate) if rate < 0.0 => (rate * 2.0).max(-8.0),
                    _ => -1.0,
                };
                self.player.play(&project, self.playhead, rate, self.viewer_size);
            }
            Command::PlayStop => self.player.stop(),
            Command::FrameBack => self.step(-1, fps),
            Command::FrameForward => self.step(1, fps),
            Command::TenFramesBack => self.step(-10, fps),
            Command::TenFramesForward => self.step(10, fps),
            Command::PrevEdit => self.set_playhead(edit::prev_edit(&project, self.playhead)),
            Command::NextEdit => self.set_playhead(edit::next_edit(&project, self.playhead)),
            Command::GoToStart => self.set_playhead(Time::ZERO),
            Command::GoToEnd => self.set_playhead(project.duration()),
            Command::ToolSelect => self.tool = Tool::Select,
            Command::ToolTrim => self.tool = Tool::Trim,
            Command::ToolBlade => self.tool = Tool::Blade,
            Command::ToolRange => self.tool = Tool::Range,
            Command::ToggleSnap => {
                self.view.snap = !self.view.snap;
                let text = if self.view.snap { self.tr("スナップ: オン", "Snapping: on") } else { self.tr("スナップ: オフ", "Snapping: off") };
                self.set_status(StatusKind::Info, text);
            }
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::Copy | Command::Cut => {
                match edit::copy_clips(&project, &self.selection) {
                    Some(text) => {
                        self.clipboard = edit::parse_clipboard(&text);
                        ctx.copy_text(text);
                        if command == Command::Cut {
                            self.commit(edit::delete(&project, &self.selection, None, true));
                            self.selection.clear();
                        } else {
                            self.set_status(StatusKind::Info, self.tr("クリップをコピーしました", "Copied clips"));
                        }
                    }
                    None => self.set_status(StatusKind::Info, self.tr("コピーするクリップを選んでください", "Select clips to copy")),
                }
            }
            Command::Paste => {
                let Some(copied) = self.clipboard.clone() else {
                    self.set_status(StatusKind::Info, self.tr("ペーストするクリップがありません", "Nothing to paste"));
                    return;
                };
                if self.paste(&project, &copied, self.playhead) {
                    // As in Final Cut Pro, the playhead ends up after what was pasted.
                    let span = copied.clips.iter().map(|clip| clip.start_seconds).min().zip(edit::clipboard_end(&copied)).map(|(start, end)| end - start);
                    if let Some(span) = span {
                        self.set_playhead(self.playhead + span);
                    }
                }
            }
            Command::Duplicate => match edit::copy_clips(&project, &self.selection).and_then(|text| edit::parse_clipboard(&text)) {
                Some(copied) => {
                    let at = edit::clipboard_end(&copied).unwrap_or(self.playhead);
                    self.paste(&project, &copied, at);
                }
                None => self.set_status(StatusKind::Info, self.tr("複製するクリップを選んでください", "Select clips to duplicate")),
            },
            Command::SelectAll => self.selection = project.clips().map(|clip| clip.id.clone()).collect(),
            Command::SelectClipAtPlayhead => {
                // The topmost clip under the playhead, with its linked partner.
                let hit = project.tracks.iter().flat_map(|track| track.clips.iter()).find(|clip| clip.start_seconds <= self.playhead && self.playhead < clip.end()).map(|clip| clip.id.clone());
                match hit {
                    Some(clip_id) => self.select_clip(&clip_id, false, false),
                    None => self.set_status(StatusKind::Info, self.tr("再生ヘッドの下にクリップがありません", "No clip under the playhead")),
                }
            }
            Command::ToggleTrackHeight => self.view.compact_tracks = !self.view.compact_tracks,
            Command::Deselect => {
                self.selection.clear();
                self.marquee = None;
            }
            Command::BladeAtPlayhead => match edit::blade(&project, &self.selection, self.playhead) {
                Some(operation) => {
                    self.commit(vec![operation]);
                }
                None => self.set_status(StatusKind::Info, self.tr("再生ヘッドの位置に分割できるクリップがありません", "No clip under the playhead to cut")),
            },
            Command::DeleteRipple | Command::DeleteLeaveGap => {
                let ripple = command == Command::DeleteRipple;
                let operations = edit::delete(&project, &self.selection, self.range(), ripple);
                if operations.is_empty() {
                    self.set_status(StatusKind::Info, self.tr("削除するクリップか範囲を選んでください", "Select clips or a range to delete"));
                } else if self.commit(operations) && self.range().is_some()
                    && ripple {
                        self.mark_out = None;
                    }
            }
            Command::TrimStartToPlayhead | Command::TrimEndToPlayhead => {
                let edge = if command == Command::TrimStartToPlayhead { Edge::Start } else { Edge::End };
                match edit::trim_to_playhead(&project, &self.selection, self.playhead, edge) {
                    Some(operation) => {
                        self.commit(vec![operation]);
                    }
                    None => self.set_status(StatusKind::Info, self.tr("再生ヘッドの下にクリップがありません", "No clip under the playhead")),
                }
            }
            Command::ToggleClipEnabled => {
                let operations = edit::toggle_muted(&project, &self.selection);
                if operations.is_empty() {
                    self.set_status(StatusKind::Info, self.tr("クリップを選んでください", "Select clips first"));
                } else {
                    self.commit(operations);
                }
            }
            Command::MarkIn => {
                self.mark_in = Some(self.playhead);
                if self.mark_out.is_some_and(|out| out <= self.playhead) {
                    self.mark_out = None;
                }
            }
            Command::MarkOut => {
                self.mark_out = Some(self.playhead);
                if self.mark_in.is_none_or(|start| start >= self.playhead) {
                    self.mark_in = Some(Time::ZERO.min(self.playhead));
                }
            }
            Command::MarkClip => match edit::clip_range_at(&project, self.playhead) {
                Some((start, end)) => {
                    self.mark_in = Some(start);
                    self.mark_out = Some(end);
                }
                None => self.set_status(StatusKind::Info, self.tr("再生ヘッドの下にクリップがありません", "No clip under the playhead")),
            },
            Command::ClearRange => {
                self.mark_in = None;
                self.mark_out = None;
            }
            Command::AppendMedia => self.place_selected_media(PlaceMode::Append),
            Command::InsertMedia => self.place_selected_media(PlaceMode::Insert),
            Command::OverwriteMedia => self.place_selected_media(PlaceMode::Overwrite),
            Command::ConnectMedia => self.place_selected_media(PlaceMode::Connect),
            Command::AddCaption => self.add_caption_at_playhead(),
            Command::ZoomIn => self.zoom(1.5),
            Command::ZoomOut => self.zoom(1.0 / 1.5),
            Command::ZoomToFit => self.fit_requested = true,
            Command::Import => self.import_dialog(),
            Command::Export => self.open_export(),
            Command::ShowHelp | Command::ShowSettings => self.toggle_window(command),
            Command::RecordNarration => self.open_recorder(),
        }
    }

    fn toggle_window(&mut self, command: Command) {
        match command {
            Command::ShowHelp => self.show_help = !self.show_help,
            _ => self.show_settings = !self.show_settings,
        }
    }

    fn paste(&mut self, project: &Project, copied: &ClipboardClips, at: Time) -> bool {
        let operations = edit::paste_clips(project, copied, at);
        if operations.is_empty() {
            self.set_status(StatusKind::Info, self.tr("このプロジェクトに貼り付けられるクリップがありません", "None of the copied clips fit this project"));
            return false;
        }
        let before: Vec<String> = project.clips().map(|clip| clip.id.clone()).collect();
        let pasted = self.commit(operations);
        if pasted && let Some(project) = self.store.project() {
            self.selection = project.clips().filter(|clip| !before.contains(&clip.id)).map(|clip| clip.id.clone()).collect();
        }
        pasted
    }

    fn step(&mut self, frames: i64, fps: f64) {
        self.player.stop();
        self.set_playhead(geom::step_frames(self.playhead, frames, fps));
    }

    pub fn zoom(&mut self, factor: f64) {
        let anchor = self.scale.x(self.playhead);
        self.scale.zoom_at(factor, anchor);
        self.view.px_per_sec = self.scale.px_per_sec;
    }

    pub fn import_dialog(&mut self) {
        let files = rfd::FileDialog::new()
            .set_title(self.tr("素材を読み込む", "Import Media"))
            .add_filter(self.tr("動画・音声・画像", "Video, audio, images"), &["mp4", "mov", "m4v", "webm", "mkv", "mp3", "wav", "m4a", "aac", "flac", "ogg", "png", "jpg", "jpeg", "webp", "gif"])
            .pick_files();
        for file in files.unwrap_or_default() {
            self.start_import(file, None);
        }
    }

    /// Copies and probes on a worker thread; registers (and places) when done.
    pub fn start_import(&mut self, path: PathBuf, place: Option<(String, Time)>) {
        self.imports.push(ImportTask::start(&self.store, path, place));
    }

    fn poll_imports(&mut self) {
        let mut finished = Vec::new();
        for (index, task) in self.imports.iter_mut().enumerate() {
            if let Some(result) = task.poll() {
                finished.push((index, result));
            }
        }
        for (index, result) in finished.into_iter().rev() {
            let task = self.imports.remove(index);
            match result {
                Ok(prepared) => {
                    let media_id = prepared.register_media.id.clone().unwrap_or_default();
                    let mut operations = vec![Operation::RegisterMedia(prepared.register_media.clone())];
                    if let Some((track_id, at)) = &task.place {
                        let placed = self.store.project().cloned().and_then(|project| {
                            let working = avs_core::apply(&project, &operations[0]).ok()?.0;
                            let media = working.media(&media_id).ok()?.clone();
                            let kind = edit::track_kind_for(media.media_type);
                            let track = working.track(track_id).ok().filter(|track| track.kind == kind).map(|track| track.id.clone()).or_else(|| edit::default_target(&working, kind))?;
                            Some(edit::place_media(&working, &media, PlaceMode::At, &track, *at))
                        });
                        match placed {
                            Some(Ok(more)) => operations.extend(more),
                            Some(Err(error)) => self.set_error(error.code, &error.message),
                            None => {}
                        }
                    }
                    let register = operations[0].clone();
                    match self.store.apply(&operations) {
                        Ok(applied) => {
                            self.set_status(StatusKind::Ok, format!("{} {}  ({} rev {})", self.tr("読み込みました:", "Imported:"), task.file_name, self.tr("保存済み", "saved"), applied.revision));
                            self.selected_media = Some(media_id);
                            self.after_project_change();
                        }
                        // Placing failed: keep the media (a recording must not be lost) and say why.
                        Err(error) if operations.len() > 1 => match self.store.apply(std::slice::from_ref(&register)) {
                            Ok(_) => {
                                let text = format!("{} {} ({}: {})", self.tr("素材は読み込みましたが、タイムラインに置けませんでした:", "Imported, but it could not be placed:"), task.file_name, error.code, error.message);
                                self.set_status(StatusKind::Error, text);
                                self.selected_media = Some(media_id);
                                self.after_project_change();
                            }
                            Err(error) => {
                                avs_session::discard_import(&prepared);
                                self.set_error(&error.code, &error.message);
                            }
                        },
                        Err(error) => {
                            avs_session::discard_import(&prepared);
                            self.set_error(&error.code, &error.message);
                        }
                    }
                }
                Err(error) => self.set_error(&error.code, &error.message),
            }
        }
    }

    // ------------------------------------------------------------ frame

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // A text field has the keyboard, or a menu is open (Esc must close it).
        if ctx.egui_wants_keyboard_input() || egui::Popup::is_any_open(ctx) {
            return;
        }
        let mut commands: Vec<Command> = Vec::new();
        let mut paste_text: Option<String> = None;
        ctx.input_mut(|input| {
            input.events.retain(|event| match event {
                egui::Event::Key { key, physical_key, pressed: true, repeat, modifiers } => match keys::command_for(*key, *physical_key, *modifiers) {
                    Some(command) => {
                        if !*repeat || command.repeats() {
                            commands.push(command);
                        }
                        false
                    }
                    None => true,
                },
                egui::Event::Key { key, physical_key, pressed: false, modifiers, .. } => keys::command_for(*key, *physical_key, *modifiers).is_none(),
                egui::Event::Copy => {
                    commands.push(Command::Copy);
                    false
                }
                egui::Event::Cut => {
                    commands.push(Command::Cut);
                    false
                }
                egui::Event::Paste(text) => {
                    paste_text = Some(text.clone());
                    commands.push(Command::Paste);
                    false
                }
                _ => true,
            });
        });
        if let Some(text) = paste_text
            && let Some(copied) = edit::parse_clipboard(&text) {
                self.clipboard = Some(copied);
            }
        for command in commands {
            self.run_from(command, Source::Key, ctx);
        }
    }

    /// Runs a command that a key press or the menu bar asked for. One press of a
    /// menu shortcut can arrive from both; the second arrival is dropped.
    fn run_from(&mut self, command: Command, source: Source, ctx: &egui::Context) {
        let echo = self.last_command.is_some_and(|(last, from, at)| last == command && from != source && at.elapsed() < SAME_PRESS);
        if echo {
            return;
        }
        self.last_command = Some((command, source, Instant::now()));
        self.run(command, ctx);
    }

    fn menu_state(&mut self) -> MenuState {
        if self.workspace.recent.is_none() {
            self.refresh_recent();
        }
        let current = self.store.data_directory();
        let home = self.store.env.home.display().to_string();
        let recent = self.workspace.recent.as_ref().map(|list| list.entries.as_slice()).unwrap_or_default();
        let recent = recent
            .iter()
            .map(|entry| RecentLibrary {
                title: entry.summary.as_ref().map(|summary| summary.title.clone()).unwrap_or_default(),
                data_directory: entry.data_directory.clone(),
                location: crate::menu_model::short_path(&entry.data_directory, &home, 56),
                is_current: std::path::Path::new(&entry.data_directory) == current,
                reachable: entry.reachable,
            })
            .collect();
        let snapshot = self.store.snapshot.as_ref();
        MenuState {
            has_selection: !self.selection.is_empty(),
            has_media: self.selected_media.is_some(),
            has_range: self.range().is_some(),
            has_mark_in: self.mark_in.is_some(),
            can_undo: snapshot.is_some_and(|snapshot| snapshot.undo_depth > 0),
            can_redo: snapshot.is_some_and(|snapshot| snapshot.redo_depth > 0),
            can_paste: self.clipboard.is_some(),
            library_switch_blocked: self.store.env.library_root_active() || self.store.env.session_path_active(),
            compact_tracks: self.view.compact_tracks,
            snap: self.view.snap,
            recent,
        }
    }

    /// Brings the menu bar up to date and runs what was chosen from it.
    fn handle_menu_bar(&mut self, ctx: &egui::Context) {
        let state = self.menu_state();
        // Same condition as the key handler: a text field or an open popup owns the keys.
        let shortcuts = !(ctx.egui_wants_keyboard_input() || egui::Popup::is_any_open(ctx));
        self.native_menu.sync(self.lang, &state, shortcuts);
        for (action, repeat) in self.native_menu.take_actions() {
            // Like the key handler: a held key repeats only the commands meant to repeat.
            if repeat && !matches!(&action, Action::Command(command) if command.repeats()) {
                continue;
            }
            match action {
                Action::Command(Command::Paste) => {
                    // A menu shortcut never reaches egui as a paste event, so take the
                    // clips another editor window copied from the system clipboard here.
                    let system = std::process::Command::new("/usr/bin/pbpaste").output().ok().map(|output| String::from_utf8_lossy(&output.stdout).into_owned());
                    if let Some(copied) = system.as_deref().and_then(edit::parse_clipboard) {
                        self.clipboard = Some(copied);
                    }
                    self.run_from(Command::Paste, Source::Menu, ctx);
                }
                Action::Command(command) => self.run_from(command, Source::Menu, ctx),
                Action::NewLibrary => self.new_library(),
                Action::OpenLibrary => self.open_library_dialog(),
                Action::OpenRecent(directory) => self.open_library(&directory),
                Action::DeleteLibrary(directory) => self.begin_delete_library(&directory),
                Action::RestoreLibrary => self.begin_restore_library(),
                Action::ShowLibraryInFinder => {
                    let _ = std::process::Command::new("/usr/bin/open").arg(self.store.data_directory()).spawn();
                }
                Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let (dropped, pointer) = ctx.input(|input| (input.raw.dropped_files.clone(), input.pointer.latest_pos()));
        if dropped.is_empty() {
            return;
        }
        let place = pointer.and_then(|pos| self.timeline_drop_target(pos));
        for file in dropped {
            if let Some(path) = file.path {
                self.start_import(path, place.clone());
            }
        }
    }

    fn poll_store(&mut self) {
        // An agent switched the active library: the editor shows the active library,
        // so it follows, and says so.
        if let Some(active) = self.store.library_switched_elsewhere() {
            self.switched_library();
            self.refresh_recent();
            self.external_edit_at = Some(Instant::now());
            let text = format!("{} {}", self.tr("外部でライブラリが切り替えられたので開き直しました:", "The active library was switched elsewhere; now showing:"), active.display());
            self.set_status(StatusKind::Info, text);
            return;
        }
        let before = self.store.revision();
        match self.store.poll(false) {
            Reload::Reloaded { external } => {
                if external {
                    // Somebody else (an agent through MCP) changed the project: drop
                    // any half-finished drag of the old project and say so.
                    self.gesture = None;
                    self.cancel_preview();
                    self.external_edit_at = Some(Instant::now());
                    let text = format!("{} (rev {} → {})", self.tr("外部で編集されたので読み込み直しました", "Reloaded after an external edit"), before, self.store.revision());
                    self.set_status(StatusKind::Info, text);
                }
                self.after_project_change();
            }
            Reload::Failed => {
                if let Some(error) = self.store.load_error.clone() {
                    self.set_error(&error.code, &error.message);
                }
            }
            Reload::Unchanged => {}
        }
    }

    pub fn switched_library(&mut self) {
        self.player.stop();
        self.store.reopen();
        self.player.set_media_root(self.store.media_root());
        self.cache = MediaCache::new(&self.store.data_directory(), &self.store.media_root());
        self.selection.clear();
        self.selected_media = None;
        self.mark_in = None;
        self.mark_out = None;
        self.playhead = Time::ZERO;
        self.target_video = None;
        self.target_audio = None;
        self.cancel_preview();
        self.fit_requested = true;
        if let Some(error) = self.store.load_error.clone() {
            self.set_error(&error.code, &error.message);
        }
    }

    fn frame_begin(&mut self, ctx: &egui::Context) {
        self.targets.clear();
        // Esc cancels a track rename before the text field sees it (it would only lose focus).
        if self.renaming.is_some() && ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.renaming = None;
            ctx.memory_mut(|memory| memory.stop_text_input());
        }
        // A preview whose control is gone (released, closed, deselected) is committed,
        // so a stale preview can never hide the stored project or stop reloads.
        if self.preview_ops.is_some() && self.gesture.is_none() && !ctx.input(|input| input.pointer.any_down()) && !egui::Popup::is_any_open(ctx) {
            self.commit_preview();
        }
        self.lang = Lang::from_shell(self.prefs.language.resolve(self.system_language));
        if self.gesture.is_none() && self.preview_ops.is_none() {
            self.poll_store();
        }
        if self.cache.poll(ctx) {
            ctx.request_repaint();
        }
        self.poll_imports();
        self.poll_export();
        if let Some(project) = self.store.project() {
            let media: Vec<avs_core::MediaAsset> = project.media.clone();
            for item in &media {
                self.cache.want_strip(item);
                self.cache.want_wave(item);
            }
        }
        if let Some(project) = self.store.project().cloned() {
            if let Some(at) = self.player.tick(&project, self.store.revision(), self.viewer_size) {
                self.playhead = at;
            } else if !self.player.is_playing() {
                // Stopped: the playhead sits on a frame, so cuts and marks land on frames.
                self.playhead = geom::quantize(self.playhead, project.fps).min(project.duration().max(Time::ZERO));
                let at = self.playhead;
                let project = self.view_project().cloned().unwrap_or(project);
                let revision = if self.preview.is_some() { u64::MAX - self.preview_gen } else { self.store.revision() };
                self.player.request_frame(&project, revision, at, self.viewer_size, false);
            }
        }
        self.handle_dropped_files(ctx);
        self.handle_shortcuts(ctx);
    }

    fn schedule_repaint(&self, ctx: &egui::Context) {
        if self.player.is_playing() || self.gesture.is_some() || self.selftest.is_some() || self.recorder.as_ref().is_some_and(Recorder::is_recording) {
            ctx.request_repaint();
        } else if self.player.scrub_busy() || !self.imports.is_empty() || self.export.is_some() || self.cache.pending() {
            ctx.request_repaint_after(Duration::from_millis(50));
        } else {
            // Keep watching the session file for other writers.
            ctx.request_repaint_after(crate::store::POLL_INTERVAL);
        }
    }
}

impl eframe::App for AvsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let began = Instant::now();
        let ctx = ui.ctx().clone();
        self.frame_begin(&ctx);
        let after_begin = began.elapsed().as_secs_f32() * 1000.0;
        if let Some(project) = self.store.project() {
            self.viewer_size = player::render_size(project, self.viewer_avail, ctx.pixels_per_point());
        }

        self.handle_menu_bar(&ctx);
        egui::Panel::top("toolbar").show_inside(ui, |ui| self.toolbar_ui(ui));
        egui::Panel::bottom("status").show_inside(ui, |ui| self.status_bar_ui(ui));
        // The three edges between the areas are draggable; the sizes are remembered.
        let full = ui.available_size();
        let timeline_max = (full.y - 220.0).max(200.0);
        let timeline = egui::Panel::bottom("timeline")
            .resizable(true)
            .default_size(self.view.timeline_height.unwrap_or(full.y * 0.44).clamp(160.0, timeline_max))
            .size_range(160.0..=timeline_max)
            .show_inside(ui, |ui| self.timeline_ui(ui));
        self.view.timeline_height = Some(timeline.response.rect.height());
        self.targets.insert("panel:timeline".into(), timeline.response.rect);
        let side_max = (full.x * 0.4).max(240.0);
        let library = egui::Panel::left("library")
            .resizable(true)
            .default_size(self.view.library_width.unwrap_or(290.0).clamp(180.0, side_max))
            .size_range(180.0..=side_max)
            .show_inside(ui, |ui| self.library_ui(ui));
        self.view.library_width = Some(library.response.rect.width());
        self.targets.insert("panel:library".into(), library.response.rect);
        let inspector = egui::Panel::right("inspector")
            .resizable(true)
            .default_size(self.view.inspector_width.unwrap_or(320.0).clamp(220.0, side_max))
            .size_range(220.0..=side_max)
            .show_inside(ui, |ui| self.right_column_ui(ui));
        self.view.inspector_width = Some(inspector.response.rect.width());
        self.targets.insert("panel:inspector".into(), inspector.response.rect);
        egui::CentralPanel::default().show_inside(ui, |ui| self.viewer_ui(ui));

        self.windows_ui(&ctx);
        if let Some(mut test) = self.selftest.take() {
            test.after_frame(self, &ctx);
            self.selftest = Some(test);
        }
        if self.quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.schedule_repaint(&ctx);
        let total = began.elapsed().as_secs_f32() * 1000.0;
        if total > 100.0 && std::env::var_os("AVS_SELFTEST_VERBOSE").is_some() {
            eprintln!("slow frame: {total:.0} ms (of which frame start {after_begin:.0} ms)");
        }
        self.frame_ms.push_back(total);
        if self.frame_ms.len() > 120 {
            self.frame_ms.pop_front();
        }
    }

    /// Panel sizes are kept in the view preferences (one owner), not in egui's memory.
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        save_app_shell_preferences(storage, APP_SHELL_STORAGE_KEY, &self.prefs);
        eframe::set_value(storage, VIEW_STORAGE_KEY, &self.view);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(test) = &mut self.selftest {
            test.inject(ctx, raw_input, &self.targets);
        }
    }
}

impl AvsApp {
    /// Re-applies the display preferences after the settings window changed them.
    pub fn apply_prefs(&mut self, ctx: &egui::Context) {
        apply_app_shell_preferences(ctx, self.prefs);
        self.lang = Lang::from_shell(self.prefs.language.resolve(self.system_language));
    }
}
