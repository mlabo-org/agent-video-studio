//! The macOS menu bar, built from `menu_model`. A choice arrives as an `Action`
//! on the next frame; the editor runs it the same way it runs a key press.

use crate::i18n::Lang;
use crate::keys::Chord;
use crate::menu_model::{self, Action, Entry, Menu as Model, MenuState, Role};
use eframe::egui::{self, Key};
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use std::sync::mpsc::{Receiver, channel};

/// An item whose state changes without the menu being rebuilt.
enum Handle {
    Item(MenuItem),
    Check(CheckMenuItem),
    Submenu(Submenu),
}

pub struct NativeMenu {
    menu: Menu,
    /// Whether `menu` has replaced the application's menu bar yet.
    installed: bool,
    /// The chosen item's id, and whether a held key repeated the choice.
    events: Receiver<(String, bool)>,
    /// What the menu bar currently shows.
    model: Vec<Model>,
    /// Bumped on every rebuild so a click on a replaced item is ignored.
    generation: u64,
    actions: Vec<Action>,
    /// Items and submenus in the order `menu_model::menus` lists them.
    handles: Vec<Handle>,
    /// Whether the items currently carry their keys.
    shortcuts_on: bool,
}

/// The key code of a chord's key, for the keys the binding table uses.
fn code(key: Key) -> Option<Code> {
    Some(match key {
        Key::Space => Code::Space,
        Key::ArrowLeft => Code::ArrowLeft,
        Key::ArrowRight => Code::ArrowRight,
        Key::ArrowUp => Code::ArrowUp,
        Key::ArrowDown => Code::ArrowDown,
        Key::Home => Code::Home,
        Key::End => Code::End,
        Key::Backspace => Code::Backspace,
        Key::Delete => Code::Delete,
        Key::Escape => Code::Escape,
        Key::OpenBracket => Code::BracketLeft,
        Key::CloseBracket => Code::BracketRight,
        Key::Equals | Key::Plus => Code::Equal,
        Key::Minus => Code::Minus,
        Key::Slash => Code::Slash,
        Key::Comma => Code::Comma,
        letter => {
            let name = letter.name();
            if name.len() != 1 || !name.as_bytes()[0].is_ascii_uppercase() {
                return None;
            }
            format!("Key{name}").parse().ok()?
        }
    })
}

pub fn accelerator(chord: Chord) -> Option<Accelerator> {
    let mut modifiers = Modifiers::empty();
    if chord.command {
        modifiers |= Modifiers::META;
    }
    if chord.shift {
        modifiers |= Modifiers::SHIFT;
    }
    if chord.alt {
        modifiers |= Modifiers::ALT;
    }
    Some(Accelerator::new(modifiers, code(chord.key)?))
}

