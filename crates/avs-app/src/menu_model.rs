//! What the menu bar contains: every menu, item, enabled state and check mark,
//! computed from the editor state. It has no windowing code, so it can be tested;
//! `native_menu` turns it into the macOS menu bar.

use crate::i18n::Lang;
use crate::keys::Command;

/// What choosing a menu item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Command(Command),
    NewLibrary,
    OpenLibrary,
    OpenRecent(String),
    DeleteLibrary(String),
    RestoreLibrary,
    ShowLibraryInFinder,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Item { action: Action, label: String, enabled: bool, checked: Option<bool> },
    Separator,
    Submenu { label: String, enabled: bool, entries: Vec<Entry> },
    /// A greyed-out line of explanation.
    Note(String),
}

/// Which system role a top-level menu has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The first menu, titled with the application name by macOS.
    App,
    Plain,
    Window,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub role: Role,
    pub label: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecentLibrary {
    pub title: String,
    /// The folder, as the action needs it.
    pub data_directory: String,
    /// The folder, as the menu shows it (see `short_path`).
    pub location: String,
    pub is_current: bool,
    pub reachable: bool,
}

/// The editor state the menus depend on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MenuState {
    pub has_selection: bool,
    pub has_media: bool,
    pub has_range: bool,
    pub has_mark_in: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub can_paste: bool,
    pub library_switch_blocked: bool,
    pub compact_tracks: bool,
    pub snap: bool,
    pub recent: Vec<RecentLibrary>,
}

fn command(lang: Lang, command: Command, enabled: bool) -> Entry {
    Entry::Item { action: Action::Command(command), label: command.label(lang).to_string(), enabled, checked: None }
}

fn checked(lang: Lang, command: Command, on: bool) -> Entry {
    Entry::Item { action: Action::Command(command), label: command.label(lang).to_string(), enabled: true, checked: Some(on) }
}

fn item(action: Action, label: &str, enabled: bool) -> Entry {
    Entry::Item { action, label: label.to_string(), enabled, checked: None }
}

fn recent_entries(lang: Lang, state: &MenuState, for_delete: bool) -> Vec<Entry> {
    let mut entries: Vec<Entry> = state
        .recent
        .iter()
        // The open library cannot be deleted from under the editor.
        .filter(|recent| !(for_delete && recent.is_current))
        .map(|recent| {
            let label = format!("{}{}  —  {}", if recent.is_current { "● " } else { "" }, recent.title, recent.location);
            let action = if for_delete { Action::DeleteLibrary(recent.data_directory.clone()) } else { Action::OpenRecent(recent.data_directory.clone()) };
            Entry::Item { action, label, enabled: for_delete || (recent.reachable && !recent.is_current), checked: None }
        })
        .collect();
    if entries.is_empty() {
        entries.push(Entry::Note(lang.pick("（項目がありません）", "(none)").to_string()));
    }
    entries
}

/// A folder for a menu line: the home folder becomes `~`, and a path longer than
/// `max` characters keeps its end (the part that tells libraries apart).
pub fn short_path(path: &str, home: &str, max: usize) -> String {
    let home = home.trim_end_matches('/');
    let text = match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => path.to_string(),
    };
    let count = text.chars().count();
    if count <= max {
        return text;
    }
    let tail: String = text.chars().skip(count - max.saturating_sub(1)).collect();
    format!("…{tail}")
}

