//! Every user command, its key binding (Final Cut Pro conventions) and its label.
//! One table feeds the keyboard handler, the menus, the tooltips and the help
//! window, so a key shown anywhere is the key that works.

use crate::i18n::Lang;
use eframe::egui::{Key, Modifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    // Playback
    PlayPause,
    PlayReverse,
    PlayStop,
    PlayForward,
    FrameBack,
    FrameForward,
    TenFramesBack,
    TenFramesForward,
    PrevEdit,
    NextEdit,
    GoToStart,
    GoToEnd,
    // Tools
    ToolSelect,
    ToolTrim,
    ToolBlade,
    ToolRange,
    ToggleSnap,
    // Edit
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Duplicate,
    SelectAll,
    Deselect,
    SelectClipAtPlayhead,
    BladeAtPlayhead,
    DeleteRipple,
    DeleteLeaveGap,
    TrimStartToPlayhead,
    TrimEndToPlayhead,
    ToggleClipEnabled,
    // Marking
    MarkIn,
    MarkOut,
    MarkClip,
    ClearRange,
    // Media to timeline
    AppendMedia,
    InsertMedia,
    OverwriteMedia,
    ConnectMedia,
    AddCaption,
    // View
    ZoomIn,
    ZoomOut,
    ZoomToFit,
    ToggleTrackHeight,
    // App
    Import,
    Export,
    ShowHelp,
    ShowSettings,
    RecordNarration,
}

/// A key plus required modifiers. `command` is ⌘ on macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub key: Key,
    pub command: bool,
    pub shift: bool,
    pub alt: bool,
}

const fn plain(key: Key) -> Chord {
    Chord { key, command: false, shift: false, alt: false }
}
const fn cmd(key: Key) -> Chord {
    Chord { key, command: true, shift: false, alt: false }
}
const fn shift(key: Key) -> Chord {
    Chord { key, command: false, shift: true, alt: false }
}
const fn alt(key: Key) -> Chord {
    Chord { key, command: false, shift: false, alt: true }
}
const fn cmd_shift(key: Key) -> Chord {
    Chord { key, command: true, shift: true, alt: false }
}

/// The binding table. The first chord of a command is the one shown in menus.
/// ⌘C / ⌘X / ⌘V never arrive as key events (the windowing layer turns them into
/// copy/cut/paste events); they are listed so menus and help show them.
pub const BINDINGS: &[(Command, Chord)] = &[
    (Command::PlayPause, plain(Key::Space)),
    (Command::PlayReverse, plain(Key::J)),
    (Command::PlayStop, plain(Key::K)),
    (Command::PlayForward, plain(Key::L)),
    (Command::FrameBack, plain(Key::ArrowLeft)),
    (Command::FrameForward, plain(Key::ArrowRight)),
    (Command::TenFramesBack, shift(Key::ArrowLeft)),
    (Command::TenFramesForward, shift(Key::ArrowRight)),
    (Command::PrevEdit, plain(Key::ArrowUp)),
    (Command::NextEdit, plain(Key::ArrowDown)),
    (Command::GoToStart, plain(Key::Home)),
    (Command::GoToEnd, plain(Key::End)),
    (Command::ToolSelect, plain(Key::A)),
    (Command::ToolTrim, plain(Key::T)),
    (Command::ToolBlade, plain(Key::B)),
    (Command::ToolRange, plain(Key::R)),
    (Command::ToggleSnap, plain(Key::N)),
    (Command::Undo, cmd(Key::Z)),
    (Command::Redo, cmd_shift(Key::Z)),
    (Command::Cut, cmd(Key::X)),
    (Command::Copy, cmd(Key::C)),
    (Command::Paste, cmd(Key::V)),
    (Command::Duplicate, cmd(Key::D)),
    (Command::SelectAll, cmd(Key::A)),
    (Command::Deselect, plain(Key::Escape)),
    (Command::Deselect, cmd_shift(Key::A)),
    (Command::SelectClipAtPlayhead, plain(Key::C)),
    (Command::BladeAtPlayhead, cmd(Key::B)),
    (Command::DeleteRipple, plain(Key::Backspace)),
    (Command::DeleteLeaveGap, shift(Key::Backspace)),
    (Command::DeleteLeaveGap, plain(Key::Delete)),
    (Command::TrimStartToPlayhead, alt(Key::OpenBracket)),
    (Command::TrimEndToPlayhead, alt(Key::CloseBracket)),
    (Command::ToggleClipEnabled, plain(Key::V)),
    (Command::MarkIn, plain(Key::I)),
    (Command::MarkOut, plain(Key::O)),
    (Command::MarkClip, plain(Key::X)),
    (Command::ClearRange, alt(Key::X)),
    (Command::AppendMedia, plain(Key::E)),
    (Command::InsertMedia, plain(Key::W)),
    (Command::OverwriteMedia, plain(Key::D)),
    (Command::ConnectMedia, plain(Key::Q)),
    (Command::AddCaption, alt(Key::C)),
    (Command::ZoomIn, cmd(Key::Equals)),
    (Command::ZoomIn, cmd(Key::Plus)),
    (Command::ZoomOut, cmd(Key::Minus)),
    (Command::ZoomToFit, shift(Key::Z)),
    (Command::Import, cmd(Key::I)),
    (Command::Export, cmd(Key::E)),
    (Command::ShowHelp, cmd(Key::Slash)),
    (Command::ShowSettings, cmd(Key::Comma)),
    (Command::RecordNarration, Chord { key: Key::A, command: false, shift: true, alt: true }),
];

