//! Agent Video Studio editor (eframe/egui).
//!
//! The window holds a copy of the project from `avs-session`; every edit is an
//! `avs_core::Operation` sent through the session, which also watches for edits
//! by other writers (the MCP server). Rendering is `avs-render`.
//!
//! Launch:
//! - `agent-video-studio`: the library the settings point at (the user's data).
//! - `agent-video-studio --library <dir>`: an isolated run. Settings, the recent
//!   list, window state and the default library all live under `<dir>`; nothing
//!   in the home folder is read or written.
//! - `... --self-test <script.json> --shots <dir>`: run a scripted session and exit.

#[allow(dead_code)]
pub mod app_shell_foundation;

pub mod app;
pub mod edit;
pub mod export_job;
pub mod geom;
pub mod i18n;
pub mod import_job;
pub mod keys;
pub mod media_cache;
pub mod menu_model;
pub mod native_menu;
pub mod player;
pub mod recorder;
pub mod selftest;
pub mod store;
pub mod theme;
pub mod ui_inspector;
pub mod ui_library;
pub mod ui_menu;
pub mod ui_timeline;
pub mod ui_viewer;
pub mod workspace_ui;

use avs_session::Env;
use eframe::egui;
use std::path::{Path, PathBuf};

pub const SANDBOX_DIR: &str = ".avs-sandbox";

/// An environment confined to `root`: the home folder (and so the settings, the
/// recent list and the default library) is `<root>/.avs-sandbox/home`. ffmpeg and
/// ffprobe are still found the usual way.
///
/// The library is `root` itself when it already holds a session or is an
/// app-owned folder (`…/Agent Video Studio/Project Library`); otherwise the
/// app-owned folder inside `root`. A library that holds a session but is not
/// app-owned is opened exactly (`AVS_PROJECT_LIBRARY_ROOT` style), and then
/// switching libraries is disabled.
pub fn sandbox_env(root: &Path) -> Result<Env, String> {
    std::fs::create_dir_all(root).map_err(|error| format!("cannot create {}: {error}", root.display()))?;
    let root = root.canonicalize().map_err(|error| format!("cannot resolve {}: {error}", root.display()))?;
    let process = Env::from_process();
    let home = root.join(SANDBOX_DIR).join("home");
    std::fs::create_dir_all(&home).map_err(|error| format!("cannot create {}: {error}", home.display()))?;
    let mut env = Env {
        home: home.clone(),
        settings_path: Some(root.join(SANDBOX_DIR).join("config").join(avs_session::env::SETTINGS_FILE_NAME)),
        library_root: None,
        session_path: None,
        ffprobe_path: process.ffprobe_path,
        ffmpeg_path: process.ffmpeg_path,
        path_dirs: process.path_dirs,
    };
    let holds_session = root.join(avs_session::env::SESSION_FILE_NAME).is_file();
    if holds_session && !avs_session::env::is_app_owned(&root) {
        env.library_root = Some(root.display().to_string());
        return Ok(env);
    }
    // A copied sandbox still names the original folder; point it back inside `root`.
    let inside = avs_session::get_settings(&env).is_ok_and(|settings| env.settings_file().is_file() && settings.data_directory.starts_with(&root));
    if !inside {
        avs_session::set_settings(&env, &root.display().to_string(), None).map_err(|error| format!("{}: {}", error.code, error.message))?;
    }
    Ok(env)
}

struct Args {
    library: Option<PathBuf>,
    self_test: Option<PathBuf>,
    shots: Option<PathBuf>,
    no_audio: bool,
    with_audio: bool,
    size: Option<(f32, f32)>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { library: None, self_test: None, shots: None, no_audio: false, with_audio: false, size: None };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| iter.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--library" => args.library = Some(PathBuf::from(value("--library")?)),
            "--self-test" => args.self_test = Some(PathBuf::from(value("--self-test")?)),
            "--shots" => args.shots = Some(PathBuf::from(value("--shots")?)),
            "--no-audio" => args.no_audio = true,
            // Self-tests run without sound unless asked (mute the tracks in the script to keep it silent).
            "--with-audio" => args.with_audio = true,
            "--size" => {
                let text = value("--size")?;
                let (w, h) = text.split_once('x').ok_or("--size is WIDTHxHEIGHT")?;
                args.size = Some((w.parse().map_err(|_| "bad --size width")?, h.parse().map_err(|_| "bad --size height")?));
            }
            "--help" | "-h" => {
                println!("agent-video-studio [--library <dir>] [--self-test <script.json> --shots <dir>] [--no-audio] [--with-audio] [--size WxH]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if args.self_test.is_some() && args.library.is_none() {
        return Err("--self-test needs --library <dir> (a self-test never touches the real library)".into());
    }
    Ok(args)
}

/// Entry point of the `agent-video-studio` binary. Returns the process exit code.
pub fn run() -> i32 {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("agent-video-studio: {message}");
            return 2;
        }
    };
    let (env, persistence, note) = match &args.library {
        Some(root) => match sandbox_env(root) {
            Ok(env) => (env, Some(root.join(SANDBOX_DIR).join("ui-state")), Some(format!("sandbox: {}", root.display()))),
            Err(message) => {
                eprintln!("agent-video-studio: {message}");
                return 2;
            }
        },
        None => (Env::from_process(), None, None),
    };
    let selftest = match (&args.self_test, &args.shots) {
        (Some(script), shots) => {
            let shots = shots.clone().unwrap_or_else(|| script.with_extension("shots"));
            let root = args.library.clone().unwrap_or_default();
            match selftest::SelfTest::load(script, shots, &root) {
                Ok(test) => Some(test),
                Err(message) => {
                    eprintln!("agent-video-studio: {message}");
                    return 2;
                }
            }
        }
        (None, _) => None,
    };
    let testing = selftest.is_some();
    let store = store::Store::open(env);
    let size = args.size.unwrap_or(if testing { (1600.0, 1000.0) } else { (1480.0, 920.0) });
    // An empty icon keeps the app bundle's own icon (macos/AppIcon.icns) in the Dock;
    // without it eframe replaces that with its built-in "e".
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Agent Video Studio")
        .with_icon(egui::IconData::default())
        .with_inner_size([size.0, size.1])
        .with_min_inner_size([1000.0, 640.0])
        .with_drag_and_drop(true);
    if testing {
        // A covered window is not repainted on macOS, and the self-test needs painted frames.
        viewport = viewport.with_resizable(false).with_window_level(egui::WindowLevel::AlwaysOnTop).with_position([40.0, 40.0]);
    }
    let options = eframe::NativeOptions { viewport, persistence_path: persistence, persist_window: !testing, ..Default::default() };
    let app_options = app::Options { audio: !args.no_audio && (!testing || args.with_audio), selftest, sandbox_note: note };
    let result = eframe::run_native("Agent Video Studio", options, Box::new(move |cc| Ok(Box::new(app::AvsApp::new(cc, store, app_options)))));
    if let Err(error) = result {
        eprintln!("agent-video-studio: {error}");
        return 1;
    }
    selftest::EXIT_CODE.load(std::sync::atomic::Ordering::SeqCst)
}
