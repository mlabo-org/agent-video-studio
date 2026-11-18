//! `--self-test <script.json> --shots <dir>`: drives the real window with a
//! script and saves screenshots of what was actually drawn.
//!
//! Keys and pointer input are injected as raw window events (through
//! `raw_input_hook`), so they take the same path as a real keyboard and mouse:
//! shortcut table, hit testing, drags, menus. Screenshots come from the renderer
//! (`ViewportCommand::Screenshot`). A step that fails is recorded and the run
//! continues; the process exits non-zero if any step failed.

use crate::app::{AvsApp, StatusKind};
use crate::keys::Command;
use avs_core::{Operation, Time};
use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Pos2, Rect};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

/// Exit code of the self-test run, read by `main` after the window closes.
pub static EXIT_CODE: AtomicI32 = AtomicI32::new(0);

const STEP_TIMEOUT: Duration = Duration::from_secs(40);

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Target {
    pub clip: Option<String>,
    /// For a clip: "start", "end", "center" or a fraction 0..1 of its width.
    pub at: Option<Value>,
    pub time: Option<f64>,
    pub track: Option<String>,
    pub widget: Option<String>,
    pub media: Option<String>,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub dx: f32,
    pub dy: f32,
    /// For a widget: a point on its "top", "bottom", "left" or "right" edge instead of the centre.
    pub edge: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Step {
    /// Milliseconds.
    Wait(u64),
    Frames(u32),
    /// "cmd+shift+z", "space", "alt+[", "backspace", ...
    Key(String),
    Type(String),
    Click { target: Target, #[serde(default)] modifiers: String },
    RightClick { target: Target },
    DoubleClick { target: Target },
    Drag { from: Target, to: Target, #[serde(default)] modifiers: String, #[serde(default)] steps: Option<u32> },
    Hover(Target),
    Scroll { target: Target, dx: f32, dy: f32, #[serde(default)] modifiers: String },
    /// A command by name (as in `keys::Command`, e.g. "PlayPause").
    Command(String),
    /// An edit by another writer (as an agent through MCP would), straight to the session file.
    Apply(Value),
    SelectMedia(String),
    Import { path: String, #[serde(default)] track: Option<String>, #[serde(default)] at: Option<f64> },
    Export(String),
    /// Starts an export and goes on at once (to test cancelling).
    ExportStart(String),
    /// Library management without the folder dialog: action is new, open, delete or restore.
    Library { action: String, path: String },
    /// Waits until imports, scrub renders, playback start and export are finished.
    WaitIdle,
    /// A file dropped from Finder at a point of the window.
    DropFile { path: String, target: Target },
    Screenshot(String),
    Dump(String),
    Theme(String),
    Language(String),
    /// Text size preference in points (10-32).
    FontSize(u8),
    Expect(Value),
}

enum Phase {
    Ready,
    /// Waiting for injected events to be consumed, then some frames.
    Events { settle: u32 },
    Until { deadline: Instant, check: fn(&AvsApp) -> bool },
    Screenshot { name: String, deadline: Instant, resend: Instant },
    Sleep { until: Instant },
    Frames(u32),
}

pub struct SelfTest {
    steps: VecDeque<Step>,
    shots: PathBuf,
    phase: Phase,
    queue: VecDeque<(Vec<Event>, Modifiers, Vec<egui::DroppedFile>)>,
    held: Modifiers,
    pointer: Pos2,
    log: Vec<Value>,
    failures: usize,
    index: usize,
    started: Instant,
    warmup: u32,
    finished: bool,
}

fn parse_modifiers(text: &str) -> Modifiers {
    let mut modifiers = Modifiers::NONE;
    for part in text.split('+').map(|part| part.trim().to_ascii_lowercase()) {
        match part.as_str() {
            "cmd" | "command" => {
                modifiers.command = true;
                modifiers.mac_cmd = true;
            }
            "shift" => modifiers.shift = true,
            "alt" | "option" | "opt" => modifiers.alt = true,
            "ctrl" => modifiers.ctrl = true,
            _ => {}
        }
    }
    modifiers
}

/// "cmd+shift+z" → (Z, ⇧⌘). Key names follow `egui::Key::from_name` plus a few aliases.
fn parse_key(text: &str) -> Option<(Key, Modifiers)> {
    let parts: Vec<&str> = text.split('+').collect();
    let (name, mods) = parts.split_last()?;
    let modifiers = parse_modifiers(&mods.join("+"));
    let key = match name.to_ascii_lowercase().as_str() {
        "space" => Key::Space,
        "backspace" => Key::Backspace,
        "delete" | "forwarddelete" => Key::Delete,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "esc" | "escape" => Key::Escape,
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "home" => Key::Home,
        "end" => Key::End,
        "plus" => Key::Plus,
        "equals" | "=" => Key::Equals,
        "minus" | "-" => Key::Minus,
        _ => Key::from_name(name).or_else(|| Key::from_name(&name.to_ascii_uppercase()))?,
    };
    Some((key, modifiers))
}

fn command_by_name(name: &str) -> Option<Command> {
    use Command::*;
    let all = [
        PlayPause, PlayReverse, PlayStop, PlayForward, FrameBack, FrameForward, TenFramesBack, TenFramesForward, PrevEdit, NextEdit, GoToStart, GoToEnd, ToolSelect, ToolTrim, ToolBlade, ToolRange, ToggleSnap, Undo, Redo,
        Cut, Copy, Paste, Duplicate, SelectAll, Deselect, SelectClipAtPlayhead, BladeAtPlayhead, DeleteRipple, DeleteLeaveGap, TrimStartToPlayhead, TrimEndToPlayhead, ToggleClipEnabled, MarkIn, MarkOut, MarkClip, ClearRange, AppendMedia,
        InsertMedia, OverwriteMedia, ConnectMedia, AddCaption, ZoomIn, ZoomOut, ZoomToFit, ToggleTrackHeight, Import, Export, ShowHelp, ShowSettings, RecordNarration,
    ];
    all.into_iter().find(|command| format!("{command:?}") == name)
}

/// A media id from an id or a media name (fixture ids are generated).
fn media_id(app: &AvsApp, name: &str) -> Option<String> {
    let project = app.store.project()?;
    project.media.iter().find(|media| media.id == name || media.name == name).map(|media| media.id.clone())
}

fn idle(app: &AvsApp) -> bool {
    app.imports.is_empty() && !app.player.scrub_busy() && !app.cache.pending() && app.export.as_ref().is_none_or(|task| !matches!(task.state, crate::export_job::ExportState::Running))
}

impl SelfTest {
    /// `${ROOT}` in the script stands for the `--library` folder, so scripts can be rerun anywhere.
    pub fn load(script: &std::path::Path, shots: PathBuf, root: &std::path::Path) -> Result<SelfTest, String> {
        let text = std::fs::read_to_string(script).map_err(|error| format!("cannot read {}: {error}", script.display()))?;
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let text = text.replace("${ROOT}", &root.display().to_string().replace('\\', "\\\\").replace('"', "\\\""));
        let steps: Vec<Step> = serde_json::from_str(&text).map_err(|error| format!("{} is not a valid script: {error}", script.display()))?;
        std::fs::create_dir_all(&shots).map_err(|error| format!("cannot create {}: {error}", shots.display()))?;
        Ok(SelfTest {
            steps: steps.into(),
            shots,
            phase: Phase::Frames(0),
            queue: VecDeque::new(),
            held: Modifiers::NONE,
            pointer: Pos2::new(5.0, 5.0),
            log: Vec::new(),
            failures: 0,
            index: 0,
            started: Instant::now(),
            warmup: 20,
            finished: false,
        })
    }

    /// Called before each frame: hands this frame's injected events to egui.
    pub fn inject(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput, _targets: &HashMap<String, Rect>) {
        if let Some((events, modifiers, dropped)) = self.queue.pop_front() {
            raw.modifiers = modifiers;
            self.held = modifiers;
            raw.events.extend(events);
            raw.dropped_files.extend(dropped);
        } else {
            raw.modifiers = self.held;
        }
        raw.focused = true;
    }

    fn fail(&mut self, message: String) {
        self.failures += 1;
        eprintln!("SELF-TEST FAIL step {}: {message}", self.index);
        self.log.push(json!({ "step": self.index, "ok": false, "message": message }));
    }

    fn pass(&mut self, message: String) {
        self.log.push(json!({ "step": self.index, "ok": true, "message": message }));
    }

    fn resolve(&self, app: &AvsApp, target: &Target) -> Option<Pos2> {
        let base = if let (Some(x), Some(y)) = (target.x, target.y) {
            Some(Pos2::new(x, y))
        } else if let Some(clip) = &target.clip {
            let rect = app.targets.get(&format!("clip:{clip}"))?;
            let x = match &target.at {
                Some(Value::String(at)) if at == "start" => rect.left() + 2.0,
                Some(Value::String(at)) if at == "end" => rect.right() - 2.0,
                Some(Value::Number(fraction)) => rect.left() + rect.width() * fraction.as_f64().unwrap_or(0.5) as f32,
                _ => rect.center().x,
            };
            Some(Pos2::new(x, rect.center().y))
        } else if let Some(time) = target.time {
            let x = app.scale.x(Time::from_secs(time)?);
            let y = match &target.track {
                Some(track) => app.targets.get(&format!("track:{track}"))?.center().y,
                None => app.targets.get("ruler")?.center().y,
            };
            Some(Pos2::new(x, y))
        } else if let Some(media) = &target.media {
            let id = media_id(app, media)?;
            Some(app.targets.get(&format!("media:{id}"))?.center())
        } else if let Some(widget) = &target.widget {
            let registered = app.ctx.data(|data| data.get_temp::<Rect>(egui::Id::new(("avs-target", widget.as_str()))));
            let rect = app.targets.get(widget).copied().or(registered)?;
            Some(match target.edge.as_deref() {
                Some("top") => Pos2::new(rect.center().x, rect.top()),
                Some("bottom") => Pos2::new(rect.center().x, rect.bottom()),
                Some("left") => Pos2::new(rect.left(), rect.center().y),
                Some("right") => Pos2::new(rect.right(), rect.center().y),
                _ => rect.center(),
            })
        } else {
            None
        };
        base.map(|pos| pos + egui::vec2(target.dx, target.dy))
    }

    fn push(&mut self, events: Vec<Event>, modifiers: Modifiers) {
        self.queue.push_back((events, modifiers, Vec::new()));
    }

    fn click(&mut self, pos: Pos2, button: PointerButton, modifiers: Modifiers, count: usize) {
        self.push(vec![Event::PointerMoved(pos)], modifiers);
        for _ in 0..count {
            self.push(vec![Event::PointerButton { pos, button, pressed: true, modifiers }], modifiers);
            self.push(vec![Event::PointerButton { pos, button, pressed: false, modifiers }], modifiers);
        }
        self.push(Vec::new(), Modifiers::NONE);
        self.pointer = pos;
    }

    fn start_step(&mut self, app: &mut AvsApp, ctx: &egui::Context, step: Step) {
        let now = Instant::now();
        self.phase = Phase::Ready;
        match step {
            Step::Wait(ms) => self.phase = Phase::Sleep { until: now + Duration::from_millis(ms) },
            Step::Frames(count) => self.phase = Phase::Frames(count),
            Step::Key(text) => match parse_key(&text) {
                Some((key, modifiers)) => {
                    let event = |pressed| match (key, modifiers.command) {
                        (Key::C, true) if pressed => Event::Copy,
                        (Key::X, true) if pressed => Event::Cut,
                        (Key::V, true) if pressed => Event::Paste(ctx.input(|_| String::new())),
                        _ => Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers },
                    };
                    let press = event(true);
                    if let Event::Paste(_) = press {
                        // The clipboard text is what ⌘C put there; the editor keeps its own copy too.
                        let text = app.clipboard.as_ref().and_then(|copied| serde_json::to_string(copied).ok()).map(|json| format!("{}{json}", crate::edit::CLIPBOARD_PREFIX)).unwrap_or_default();
                        self.push(vec![Event::Paste(text)], modifiers);
                    } else {
                        self.push(vec![press], modifiers);
                        self.push(vec![event(false)], modifiers);
                    }
                    self.push(Vec::new(), Modifiers::NONE);
                    self.phase = Phase::Events { settle: 3 };
                }
                None => self.fail(format!("unknown key {text}")),
            },
            Step::Type(text) => {
                self.push(vec![Event::Text(text)], Modifiers::NONE);
                self.phase = Phase::Events { settle: 2 };
            }
            Step::Click { target, modifiers } => match self.resolve(app, &target) {
                Some(pos) => {
                    self.click(pos, PointerButton::Primary, parse_modifiers(&modifiers), 1);
                    self.phase = Phase::Events { settle: 3 };
                }
                None => self.fail(format!("click target not on screen: {target:?}")),
            },
            Step::DoubleClick { target } => match self.resolve(app, &target) {
                Some(pos) => {
                    self.click(pos, PointerButton::Primary, Modifiers::NONE, 2);
                    self.phase = Phase::Events { settle: 3 };
                }
                None => self.fail(format!("double-click target not on screen: {target:?}")),
            },
            Step::RightClick { target } => match self.resolve(app, &target) {
                Some(pos) => {
                    self.click(pos, PointerButton::Secondary, Modifiers::NONE, 1);
                    self.phase = Phase::Events { settle: 3 };
                }
                None => self.fail(format!("right-click target not on screen: {target:?}")),
            },
            Step::Hover(target) => match self.resolve(app, &target) {
                Some(pos) => {
                    self.push(vec![Event::PointerMoved(pos)], Modifiers::NONE);
                    self.pointer = pos;
                    self.phase = Phase::Events { settle: 30 };
                }
                None => self.fail(format!("hover target not on screen: {target:?}")),
            },
            Step::Scroll { target, dx, dy, modifiers } => match self.resolve(app, &target) {
                Some(pos) => {
                    let modifiers = parse_modifiers(&modifiers);
                    self.push(vec![Event::PointerMoved(pos)], modifiers);
                    self.push(vec![Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(dx, dy), modifiers, phase: egui::TouchPhase::Move }], modifiers);
                    self.push(Vec::new(), Modifiers::NONE);
                    self.phase = Phase::Events { settle: 10 };
                }
                None => self.fail(format!("scroll target not on screen: {target:?}")),
            },
            Step::Drag { from, to, modifiers, steps } => match (self.resolve(app, &from), self.resolve(app, &to)) {
                (Some(start), Some(end)) => {
                    let modifiers = parse_modifiers(&modifiers);
                    let steps = steps.unwrap_or(12).max(2);
                    self.push(vec![Event::PointerMoved(start)], modifiers);
                    self.push(vec![Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers }], modifiers);
                    for index in 1..=steps {
                        let t = index as f32 / steps as f32;
                        self.push(vec![Event::PointerMoved(start + (end - start) * t)], modifiers);
                    }
                    self.push(vec![Event::PointerButton { pos: end, button: PointerButton::Primary, pressed: false, modifiers }], modifiers);
                    self.push(Vec::new(), Modifiers::NONE);
                    self.pointer = end;
                    self.phase = Phase::Events { settle: 4 };
                }
                _ => self.fail(format!("drag target not on screen: {from:?} -> {to:?}")),
            },
            Step::Command(name) => match command_by_name(&name) {
                Some(command) => {
                    app.run(command, ctx);
                    self.phase = Phase::Frames(3);
                }
                None => self.fail(format!("unknown command {name}")),
            },
            Step::Apply(value) => {
                // Another writer: a separate Session handle on the same file, without the editor's revision.
                let session = avs_session::Session::new(app.store.session.paths().clone());
                let operations: Result<Vec<Operation>, _> = if value.is_array() { serde_json::from_value(value.clone()) } else { serde_json::from_value(value.clone()).map(|one| vec![one]) };
                match operations {
                    Ok(operations) => match session.apply_many(&operations, None) {
                        Ok(applied) => {
                            self.pass(format!("external edit applied, rev {}", applied.revision));
                            self.phase = Phase::Sleep { until: now + Duration::from_millis(600) };
                        }
                        Err(error) => self.fail(format!("external edit failed: {}: {}", error.code, error.message)),
                    },
                    Err(error) => self.fail(format!("bad operation: {error}")),
                }
            }
            Step::SelectMedia(name) => match media_id(app, &name) {
                Some(id) => app.selected_media = Some(id),
                None => self.fail(format!("no media {name}")),
            },
            Step::Import { path, track, at } => {
                let place = match (track, at) {
                    (Some(track), Some(at)) => Some((track, Time::from_secs(at).unwrap_or(Time::ZERO))),
                    _ => None,
                };
                app.start_import(PathBuf::from(path), place);
                self.phase = Phase::Until { deadline: now + STEP_TIMEOUT, check: idle };
            }
            Step::Export(path) => {
                app.start_export(PathBuf::from(path));
                self.phase = Phase::Until { deadline: now + Duration::from_secs(120), check: idle };
            }
            Step::ExportStart(path) => {
                app.start_export(PathBuf::from(path));
                self.phase = Phase::Frames(5);
            }
            Step::Library { action, path } => {
                match action.as_str() {
                    "new" => app.create_library(&path),
                    "open" => app.open_library(&path),
                    "delete" => app.begin_delete_library(&path),
                    "restore" => app.begin_restore_path(&path),
                    other => self.fail(format!("unknown library action {other}")),
                }
                self.phase = Phase::Frames(4);
            }
            Step::DropFile { path, target } => match self.resolve(app, &target) {
                Some(pos) => {
                    self.push(vec![Event::PointerMoved(pos)], Modifiers::NONE);
                    let file = egui::DroppedFile { path: Some(PathBuf::from(path)), ..Default::default() };
                    self.queue.push_back((Vec::new(), Modifiers::NONE, vec![file]));
                    self.push(Vec::new(), Modifiers::NONE);
                    self.phase = Phase::Events { settle: 3 };
                }
                None => self.fail(format!("drop target not on screen: {target:?}")),
            },
            Step::WaitIdle => self.phase = Phase::Until { deadline: now + STEP_TIMEOUT, check: idle },
            Step::Screenshot(name) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(name.clone())));
                self.phase = Phase::Screenshot { name, deadline: now + STEP_TIMEOUT, resend: now + Duration::from_secs(4) };
            }
            Step::Dump(name) => {
                let state = dump(app);
                let path = self.shots.join(format!("{name}.json"));
                match std::fs::write(&path, serde_json::to_string_pretty(&state).unwrap_or_default()) {
                    Ok(()) => self.pass(format!("dumped {}", path.display())),
                    Err(error) => self.fail(format!("cannot write {}: {error}", path.display())),
                }
            }
            Step::Theme(theme) => {
                use crate::app_shell_foundation::AppShellThemePreference as T;
                app.prefs.theme = match theme.as_str() {
                    "light" => T::Light,
                    "dark" => T::Dark,
                    _ => T::System,
                };
                app.apply_prefs(ctx);
                self.phase = Phase::Frames(4);
            }
            Step::Language(language) => {
                use crate::app_shell_foundation::AppShellLanguagePreference as L;
                app.prefs.language = match language.as_str() {
                    "ja" => L::Japanese,
                    "en" => L::English,
                    _ => L::System,
                };
                app.apply_prefs(ctx);
                self.phase = Phase::Frames(3);
            }
            Step::FontSize(points) => {
                app.prefs.font_size_points = points;
                app.apply_prefs(ctx);
                self.phase = Phase::Frames(4);
            }
            Step::Expect(expect) => {
                let problems = check(app, &expect);
                if problems.is_empty() {
                    self.pass(format!("expect ok: {expect}"));
                } else {
                    self.fail(format!("expect {expect}: {}", problems.join("; ")));
                }
            }
        }
    }

    /// Called at the end of each frame.
    pub fn after_frame(&mut self, app: &mut AvsApp, ctx: &egui::Context) {
        if self.finished {
            return;
        }
        if self.warmup > 0 {
            self.warmup -= 1;
            return;
        }
        // Screenshots arrive as events some frames after the request.
        let shots: Vec<(String, std::sync::Arc<egui::ColorImage>)> = ctx.input(|input| {
            input
                .raw
                .events
                .iter()
                .filter_map(|event| match event {
                    Event::Screenshot { user_data, image, .. } => user_data.data.as_ref().and_then(|data| data.downcast_ref::<String>()).map(|name| (name.clone(), image.clone())),
                    _ => None,
                })
                .collect()
        });
        for (name, image) in shots {
            let path = self.shots.join(format!("{name}.png"));
            match save_png(&path, &image) {
                Ok(()) => self.pass(format!("screenshot {}", path.display())),
                Err(error) => self.fail(format!("screenshot {name}: {error}")),
            }
            if matches!(&self.phase, Phase::Screenshot { name: wanted, .. } if *wanted == name) {
                self.phase = Phase::Ready;
            }
        }
        let now = Instant::now();
        let ready = match &mut self.phase {
            Phase::Ready => true,
            Phase::Events { settle } => {
                if self.queue.is_empty() {
                    if *settle == 0 {
                        true
                    } else {
                        *settle -= 1;
                        false
                    }
                } else {
                    false
                }
            }
            Phase::Until { deadline, check } => {
                if check(app) {
                    true
                } else if now > *deadline {
                    let message = "timed out waiting for the editor to become idle".to_string();
                    self.phase = Phase::Ready;
                    self.fail(message);
                    true
                } else {
                    false
                }
            }
            Phase::Screenshot { deadline, name, resend } => {
                if now > *resend {
                    // A request made while the window was not being presented can be dropped; ask again.
                    *resend = now + Duration::from_secs(4);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(name.clone())));
                }
                if now > *deadline {
                    let message = format!("screenshot {name} never arrived");
                    self.phase = Phase::Ready;
                    self.fail(message);
                    true
                } else {
                    false
                }
            }
            Phase::Sleep { until } => now >= *until,
            Phase::Frames(count) => {
                if *count == 0 {
                    true
                } else {
                    *count -= 1;
                    false
                }
            }
        };
        if !ready {
            return;
        }
        match self.steps.pop_front() {
            Some(step) => {
                self.index += 1;
                if std::env::var_os("AVS_SELFTEST_VERBOSE").is_some() {
                    eprintln!("step {} {:?} at {:.1}s", self.index, step, self.started.elapsed().as_secs_f64());
                }
                self.start_step(app, ctx, step);
            }
            None => self.finish(app),
        }
    }

    fn finish(&mut self, app: &mut AvsApp) {
        self.finished = true;
        let summary = json!({
            "ok": self.failures == 0,
            "failures": self.failures,
            "steps": self.index,
            "seconds": self.started.elapsed().as_secs_f64(),
            "log": self.log,
            "final": dump(app),
        });
        let _ = std::fs::write(self.shots.join("self-test-result.json"), serde_json::to_string_pretty(&summary).unwrap_or_default());
        EXIT_CODE.store(if self.failures == 0 { 0 } else { 1 }, Ordering::SeqCst);
        eprintln!("SELF-TEST {} ({} step(s), {} failure(s))", if self.failures == 0 { "PASSED" } else { "FAILED" }, self.index, self.failures);
        app.quit_requested = true;
    }
}