impl Chord {
    fn matches(&self, key: Key, modifiers: Modifiers) -> bool {
        self.key == key && self.command == modifiers.command && self.shift == modifiers.shift && self.alt == modifiers.alt
    }

    /// macOS-style label: ⌥⇧⌘ then the key.
    pub fn label(&self) -> String {
        let mut text = String::new();
        if self.alt {
            text.push('⌥');
        }
        if self.shift {
            text.push('⇧');
        }
        if self.command {
            text.push('⌘');
        }
        text.push_str(key_label(self.key));
        text
    }
}

fn key_label(key: Key) -> &'static str {
    match key {
        Key::Space => "Space",
        Key::ArrowLeft => "←",
        Key::ArrowRight => "→",
        Key::ArrowUp => "↑",
        Key::ArrowDown => "↓",
        Key::Home => "Home",
        Key::End => "End",
        Key::Backspace => "⌫",
        Key::Delete => "⌦",
        Key::Escape => "Esc",
        Key::OpenBracket => "[",
        Key::CloseBracket => "]",
        Key::Equals => "+",
        Key::Plus => "+",
        Key::Minus => "−",
        Key::Slash => "/",
        Key::Comma => ",",
        other => other.symbol_or_name(),
    }
}

/// The command a key press triggers. `physical` is the key at the same position on
/// a US layout; it is used when Option changes the typed character (⌥[ types “).
pub fn command_for(key: Key, physical: Option<Key>, modifiers: Modifiers) -> Option<Command> {
    let find = |key: Key| BINDINGS.iter().find(|(_, chord)| chord.matches(key, modifiers)).map(|(command, _)| *command);
    find(key).or_else(|| if modifiers.alt { physical.and_then(find) } else { None })
}

impl Command {
    /// The chord shown for this command, if it has one.
    pub fn chord(self) -> Option<Chord> {
        BINDINGS.iter().find(|(command, _)| *command == self).map(|(_, chord)| *chord)
    }

    pub fn shortcut(self) -> String {
        self.chord().map(|chord| chord.label()).unwrap_or_default()
    }

