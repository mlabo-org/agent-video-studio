English | [日本語](README.ja.md)

# Agent Video Studio

Agent Video Studio (AVS) is a local video editor built for two operators at once: a person using a native macOS editing app, and an agent (Codex, Claude Code, or another plugin) using MCP tools. Both work on the same project session, so an agent's edit appears in the open editor within a moment and the person's edits are what the agent's next call sees.

It is written in Rust. There is no Node, npm, browser, or dev server.

## What you run

| Artifact | Installed at | What it is |
|---|---|---|
| Editor | `~/Applications/Agent Video Studio.app` | The editing app (eframe/egui). Open it with `open -a "Agent Video Studio"`. |
| MCP server | `~/.local/bin/agent-video-studio-mcp` | The agent tool surface: 69 tools over stdio. `.mcp.json` starts it for both hosts. |

AVS needs `ffmpeg` and `ffprobe` on `PATH` (or `FFMPEG_PATH` / `FFPROBE_PATH`). It does not need an ffmpeg built with `drawtext`: captions are drawn by AVS itself.

## Plugin Source (Codex and Claude Code)

This repository is the single source for the `agent-video-studio` plugin on both hosts.

- `.codex-plugin/plugin.json`: Codex plugin metadata.
- `.claude-plugin/marketplace.json`: makes this repository its own Claude Code marketplace (entry source `./`). There is no `.claude-plugin/plugin.json`.
- `.mcp.json`: one launch definition for both hosts, `$HOME/.local/bin/agent-video-studio-mcp`.
- `skills/agent-video-studio/SKILL.md`: when and how an agent uses AVS.
- `local-capability-manifest.json`, `schemas/v1/leaves/`: the capability descriptors and per-tool JSON schemas other plugins and the local capability broker read. Generated; see Development.

### Install

You need macOS 14 or later, a Rust toolchain (`cargo`), the Xcode command line tools, and `ffmpeg` (for example `brew install ffmpeg`). Clone the repository anywhere; nothing depends on its location.

```bash
AVS_SIGN_IDENTITY="<code-signing identity>" scripts/install.sh   # with a certificate
AVS_SIGN_IDENTITY=- scripts/install.sh                           # without one (ad-hoc signature)
```

`AVS_SIGN_IDENTITY` is the name of a code-signing certificate in your keychain (`security find-identity -v -p codesigning` lists them). A self-signed certificate created in Keychain Access (Certificate Assistant, type "Code Signing") is enough for your own machine. With a certificate, macOS keeps the app's microphone and folder grants across rebuilds. `-` signs ad hoc instead: everything works, but macOS asks for those grants again after every install. The script never chooses ad hoc by itself.

The script builds a release in a temporary directory, puts `macos/AppIcon.icns` in the bundle, signs the app and the MCP server, and renames the new files into place: `~/Applications/Agent Video Studio.app` and `~/.local/bin/agent-video-studio-mcp` (`AVS_APP_DIR` and `AVS_BIN_DIR` change those folders; `.mcp.json` expects the server in `~/.local/bin`). A source change is not live until this has run.

The editor alone needs nothing more: `open -a "Agent Video Studio"`. To give an agent the tools, add the plugin to its host:

- Claude Code: `claude plugin marketplace add <path to this clone>` (or the GitHub repository), then `claude plugin install agent-video-studio@agent-video-studio`. From a local clone the plugin loads in place; new sessions and `/reload-plugins` pick up metadata changes. A marketplace added from GitHub is updated with `claude plugin marketplace update agent-video-studio` and `claude plugin update agent-video-studio@agent-video-studio`. The binaries always come from `scripts/install.sh`.
- Codex: install the plugin from this checkout through your Codex plugin marketplace. Codex runs a cached copy of the plugin's metadata, so refresh that copy after a metadata change.
- Any other MCP client: start `~/.local/bin/agent-video-studio-mcp` as a stdio server.

An MCP server process that is already running keeps the old binary until its session reloads.

## Workspace

| Crate | Role |
|---|---|
| `crates/avs-core` | The timeline document and every edit. No I/O. `ops::Operation` is the only form an edit takes; `ops::apply` runs one on a copy and rejects a result that breaks an invariant. Also read-only queries, `avs.auto-edit-plan.v1`, and media placement. |
| `crates/avs-session` | Everything on disk: the project session file (revisions, undo/redo, activity log), library settings, workspaces, media import, export job state. |
| `crates/avs-render` | One ffmpeg composition graph used by the export and by the live preview, so the picture you preview is the picture you export. |
| `crates/avs-handoff` | Import of a CodexVideo `avs.codexvideo-handoff.v1` manifest. |
| `crates/avs-mcp` | The MCP server. `tools.json` is the tool catalog. |
| `crates/avs-app` | The editor app. |