impl NativeMenu {
    /// Prepares the menu bar; the first `sync` fills and shows it. Must run on the
    /// main thread.
    pub fn install(ctx: &egui::Context) -> NativeMenu {
        let (sender, events) = channel();
        let ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = sender.send((event.id().0.clone(), is_key_repeat()));
            // The window may be idle; wake it so the choice runs now.
            ctx.request_repaint();
        }));
        NativeMenu { menu: Menu::new(), installed: false, events, model: Vec::new(), generation: 0, actions: Vec::new(), handles: Vec::new(), shortcuts_on: true }
    }

    /// Makes the menu bar show `state`. `shortcuts` is false while a text field or a
    /// popup has the keyboard: the items then carry no key, so ⌘C, arrows and Esc
    /// reach the field instead of the menu.
    pub fn sync(&mut self, lang: Lang, state: &MenuState, shortcuts: bool) {
        let model = menu_model::menus(lang, state);
        // An item's key cannot be taken away once set (the menu library ignores the
        // request on macOS), so a change of `shortcuts` builds the menus again.
        let keys_changed = shortcuts != self.shortcuts_on;
        self.shortcuts_on = shortcuts;
        if model != self.model || keys_changed {
            if !keys_changed && menu_model::shape(&model) == menu_model::shape(&self.model) {
                let mut next = 0;
                for menu in &model {
                    self.update(&menu.entries, &mut next);
                }
            } else {
                self.rebuild(lang, &model);
            }
            self.model = model;
        }
        if !self.installed {
            // Done on the first frame, not at start-up: the windowing layer installs
            // its own default menu when the application finishes launching, and that
            // would replace one installed earlier.
            self.menu.init_for_nsapp();
            self.installed = true;
        }
    }

    /// The choices made since the last call, oldest first, each with whether a held
    /// key repeated it (a menu shortcut repeats while its key is down).
    pub fn take_actions(&mut self) -> Vec<(Action, bool)> {
        let mut actions = Vec::new();
        while let Ok((id, repeat)) = self.events.try_recv() {
            let Some((generation, index)) = id.split_once(':') else { continue };
            if generation.parse() != Ok(self.generation) {
                continue;
            }
            if let Some(action) = index.parse::<usize>().ok().and_then(|index| self.actions.get(index)) {
                actions.push((action.clone(), repeat));
            }
        }
        actions
    }

    fn update(&self, entries: &[Entry], next: &mut usize) {
        for entry in entries {
            match entry {
                Entry::Item { enabled, checked, .. } => {
                    match &self.handles[*next] {
                        Handle::Item(item) => item.set_enabled(*enabled),
                        Handle::Check(item) => {
                            item.set_enabled(*enabled);
                            item.set_checked(checked.unwrap_or(false));
                        }
                        Handle::Submenu(_) => {}
                    }
                    *next += 1;
                }
                Entry::Submenu { enabled, entries, .. } => {
                    if let Handle::Submenu(submenu) = &self.handles[*next] {
                        submenu.set_enabled(*enabled);
                    }
                    *next += 1;
                    self.update(entries, next);
                }
                Entry::Separator | Entry::Note(_) => {}
            }
        }
    }

    fn rebuild(&mut self, lang: Lang, model: &[Model]) {
        while self.menu.remove_at(0).is_some() {}
        self.generation += 1;
        self.actions.clear();
        self.handles.clear();
        let tr = |ja: &'static str, en: &'static str| Some(lang.pick(ja, en));
        for menu in model {
            let submenu = Submenu::new(&menu.label, true);
            let _ = self.menu.append(&submenu);
            match menu.role {
                Role::App => {
                    // Settings sits between About and the system's own items; Quit is last.
                    let (settings, rest) = menu.entries.split_at(menu.entries.len().min(1));
                    let _ = submenu.append_items(&[&PredefinedMenuItem::about(tr("Agent Video Studio について", "About Agent Video Studio"), None), &PredefinedMenuItem::separator()]);
                    self.fill(&submenu, settings);
                    let _ = submenu.append_items(&[
                        &PredefinedMenuItem::separator(),
                        &PredefinedMenuItem::services(tr("サービス", "Services")),
                        &PredefinedMenuItem::separator(),
                        &PredefinedMenuItem::hide(tr("Agent Video Studio を非表示", "Hide Agent Video Studio")),
                        &PredefinedMenuItem::hide_others(tr("ほかを非表示", "Hide Others")),
                        &PredefinedMenuItem::show_all(tr("すべてを表示", "Show All")),
                    ]);
                    self.fill(&submenu, rest);
                }
                Role::Window => {
                    let _ = submenu.append_items(&[
                        &PredefinedMenuItem::minimize(tr("しまう", "Minimize")),
                        &PredefinedMenuItem::maximize(tr("拡大／縮小", "Zoom")),
                        &PredefinedMenuItem::fullscreen(tr("フルスクリーンにする", "Enter Full Screen")),
                        &PredefinedMenuItem::separator(),
                        &PredefinedMenuItem::bring_all_to_front(tr("すべてを手前に移動", "Bring All to Front")),
                    ]);
                    self.fill(&submenu, &menu.entries);
                    submenu.set_as_windows_menu_for_nsapp();
                }
                Role::Help => {
                    self.fill(&submenu, &menu.entries);
                    submenu.set_as_help_menu_for_nsapp();
                }
                Role::Plain => self.fill(&submenu, &menu.entries),
            }
        }
    }

    fn fill(&mut self, parent: &Submenu, entries: &[Entry]) {
        for entry in entries {
            match entry {
                Entry::Separator => {
                    let _ = parent.append(&PredefinedMenuItem::separator());
                }
                Entry::Note(text) => {
                    let _ = parent.append(&MenuItem::new(text, false, None));
                }
                Entry::Submenu { label, enabled, entries } => {
                    let submenu = Submenu::new(label, *enabled);
                    let _ = parent.append(&submenu);
                    self.handles.push(Handle::Submenu(submenu.clone()));
                    self.fill(&submenu, entries);
                }
                Entry::Item { action, label, enabled, checked } => {
                    let id = format!("{}:{}", self.generation, self.actions.len());
                    self.actions.push(action.clone());
                    let shortcut = match action {
                        Action::Command(command) => command.chord().and_then(accelerator),
                        Action::Quit => accelerator(Chord { key: Key::Q, command: true, shift: false, alt: false }),
                        _ => None,
                    };
                    let shown = if self.shortcuts_on { shortcut } else { None };
                    match checked {
                        Some(on) => {
                            let item = CheckMenuItem::with_id(id, label, *enabled, *on, shown);
                            let _ = parent.append(&item);
                            self.handles.push(Handle::Check(item));
                        }
                        None => {
                            let item = MenuItem::with_id(id, label, *enabled, shown);
                            let _ = parent.append(&item);
                            self.handles.push(Handle::Item(item));
                        }
                    }
                }
            }
        }
    }
}

/// Whether the event being handled right now is a key held down long enough to
/// repeat. The menu handler runs inside that event, on the main thread.
fn is_key_repeat() -> bool {
    use objc2_app_kit::{NSApplication, NSEventType};
    let Some(main_thread) = objc2::MainThreadMarker::new() else { return false };
    let event = NSApplication::sharedApplication(main_thread).currentEvent();
    event.is_some_and(|event| event.r#type() == NSEventType::KeyDown && event.isARepeat())
}

/// Whether a command that just ran from one source should be dropped when the
/// other source reports it too: one ⌘Z press can reach the editor both as a menu
/// choice and as a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Key,
    Menu,
}

pub const SAME_PRESS: std::time::Duration = std::time::Duration::from_millis(250);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{BINDINGS, Command};

    /// Every key in the binding table can be shown in the menu bar.
    #[test]
    fn every_binding_has_a_menu_shortcut() {
        for (command, chord) in BINDINGS {
            assert!(accelerator(*chord).is_some(), "{command:?} has a key the menu bar cannot show");
        }
        assert!(Command::ToggleTrackHeight.chord().is_none());
    }
}