    /// The menu and tooltip label.
    pub fn label(self, lang: Lang) -> &'static str {
        use Command::*;
        let (ja, en) = match self {
            PlayPause => ("再生/停止", "Play/Pause"),
            PlayReverse => ("逆方向に再生", "Play Reverse"),
            PlayStop => ("停止", "Stop"),
            PlayForward => ("順方向に再生（連打で倍速）", "Play Forward (repeat for faster)"),
            FrameBack => ("1フレーム戻る", "Previous Frame"),
            FrameForward => ("1フレーム進む", "Next Frame"),
            TenFramesBack => ("10フレーム戻る", "Back 10 Frames"),
            TenFramesForward => ("10フレーム進む", "Forward 10 Frames"),
            PrevEdit => ("前の編集点へ", "Previous Edit"),
            NextEdit => ("次の編集点へ", "Next Edit"),
            GoToStart => ("先頭へ", "Go to Beginning"),
            GoToEnd => ("末尾へ", "Go to End"),
            ToolSelect => ("選択ツール", "Select Tool"),
            ToolTrim => ("トリムツール", "Trim Tool"),
            ToolBlade => ("ブレードツール", "Blade Tool"),
            ToolRange => ("範囲選択ツール", "Range Selection Tool"),
            ToggleSnap => ("スナップ", "Snapping"),
            Undo => ("取り消す", "Undo"),
            Redo => ("やり直す", "Redo"),
            Cut => ("カット", "Cut"),
            Copy => ("コピー", "Copy"),
            Paste => ("再生ヘッドにペースト", "Paste at Playhead"),
            Duplicate => ("複製", "Duplicate"),
            SelectAll => ("すべてを選択", "Select All"),
            Deselect => ("選択を解除", "Deselect All"),
            SelectClipAtPlayhead => ("再生ヘッドのクリップを選択", "Select Clip at Playhead"),
            BladeAtPlayhead => ("再生ヘッドで分割", "Blade at Playhead"),
            DeleteRipple => ("詰めて削除", "Ripple Delete"),
            DeleteLeaveGap => ("隙間を残して削除", "Delete, Leave Gap"),
            TrimStartToPlayhead => ("頭を再生ヘッドまでトリム", "Trim Start to Playhead"),
            TrimEndToPlayhead => ("尻を再生ヘッドまでトリム", "Trim End to Playhead"),
            ToggleClipEnabled => ("クリップのミュート切替", "Toggle Clip Mute"),
            MarkIn => ("範囲の開始を設定", "Set Range Start"),
            MarkOut => ("範囲の終了を設定", "Set Range End"),
            MarkClip => ("クリップの範囲を選択", "Select Clip Range"),
            ClearRange => ("範囲を解除", "Clear Range"),
            AppendMedia => ("末尾に追加", "Append to End"),
            InsertMedia => ("再生ヘッドに挿入", "Insert at Playhead"),
            OverwriteMedia => ("再生ヘッドに上書き", "Overwrite at Playhead"),
            ConnectMedia => ("別トラックに重ねる", "Connect on Another Track"),
            AddCaption => ("再生ヘッドに字幕を追加", "Add Caption at Playhead"),
            ZoomIn => ("拡大", "Zoom In"),
            ZoomOut => ("縮小", "Zoom Out"),
            ZoomToFit => ("全体を表示", "Zoom to Fit"),
            ToggleTrackHeight => ("トラックを低く表示", "Compact Track Height"),
            Import => ("素材を読み込む…", "Import Media…"),
            Export => ("書き出す…", "Export…"),
            ShowHelp => ("キーボードショートカット", "Keyboard Shortcuts"),
            ShowSettings => ("表示設定…", "Display Settings…"),
            RecordNarration => ("ナレーションを録音…", "Record Narration…"),
        };
        lang.pick(ja, en)
    }

    /// Whether holding the key repeats the command. Only stepping, zooming and
    /// undo/redo repeat; a held Space, E or ⌫ must not toggle, place or delete again.
    pub fn repeats(self) -> bool {
        use Command::*;
        matches!(self, FrameBack | FrameForward | TenFramesBack | TenFramesForward | PrevEdit | NextEdit | ZoomIn | ZoomOut | Undo | Redo)
    }

    /// "Label (⌘B)" for tooltips.
    pub fn tooltip(self, lang: Lang) -> String {
        let shortcut = self.shortcut();
        if shortcut.is_empty() { self.label(lang).to_string() } else { format!("{}  ({shortcut})", self.label(lang)) }
    }

    /// Groups for the help window, in display order.
    pub fn groups() -> Vec<(&'static str, &'static str, Vec<Command>)> {
        use Command::*;
        vec![
            ("再生", "Playback", vec![PlayPause, PlayReverse, PlayStop, PlayForward, FrameBack, FrameForward, TenFramesBack, TenFramesForward, PrevEdit, NextEdit, GoToStart, GoToEnd]),
            ("ツール", "Tools", vec![ToolSelect, ToolTrim, ToolBlade, ToolRange, ToggleSnap]),
            (
                "編集",
                "Edit",
                vec![Undo, Redo, Cut, Copy, Paste, Duplicate, SelectAll, Deselect, SelectClipAtPlayhead, BladeAtPlayhead, DeleteRipple, DeleteLeaveGap, TrimStartToPlayhead, TrimEndToPlayhead, ToggleClipEnabled],
            ),
            ("範囲", "Range", vec![MarkIn, MarkOut, MarkClip, ClearRange]),
            ("素材をタイムラインへ", "Media to Timeline", vec![AppendMedia, InsertMedia, OverwriteMedia, ConnectMedia, AddCaption]),
            ("表示", "View", vec![ZoomIn, ZoomOut, ZoomToFit, ToggleTrackHeight]),
            ("アプリ", "App", vec![Import, Export, RecordNarration, ShowSettings, ShowHelp]),
        ]
    }

    /// Every chord of this command (for the help window).
    pub fn all_shortcuts(self) -> Vec<String> {
        BINDINGS.iter().filter(|(command, _)| *command == self).map(|(_, chord)| chord.label()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(command: bool, shift: bool, alt: bool) -> Modifiers {
        Modifiers { alt, ctrl: false, shift, mac_cmd: command, command }
    }

    #[test]
    fn fcp_keys_map_to_commands() {
        assert_eq!(command_for(Key::B, None, mods(true, false, false)), Some(Command::BladeAtPlayhead));
        assert_eq!(command_for(Key::B, None, mods(false, false, false)), Some(Command::ToolBlade));
        assert_eq!(command_for(Key::Backspace, None, mods(false, false, false)), Some(Command::DeleteRipple));
        assert_eq!(command_for(Key::Backspace, None, mods(false, true, false)), Some(Command::DeleteLeaveGap));
        assert_eq!(command_for(Key::Delete, None, mods(false, false, false)), Some(Command::DeleteLeaveGap));
        assert_eq!(command_for(Key::Z, None, mods(true, true, false)), Some(Command::Redo));
        assert_eq!(command_for(Key::Z, None, mods(false, true, false)), Some(Command::ZoomToFit));
        assert_eq!(command_for(Key::X, None, mods(false, false, true)), Some(Command::ClearRange));
        assert_eq!(command_for(Key::Equals, None, mods(true, false, false)), Some(Command::ZoomIn));
        assert_eq!(command_for(Key::Plus, None, mods(true, false, false)), Some(Command::ZoomIn));
    }

    #[test]
    fn option_brackets_fall_back_to_the_physical_key() {
        // ⌥[ types a quote character on a US layout, which egui reports as no key or another key.
        assert_eq!(command_for(Key::Quote, Some(Key::OpenBracket), mods(false, false, true)), Some(Command::TrimStartToPlayhead));
        assert_eq!(command_for(Key::CloseBracket, None, mods(false, false, true)), Some(Command::TrimEndToPlayhead));
        // Without Option the physical fallback is not used.
        assert_eq!(command_for(Key::Quote, Some(Key::OpenBracket), mods(false, false, false)), None);
    }

    #[test]
    fn no_chord_is_bound_twice() {
        for (index, (_, chord)) in BINDINGS.iter().enumerate() {
            for (_, other) in &BINDINGS[index + 1..] {
                assert_ne!(chord, other, "{} bound twice", chord.label());
            }
        }
    }

    #[test]
    fn only_stepping_keys_repeat() {
        assert!(Command::FrameForward.repeats());
        assert!(Command::Undo.repeats());
        for command in [Command::PlayPause, Command::AppendMedia, Command::DeleteRipple, Command::Paste, Command::BladeAtPlayhead] {
            assert!(!command.repeats(), "{command:?}");
        }
    }

    #[test]
    fn labels_use_mac_symbols() {
        assert_eq!(Command::Redo.shortcut(), "⇧⌘Z");
        assert_eq!(Command::TrimStartToPlayhead.shortcut(), "⌥[");
        assert_eq!(Command::DeleteLeaveGap.all_shortcuts(), vec!["⇧⌫".to_string(), "⌦".to_string()]);
    }
}