fn save_png(path: &std::path::Path, image: &egui::ColorImage) -> Result<(), String> {
    let [width, height] = image.size;
    let file = std::fs::File::create(path).map_err(|error| error.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
    let bytes: Vec<u8> = image.pixels.iter().flat_map(|pixel| pixel.to_srgba_unmultiplied()).collect();
    writer.write_image_data(&bytes).map_err(|error| error.to_string())
}

/// The editor state a script can check.
pub fn dump(app: &AvsApp) -> Value {
    let project = app.store.project();
    json!({
        "revision": app.store.revision(),
        "playhead": app.playhead.secs(),
        "tool": format!("{:?}", app.tool),
        "snap": app.view.snap,
        "selection": app.selection,
        "selectedMedia": app.selected_media,
        "range": app.range().map(|(start, end)| [start.secs(), end.secs()]),
        "playing": app.player.is_playing(),
        "status": app.status.as_ref().map(|status| json!({ "kind": format!("{:?}", status.kind), "text": status.text })),
        "loadError": app.store.load_error.as_ref().map(|error| format!("{}: {}", error.code, error.message)),
        "dataDirectory": app.store.data_directory(),
        "pxPerSec": app.scale.px_per_sec,
        "tracks": project.map(|project| project.tracks.iter().map(|track| json!({
            "id": track.id,
            "kind": track.kind,
            "muted": track.is_muted(),
            "locked": track.is_locked(),
            "clips": track.clips.iter().map(|clip| json!({
                "id": clip.id,
                "start": clip.start_seconds.secs(),
                "end": clip.end().secs(),
                "sourceStart": clip.source_start().secs(),
                "muted": clip.properties.muted,
                "linked": clip.link.as_ref().map(|link| link.group_id.clone()),
                "name": clip.name,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>()),
        "mediaCount": project.map_or(0, |project| project.media.len()),
        "title": project.map(|project| project.title.clone()),
        "frameMsAvg": if app.frame_ms.is_empty() { 0.0 } else { app.frame_ms.iter().sum::<f32>() / app.frame_ms.len() as f32 },
        "frameMsMax": app.frame_ms.iter().copied().fold(0.0f32, f32::max),
        "frames": app.frame_ms.len(),
        "renaming": app.renaming.as_ref().map(|(id, text)| json!([id, text])),
        "keyboardFocus": app.ctx.egui_wants_keyboard_input(),
        "panels": { "timelineHeight": app.view.timeline_height, "libraryWidth": app.view.library_width, "inspectorWidth": app.view.inspector_width, "compactTracks": app.view.compact_tracks },
    })
}

/// Checks `{"revision": 3, "selectedCount": 2, "clip": {"id": "clip-1", "start": 1.0}, ...}`.
fn check(app: &AvsApp, expect: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let Some(map) = expect.as_object() else { return vec!["expect must be an object".into()] };
    let project = app.store.project();
    let near = |a: f64, b: f64| (a - b).abs() <= 1.0 / app.fps().max(1.0) + 1e-6;
    for (key, value) in map {
        match key.as_str() {
            "revision" => {
                if Some(app.store.revision()) != value.as_u64() {
                    problems.push(format!("revision is {}", app.store.revision()));
                }
            }
            "revisionAtLeast" => {
                if app.store.revision() < value.as_u64().unwrap_or(0) {
                    problems.push(format!("revision is {}", app.store.revision()));
                }
            }
            "selectedCount" => {
                if Some(app.selection.len() as u64) != value.as_u64() {
                    problems.push(format!("selection is {:?}", app.selection));
                }
            }
            "selected" => {
                let wanted: Vec<String> = serde_json::from_value(value.clone()).unwrap_or_default();
                if wanted.iter().any(|id| !app.selection.contains(id)) || wanted.len() != app.selection.len() {
                    problems.push(format!("selection is {:?}", app.selection));
                }
            }
            "clipCount" => {
                let count = project.map_or(0, |project| project.clips().count());
                if Some(count as u64) != value.as_u64() {
                    problems.push(format!("clip count is {count}"));
                }
            }
            "trackCount" => {
                let count = project.map_or(0, |project| project.tracks.len());
                if Some(count as u64) != value.as_u64() {
                    problems.push(format!("track count is {count}"));
                }
            }
            "mediaCount" => {
                let count = project.map_or(0, |project| project.media.len());
                if Some(count as u64) != value.as_u64() {
                    problems.push(format!("media count is {count}"));
                }
            }
            "playhead" => {
                if !near(app.playhead.secs(), value.as_f64().unwrap_or(-1.0)) {
                    problems.push(format!("playhead is {}", app.playhead.secs()));
                }
            }
            "playheadAtLeast" => {
                if app.playhead.secs() + 1e-6 < value.as_f64().unwrap_or(0.0) {
                    problems.push(format!("playhead is {}", app.playhead.secs()));
                }
            }
            "playheadAtMost" => {
                if app.playhead.secs() > value.as_f64().unwrap_or(0.0) + 1e-6 {
                    problems.push(format!("playhead is {}", app.playhead.secs()));
                }
            }
            "playing" => {
                if Some(app.player.is_playing()) != value.as_bool() {
                    problems.push(format!("playing is {}", app.player.is_playing()));
                }
            }
            "tool" => {
                if !format!("{:?}", app.tool).eq_ignore_ascii_case(value.as_str().unwrap_or_default()) {
                    problems.push(format!("tool is {:?}", app.tool));
                }
            }
            "snap" => {
                if Some(app.view.snap) != value.as_bool() {
                    problems.push(format!("snap is {}", app.view.snap));
                }
            }
            "range" => {
                let wanted: Option<[f64; 2]> = serde_json::from_value(value.clone()).ok();
                let actual = app.range().map(|(start, end)| [start.secs(), end.secs()]);
                let same = match (wanted, actual) {
                    (Some(a), Some(b)) => near(a[0], b[0]) && near(a[1], b[1]),
                    (None, None) => true,
                    _ => false,
                };
                if !same {
                    problems.push(format!("range is {actual:?}"));
                }
            }
            "statusKind" => {
                let kind = app.status.as_ref().map(|status| format!("{:?}", status.kind)).unwrap_or_default();
                if !kind.eq_ignore_ascii_case(value.as_str().unwrap_or_default()) {
                    problems.push(format!("status is {kind}: {}", app.status.as_ref().map(|s| s.text.clone()).unwrap_or_default()));
                }
            }
            "statusContains" => {
                let text = app.status.as_ref().map(|status| status.text.clone()).unwrap_or_default();
                if !text.contains(value.as_str().unwrap_or_default()) {
                    problems.push(format!("status is {text:?}"));
                }
            }
            "noError" => {
                if app.status.as_ref().is_some_and(|status| status.kind == StatusKind::Error) {
                    problems.push(format!("error: {}", app.status.as_ref().map(|s| s.text.clone()).unwrap_or_default()));
                }
            }
            "clip" => {
                let Some(id) = value.get("id").and_then(Value::as_str) else {
                    problems.push("clip needs id".into());
                    continue;
                };
                let exists = value.get("exists").and_then(Value::as_bool).unwrap_or(true);
                match project.and_then(|project| project.clip(id).ok()) {
                    None if exists => problems.push(format!("clip {id} does not exist")),
                    None => {}
                    Some(_) if !exists => problems.push(format!("clip {id} still exists")),
                    Some(clip) => {
                        for (field, actual) in [("start", clip.start_seconds.secs()), ("end", clip.end().secs()), ("sourceStart", clip.source_start().secs()), ("duration", clip.duration_seconds.secs())] {
                            if let Some(wanted) = value.get(field).and_then(Value::as_f64)
                                && !near(wanted, actual) {
                                    problems.push(format!("clip {id} {field} is {actual}"));
                                }
                        }
                        if let Some(track) = value.get("track").and_then(Value::as_str)
                            && clip.track_id != track {
                                problems.push(format!("clip {id} is on {}", clip.track_id));
                            }
                        if let Some(muted) = value.get("muted").and_then(Value::as_bool)
                            && clip.properties.muted.unwrap_or(false) != muted {
                                problems.push(format!("clip {id} muted is {:?}", clip.properties.muted));
                            }
                        if let Some(props) = value.get("properties").and_then(Value::as_object) {
                            let actual = serde_json::to_value(&clip.properties).unwrap_or_default();
                            for (name, wanted) in props {
                                let got = actual.get(name).cloned().unwrap_or(Value::Null);
                                let equal = match (wanted.as_f64(), got.as_f64()) {
                                    (Some(a), Some(b)) => (a - b).abs() < 1e-3,
                                    _ => *wanted == got || (wanted.is_object() && got.is_object() && wanted.as_object().is_some_and(|w| w.iter().all(|(k, v)| got.get(k).is_some_and(|g| g.as_f64().zip(v.as_f64()).map_or(g == v, |(g, v)| (g - v).abs() < 1e-3))))),
                                };
                                if !equal {
                                    problems.push(format!("clip {id} {name} is {got}"));
                                }
                            }
                        }
                        if let Some(text) = value.get("caption").and_then(Value::as_str)
                            && clip.caption.as_ref().map(|caption| caption.text.as_str()) != Some(text) {
                                problems.push(format!("clip {id} caption is {:?}", clip.caption.as_ref().map(|c| c.text.clone())));
                            }
                    }
                }
            }
            "dataDirectoryEndsWith" => {
                let directory = app.store.data_directory().display().to_string();
                if !directory.ends_with(value.as_str().unwrap_or_default()) {
                    problems.push(format!("library is {directory}"));
                }
            }
            "pathExists" => {
                let path = value.as_str().unwrap_or_default();
                if !std::path::Path::new(path).exists() {
                    problems.push(format!("{path} does not exist"));
                }
            }
            "pathMissing" => {
                let path = value.as_str().unwrap_or_default();
                if std::path::Path::new(path).exists() {
                    problems.push(format!("{path} exists"));
                }
            }
            "popupOpen" => {
                let open = egui::Popup::is_any_open(&app.ctx);
                if Some(open) != value.as_bool() {
                    problems.push(format!("popup open is {open}"));
                }
            }
            "track" => {
                let Some(id) = value.get("id").and_then(Value::as_str) else {
                    problems.push("track needs id".into());
                    continue;
                };
                match project.and_then(|project| project.track(id).ok()) {
                    None => problems.push(format!("track {id} does not exist")),
                    Some(track) => {
                        if let Some(muted) = value.get("muted").and_then(Value::as_bool)
                            && track.is_muted() != muted {
                                problems.push(format!("track {id} muted is {}", track.is_muted()));
                            }
                        if let Some(index) = value.get("index").and_then(Value::as_u64) {
                            let actual = project.and_then(|project| project.track_index(id).ok()).unwrap_or(usize::MAX);
                            if actual as u64 != index {
                                problems.push(format!("track {id} is at index {actual}"));
                            }
                        }
                        if let Some(count) = value.get("clips").and_then(Value::as_u64)
                            && track.clips.len() as u64 != count {
                                problems.push(format!("track {id} has {} clips", track.clips.len()));
                            }
                    }
                }
            }
            "exportState" => {
                let state = match app.export.as_ref().map(|task| &task.state) {
                    None => "none".to_string(),
                    Some(crate::export_job::ExportState::Ready) => "ready".into(),
                    Some(crate::export_job::ExportState::Running) => "running".into(),
                    Some(crate::export_job::ExportState::Done(Ok(_))) => "done".into(),
                    Some(crate::export_job::ExportState::Done(Err(error))) => format!("failed: {error}"),
                };
                if !state.starts_with(value.as_str().unwrap_or_default()) {
                    problems.push(format!("export is {state}"));
                }
            }
            "panelAtLeast" | "panelAtMost" => {
                let at_least = key == "panelAtLeast";
                let sizes = [("timeline", app.view.timeline_height), ("library", app.view.library_width), ("inspector", app.view.inspector_width)];
                for (name, size) in sizes {
                    if let Some(limit) = value.get(name).and_then(Value::as_f64) {
                        let size = f64::from(size.unwrap_or(0.0));
                        if (at_least && size + 0.5 < limit) || (!at_least && size > limit + 0.5) {
                            problems.push(format!("{name} is {size}"));
                        }
                    }
                }
            }
            "fileExists" => {
                let path = value.as_str().unwrap_or_default();
                if !std::path::Path::new(path).is_file() {
                    problems.push(format!("{path} does not exist"));
                }
            }
            other => problems.push(format!("unknown expectation {other}")),
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_parse() {
        let (key, mods) = parse_key("cmd+shift+z").unwrap();
        assert_eq!(key, Key::Z);
        assert!(mods.command && mods.shift && !mods.alt);
        assert_eq!(parse_key("alt+[").unwrap().0, Key::OpenBracket);
        assert_eq!(parse_key("space").unwrap().0, Key::Space);
        assert_eq!(parse_key("b").unwrap().0, Key::B);
    }

    #[test]
    fn commands_by_name() {
        assert_eq!(command_by_name("BladeAtPlayhead"), Some(Command::BladeAtPlayhead));
        assert_eq!(command_by_name("Nope"), None);
    }

    #[test]
    fn scripts_deserialize() {
        let steps: Vec<Step> = serde_json::from_str(
            r#"[{"wait": 100}, {"key": "cmd+b"}, {"click": {"target": {"clip": "clip-1", "at": "start"}, "modifiers": "cmd"}},
                {"drag": {"from": {"clip": "clip-1"}, "to": {"time": 3.0, "track": "V1"}}}, {"screenshot": "a"}, {"expect": {"revision": 2}},
                {"apply": {"tool": "split_clip", "args": {"clipId": "clip-1", "atSeconds": 1.0}}}, "waitIdle"]"#,
        )
        .unwrap();
        assert_eq!(steps.len(), 8);
    }
}