### One path for every edit

The editor, the MCP tools, auto-edit plans, and the handoff import all build `Operation` values and hand them to `Session::apply` / `apply_many`, which takes the library lock, checks the caller's revision, runs `avs_core::apply`, pushes an undo snapshot, and renames a temp file into place. There is no API that saves a whole project, so one operator cannot overwrite another's edit. A caller whose revision is stale is refused and changes nothing; the editor reloads and re-applies its operation.

Design rules that hold everywhere:

- Time is integer microseconds (`Time`); JSON carries seconds. There is no epsilon comparison.
- `apply` validates its result. Overlap, media range, fade length, speed, volume, crop, and link consistency are checked in one place and reported by code (`CLIP_OVERLAP`, `MEDIA_RANGE_EXCEEDED`, ...).
- A locked track refuses every clip edit (`TRACK_LOCKED`).
- A video clip and the clip carrying its sound are a linked pair and are moved, trimmed, split, deleted, and re-timed together. Cutting one half only unlinks the pieces.
- A split continues at `sourceStart + offset x speed` and removes the fades at the cut; a trim shrinks fades that no longer fit.
- `overwrite` clears exactly the range each placed clip covers; `insert` opens room on that track.

## Project data

- Settings: `~/Library/Application Support/Agent Video Studio/project-library-settings.json` names the active Project Library. `AVS_PROJECT_LIBRARY_ROOT` overrides it for one process; `AVS_PROJECT_SESSION_PATH` selects a single session file.
- A Project Library holds `project-session.json`, `media/` (imported files; clips refer to them as `/project-library/media/...`), `exports/`, `previews/`, and hidden state (`.project-session.lock`, `.export-jobs/`, `.cache/`).
- The session file is envelope version 2: a library id, the revision, the project, up to 30 undo snapshots, and an activity log of up to 200 entries. Version 1 files written by the earlier TypeScript implementation load as they are and become version 2 on their first write. An invalid undo snapshot is dropped with a warning; only an invalid current project stops a load.
- Deleting a workspace renames it to a `.deleted` sibling and can be restored.

## The editor