/// The whole menu bar, left to right.
pub fn menus(lang: Lang, state: &MenuState) -> Vec<Menu> {
    use Command::*;
    let tr = |ja: &'static str, en: &'static str| lang.pick(ja, en);
    let can_switch = !state.library_switch_blocked;
    let deletable = state.has_selection || state.has_range;

    let app = vec![command(lang, ShowSettings, true), Entry::Separator, item(Action::Quit, tr("Agent Video Studio を終了", "Quit Agent Video Studio"), true)];

    let mut file = vec![
        item(Action::NewLibrary, tr("新しいライブラリ…", "New Library…"), can_switch),
        item(Action::OpenLibrary, tr("ライブラリを開く…", "Open Library…"), can_switch),
        Entry::Submenu { label: tr("最近使った項目", "Open Recent").to_string(), enabled: can_switch, entries: recent_entries(lang, state, false) },
        Entry::Submenu { label: tr("ライブラリを削除", "Delete Library").to_string(), enabled: can_switch, entries: recent_entries(lang, state, true) },
        item(Action::RestoreLibrary, tr("削除したライブラリを復元…", "Restore Deleted Library…"), can_switch),
    ];
    if state.library_switch_blocked {
        file.push(Entry::Note(tr("環境変数でライブラリが固定されているため切り替えできません", "The library is fixed by an environment variable").to_string()));
    }
    file.extend([
        Entry::Separator,
        command(lang, Import, true),
        command(lang, Export, true),
        command(lang, RecordNarration, true),
        Entry::Separator,
        item(Action::ShowLibraryInFinder, tr("Finder でライブラリを表示", "Show Library in Finder"), true),
    ]);

    let edit = vec![
        command(lang, Undo, state.can_undo),
        command(lang, Redo, state.can_redo),
        Entry::Separator,
        command(lang, Cut, state.has_selection),
        command(lang, Copy, state.has_selection),
        command(lang, Paste, state.can_paste),
        command(lang, Duplicate, state.has_selection),
        Entry::Separator,
        command(lang, SelectAll, true),
        command(lang, Deselect, state.has_selection),
        command(lang, SelectClipAtPlayhead, true),
        Entry::Separator,
        command(lang, BladeAtPlayhead, true),
        command(lang, DeleteRipple, deletable),
        command(lang, DeleteLeaveGap, deletable),
        command(lang, TrimStartToPlayhead, true),
        command(lang, TrimEndToPlayhead, true),
        command(lang, ToggleClipEnabled, state.has_selection),
        Entry::Separator,
        command(lang, AddCaption, true),
    ];

    let mark = vec![
        command(lang, MarkIn, true),
        command(lang, MarkOut, true),
        command(lang, MarkClip, true),
        command(lang, ClearRange, state.has_range || state.has_mark_in),
        Entry::Separator,
        command(lang, PrevEdit, true),
        command(lang, NextEdit, true),
    ];

    let media = vec![
        command(lang, AppendMedia, state.has_media),
        command(lang, InsertMedia, state.has_media),
        command(lang, OverwriteMedia, state.has_media),
        command(lang, ConnectMedia, state.has_media),
    ];

    let tools = vec![command(lang, ToolSelect, true), command(lang, ToolTrim, true), command(lang, ToolBlade, true), command(lang, ToolRange, true), Entry::Separator, checked(lang, ToggleSnap, state.snap)];

    let view = vec![command(lang, ZoomIn, true), command(lang, ZoomOut, true), command(lang, ZoomToFit, true), checked(lang, ToggleTrackHeight, state.compact_tracks)];

    let playback = [PlayPause, PlayReverse, PlayStop, PlayForward, FrameBack, FrameForward, TenFramesBack, TenFramesForward, GoToStart, GoToEnd].into_iter().map(|each| command(lang, each, true)).collect();

    let help = vec![command(lang, ShowHelp, true)];

    let menu = |role: Role, ja: &'static str, en: &'static str, entries: Vec<Entry>| Menu { role, label: tr(ja, en).to_string(), entries };
    vec![
        menu(Role::App, "Agent Video Studio", "Agent Video Studio", app),
        menu(Role::Plain, "ファイル", "File", file),
        menu(Role::Plain, "編集", "Edit", edit),
        menu(Role::Plain, "マーク", "Mark", mark),
        menu(Role::Plain, "素材", "Media", media),
        menu(Role::Plain, "ツール", "Tools", tools),
        menu(Role::Plain, "表示", "View", view),
        menu(Role::Plain, "再生", "Playback", playback),
        menu(Role::Window, "ウインドウ", "Window", Vec::new()),
        menu(Role::Help, "ヘルプ", "Help", help),
    ]
}

/// The same menus with every enabled flag and check mark cleared: two models with
/// equal shapes differ only in state that can be updated in place.
pub fn shape(menus: &[Menu]) -> Vec<Menu> {
    fn strip(entries: &[Entry]) -> Vec<Entry> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Item { action, label, checked, .. } => Entry::Item { action: action.clone(), label: label.clone(), enabled: true, checked: checked.map(|_| false) },
                Entry::Submenu { label, entries, .. } => Entry::Submenu { label: label.clone(), enabled: true, entries: strip(entries) },
                other => other.clone(),
            })
            .collect()
    }
    menus.iter().map(|menu| Menu { role: menu.role, label: menu.label.clone(), entries: strip(&menu.entries) }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn commands(entries: &[Entry], out: &mut Vec<Command>) {
        for entry in entries {
            match entry {
                Entry::Item { action: Action::Command(command), .. } => out.push(*command),
                Entry::Submenu { entries, .. } => commands(entries, out),
                _ => {}
            }
        }
    }

    /// Every command in the key list is reachable from the menu bar, once.
    #[test]
    fn every_command_is_in_exactly_one_menu() {
        let mut in_menus = Vec::new();
        for menu in menus(Lang::Ja, &MenuState::default()) {
            commands(&menu.entries, &mut in_menus);
        }
        let unique: HashSet<Command> = in_menus.iter().copied().collect();
        assert_eq!(unique.len(), in_menus.len(), "a command is listed twice");
        for (_, _, group) in Command::groups() {
            for command in group {
                assert!(unique.contains(&command), "{command:?} is missing from the menu bar");
            }
        }
    }

    #[test]
    fn state_changes_keep_the_shape_and_recent_libraries_change_it() {
        let idle = menus(Lang::Ja, &MenuState::default());
        let busy = menus(Lang::Ja, &MenuState { has_selection: true, can_undo: true, snap: true, compact_tracks: true, ..MenuState::default() });
        assert_ne!(idle, busy);
        assert_eq!(shape(&idle), shape(&busy));
        let recent = RecentLibrary { title: "A".into(), data_directory: "/tmp/a".into(), location: "/tmp/a".into(), is_current: false, reachable: true };
        let with_recent = menus(Lang::Ja, &MenuState { recent: vec![recent], ..MenuState::default() });
        assert_ne!(shape(&idle), shape(&with_recent));
    }

    #[test]
    fn menu_paths_are_short() {
        assert_eq!(short_path("/Users/m/Movies/Lib", "/Users/m", 40), "~/Movies/Lib");
        assert_eq!(short_path("/Users/m", "/Users/m/", 40), "~");
        // Another user's folder that merely starts with the same letters is not home.
        assert_eq!(short_path("/Users/mm/Lib", "/Users/m", 40), "/Users/mm/Lib");
        assert_eq!(short_path("/Volumes/SSD/projects/2026/long-name/Project Library", "/Users/m", 20), "…ame/Project Library");
        assert_eq!(short_path("/日本語/ライブラリ", "", 5), "…イブラリ");
    }

    #[test]
    fn the_open_library_is_not_offered_for_deletion_or_reopening() {
        let current = RecentLibrary { title: "Now".into(), data_directory: "/tmp/now".into(), location: "/tmp/now".into(), is_current: true, reachable: true };
        let all = menus(Lang::En, &MenuState { recent: vec![current], ..MenuState::default() });
        let file = &all[1].entries;
        let Entry::Submenu { entries: open, .. } = &file[2] else { panic!("Open Recent") };
        let Entry::Submenu { entries: delete, .. } = &file[3] else { panic!("Delete Library") };
        assert!(matches!(&open[0], Entry::Item { enabled: false, .. }));
        assert!(matches!(&delete[0], Entry::Note(_)));
    }
}