One window with five regions: media library, preview, multi-track timeline, inspector, and agent activity (the session's activity log and current revision, with external edits highlighted). The menus (File, Edit, Mark, Media, Tools, View, Playback, Window, Help) are in the macOS menu bar, not in the window. Theme, language (Japanese/English), and font size are in the settings window (App Shell Foundation), under the application menu.

Keys and edit behaviour follow Final Cut Pro conventions. The full list is in the app under Help. While a text field is being edited the keys belong to the field: the menus then show no shortcuts.

| | Keys |
|---|---|
| Playback | Space, J / K / L, Left/Right one frame, Shift+Left/Right ten frames, Up/Down previous/next edit point, Home/End |
| Tools | A select, T trim, B blade, R range, N snapping |
| Editing | Cmd+Z / Shift+Cmd+Z, Cmd+B split at the playhead, Delete ripple-delete, Shift+Delete or Forward Delete delete leaving a gap, Option+[ / Option+] trim start/end to the playhead, V mute, Cmd+X/C/V, Cmd+D duplicate, Cmd+A |
| Range | I / O, X range of the clip under the playhead, Option+X clear |
| Media to timeline | E append, W insert, D overwrite, Q place on another track, Option+C add caption |
| View | Cmd+Plus / Cmd+Minus, Shift+Z fit, pinch |
| App | Cmd+I import, Cmd+E export, Shift+Option+A record narration, Cmd+, settings |

Differences from Final Cut Pro, chosen because the timeline is track-based rather than magnetic: dragging a clip edge with the select tool is a plain trim (use Option or the trim tool to ripple); Option-dragging a clip body slides it; V mutes sound only (hide a picture with the track's M); a range spans all tracks.

Drags and sliders preview on a copy and commit once on release, so one undo reverts one gesture. A failed edit shows its error code in the status bar. Paste leaves the playhead after the pasted clips, and the timeline scrolls to the playhead whenever a command moves it out of view.

The editor shows the active library: when an agent switches it (`create_project_session`, `switch_project_library`), the open window follows and says so in the status bar. A library fixed by `--library` or an environment variable is not switched.

Narration recording takes one input channel of the default input device and writes a mono file (rates above 48 kHz are reduced), so an audio interface with many inputs works: the recording window shows a meter per input and remembers the chosen channel.

Start the editor against a throwaway library with `--library <folder>`; it then keeps its settings inside that folder and does not read the real ones.

## The MCP tools

Every tool answers `{ok: true, tool, ...}` or, with `isError: true`, `{ok: false, tool, error: {code, message, details}}`. Mutations return the new `revision`, the outcomes, and a validation summary; read the project with `get_timeline`.

- Read: `get_timeline`, `list_tracks`, `get_media`, `search_media`, `inspect_timeline`, `validate_timeline`, `collect_clips_in_range`, `get_project_session_status`.
- Timeline edits (each is one `Operation` and one undoable revision; `expectedRevision` is optional): `add_track`, `reorder_track`, `delete_track`, `set_track_muted`, `set_track_properties`, `register_media`, `delete_media`, `add_clips`, `move_clip`, `move_clips`, `trim_clip`, `ripple_trim_clip`, `roll_edit`, `slip_clip`, `slide_clip`, `trim_clip_to_time`, `trim_clips_to_time`, `delete_left_at_time`, `delete_right_at_time`, `delete_clips_left_at_time`, `delete_clips_right_at_time`, `split_clip`, `blade_at_time`, `delete_clip`, `delete_clips`, `ripple_delete_clip`, `close_gap_at_time`, `lift_range`, `extract_range`, `set_clip_properties`, `set_clip_audio`, `set_caption_text`, `add_captions`, `set_project_settings`.
- `undo_project`, `redo_project`.
- Media: `import_media_file` (copy an absolute local file into the library and register it; duration, streams, and geometry come from `ffprobe`, with a timeout) and `import_media_file_to_timeline` (the same plus placement in one revision; a video with sound gets a linked audio clip on a free audio track, or a new one).
- Auto-edit plans: `plan_silence_removal`, `inspect_auto_edit_plan`, `apply_auto_edit_plan`.
- CodexVideo: `import_handoff_manifest`, `import_codexvideo_job` (alias).
- Render: `render_preview`, `render_final_export`, `start_render_final_export`, `get_render_final_export_status`, `cancel_render_final_export`.
- Project library: `get_project_library_settings`, `set_project_library_settings`, `reset_project_library_settings`, `get_project_io_status`, `list_recent_project_workspaces`, `forget_recent_project_workspace`, `inspect_project_library_target`, `create_project_session`, `switch_project_library`, `inspect_project_workspace_delete`, `delete_project_workspace`, `inspect_project_workspace_restore`, `restore_project_workspace`.

`delete_clips`, `lift_range`, `extract_range`, `delete_clips_left_at_time`, `delete_clips_right_at_time`, and `apply_auto_edit_plan` require `confirm: true`. `delete_project_workspace` and `restore_project_workspace` require the `confirmDataDirectory` returned by their inspect tools.

### Auto-edit plans

`avs.auto-edit-plan.v1` lets another agent or plugin prepare edits for AVS to apply:

- a plan carries `planId`, `producer`, `target: {projectId, baseRevision}`, and `operations`
- an operation is `{operationId, type, ...arguments}`, where `type` is any timeline edit tool name and the arguments are that tool's
- operations run in order and each sees the timeline as the previous one left it; `plan_silence_removal` emits its cuts last-to-first so the detected silence coordinates stay valid
- `inspect_auto_edit_plan` dry-runs the plan and returns a `planHash`; `apply_auto_edit_plan` applies exactly that plan (`expectedPlanHash`, `expectedRevision`, `confirm: true`) as one revision and one undo step, or changes nothing

AVS does not detect silence, transcribe, synthesize speech, or call providers; upstream capabilities do that and pass results in.

### CodexVideo handoff

`import_handoff_manifest` reads an `avs.codexvideo-handoff.v1` manifest, copies its assets into the library, and creates tracks, clips, captions, and project settings as one revision and one undo step. If anything fails, nothing is saved and the copies are removed. It returns the mapping from manifest ids to AVS media, track, clip, and caption ids. It records `job.jobRoot` only when the manifest file is inside that folder, because the job root becomes a folder renders may be written to. A clip that overruns its file by under 100 ms (a manifest duration slightly longer than the file) is cut to the file with a warning.

### Rendering

- The export and the preview use the same graph. Tracks earlier in the list are drawn on top; captions are on top of all video. Sound comes from clips on audio tracks.
- Output is an `.mp4` under the active library's `exports/` or `previews/`, or an imported CodexVideo job's `avs/exports/` or `avs/previews/`. The editor's Export uses a normal save dialog and may write anywhere you choose.
- An export writes to a `.partial` file beside the output and renames it on success. A failed or cancelled export leaves an existing output untouched.
- `start_render_final_export` runs in the background, one per library. Its state is a file in the library, so status and cancel work from any AVS process and from the editor.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --no-deps --all-targets -- -D warnings
cargo build -p avs-mcp && target/debug/agent-video-studio-mcp --write-contract .   # regenerate the manifest and schemas
crates/avs-app/selftest/run.sh edit1            # drive the built editor, save screenshots
scripts/make-icon.sh                            # rebuild macos/AppIcon.icns after editing macos/AppIcon.svg
scripts/third-party-licenses.sh                 # rewrite THIRD_PARTY_LICENSES.md after changing dependencies
```

- Tests never touch real libraries: they build an `avs_session::Env` on a temp directory. The render, import, and MCP tests generate small media with ffmpeg's `lavfi`.
- `local-capability-manifest.json` and `schemas/v1/leaves/` are generated from `crates/avs-mcp/tools.json` (names, descriptions, input schemas) and `contract.json` (authority requests, artifact ports). Both are compiled into the server, so regenerate with a binary built from the changed source, not with the installed one. A test fails when the committed files drift. Another test checks every property of a published edit schema against the Rust type that runs it.
- The editor self-test (`--self-test <script.json> --shots <folder>`) injects keys and pointer events, applies operations, checks state, and saves screenshots of the real window. It needs the display awake and unlocked, and `run.sh` needs GNU `timeout`. Build first: `cargo build -p avs-app --bins --examples --target-dir target`.
- A real CodexVideo job can be imported into a temp library with `AVS_REAL_HANDOFF_MANIFEST=<path> cargo test -p avs-handoff --test real_manifest -- --ignored --nocapture`.

## Current limitations

- macOS only in practice: the editor's fonts, the app bundle, and the install script assume it.
- `sequence` media is not rendered; such a clip is skipped with a `SEQUENCE_MEDIA_UNSUPPORTED` warning.
- Captions: characters missing from the caption font (for example emoji) are left blank with a warning. Colours are hex or basic names.
- Reverse playback (J) steps frame by frame, and fast-forward plays without sound.
- A property cannot be cleared back to "unset" (for example to return a clip to automatic fit).
- Undoing an import removes the media from the project but leaves the copied file in the library.
- Render outputs are written to the project library, outside this package, so the local capability broker's package-relative artifact roots cannot describe them.
- The self-test injects keys inside the editor, so it does not cover the macOS menu bar, the system file panels, input methods, or drops from Finder. Those were checked by hand with a real mouse and keyboard in November 2026.

## License

MIT; see `LICENSE`. The crates the editor and the MCP server are built from, and the fonts embedded in the editor, are listed in `THIRD_PARTY_LICENSES.md`. `ffmpeg` and `ffprobe` are not included.

`AGENTS.md` and `CLAUDE.md` are the rules for coding agents working on this repository, and `creator-contract.json` is its plugin-authoring contract; none of them is needed to build or use AVS.

## History

Until November 2026 AVS was a TypeScript project: a React editor that ran only under the Vite dev server, an MCP server started with `node --import tsx`, and an ffmpeg renderer separate from the browser preview. It was rebuilt in Rust to remove a class of defects found in an audit: the editor saved whole projects and could overwrite agent edits; a split ignored clip speed; a multi-clip overwrite removed unrelated clips; mixing N audio tracks lowered each to 1/N; exports dropped captions when ffmpeg lacked `drawtext`; a cancelled export could delete an earlier output; and the capability schemas had no generator. The tool names and input schemas were kept, with these changes: `migrate_legacy_project_session` was removed, `set_project_settings` and `set_track_properties` were added, plan operations may be any edit, results use the `{ok, tool, ...}` shape, and renders go to the project library instead of the plugin folder. The TypeScript source is not part of this repository's public history, which starts at the Rust version.
