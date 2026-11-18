# Agent Video Studio Local Constitution

This file is the local `AGENTS.md` of the Agent Video Studio repository.
It is the scoped SSOT for host agent (Codex or Claude Code) execution conditions inside this Agent Video Studio plugin source repository. Claude Code reaches it through `CLAUDE.md`, which only points here.
Treat it as an operational contract, not as optional guidance.
If it conflicts with system instructions, developer instructions, explicit user requests, the host's global rules, an `AGENTS.md` in a parent directory of this checkout, or a more local `AGENTS.md`, follow the host's priority order. This file specializes ordinary rules for this plugin and never weakens a rule a higher file names as fixed.

## Scope

- This repository is the single source of truth for the `agent-video-studio` plugin, shared by Codex and Claude Code. It works from wherever it is cloned; nothing in it depends on the checkout's location.
- Before substantive plugin work, read the host's global rules (Codex: `~/.codex/AGENTS.md`; Claude Code: `~/.claude/AGENTS.md` or `~/.claude/CLAUDE.md`) and any `AGENTS.md` in a parent directory of this checkout, when they exist and are not already in context.
- The project goal is an agent-native video editor: a native editing app a person operates directly, plus a structured tool surface that agents operate directly, both working on the same project session.
- Keep the timeline schema, media IDs, clip IDs, tool contracts, and verification loops as first-class product surfaces.
- The implementation is Rust: a Cargo workspace under `crates/`. There is no Node, npm, or browser runtime in this repository.

## Plugin Boundaries

- Treat `.codex-plugin/plugin.json`, `.claude-plugin/marketplace.json`, `.mcp.json`, and `skills/agent-video-studio/SKILL.md` as the plugin routing and activation source. Codex reads `.codex-plugin/plugin.json`. Claude Code installs the plugin from this repository's own marketplace (`.claude-plugin/marketplace.json`, entry source `./`) and takes the plugin name from that entry; there is no `.claude-plugin/plugin.json`.
- Treat plugin caches as generated runtime copies and never edit them as the primary source. In Codex: `~/.codex/plugins/cache/`; in Claude Code: `~/.claude/plugins/cache/`.
- Both hosts run the same two installed artifacts, built from this source by `scripts/install.sh`:
  - `~/.local/bin/agent-video-studio-mcp`: the MCP server. `.mcp.json` starts it with `/bin/sh -c 'exec "$HOME/.local/bin/agent-video-studio-mcp"'` and is one launch definition for both hosts. Do not point `.mcp.json` at this checkout, at a Cargo `target/` directory, or at `cargo run` (`crates/avs-mcp/tests/plugin.rs` enforces this).
  - `~/Applications/Agent Video Studio.app`: the editor.
- A Rust source change is not live until `scripts/install.sh` has run. Running it replaces the installed binaries and is an install step: run it when the task is to hand over a working change or the user asks for it, not otherwise, and report whether it ran. `AVS_SIGN_IDENTITY` must name a code-signing certificate in the keychain, because macOS keeps the app's privacy grants (microphone, folders) by signature and an ad-hoc signature changes with every build. The value `-` asks for an ad-hoc signature explicitly, for a machine without a certificate; the script never falls back to one on its own.
- Plugin metadata (`.codex-plugin/plugin.json`, `.claude-plugin/marketplace.json`, `.mcp.json`, skills) does not go through `scripts/install.sh`. When a metadata change must be visible to a host, use that host's route: in Codex, refresh the installed copy of the plugin (Codex runs a cached copy, so reinstall or refresh it the way that setup does); in Claude Code, the plugin loads in place from this checkout, so the change takes effect at the next session start or `/reload-plugins`. After an install, an MCP server process that is already running keeps the old binary until its host session restarts or reloads it.

## Source Boundaries

- `crates/avs-core`: the timeline document and every edit, with no I/O. `ops::Operation` is the only form an edit takes and `ops::apply` is the only function that performs one. It validates its result, so no caller can produce an invalid project. Add or change editing behaviour here, never in a caller.
- `crates/avs-session`: everything on disk (project session file, library settings, workspaces, media import, export job state). A project reaches disk only through `Session::apply`, `apply_many`, `undo`, and `redo`. Do not add an API that saves a whole project.
- `crates/avs-render`: one ffmpeg composition graph used by both the export and the live preview. Do not add a second way to draw the timeline.
- `crates/avs-handoff`: the CodexVideo handoff import, expressed as operations applied in one `apply_many`.
- `crates/avs-mcp`: the agent tool surface. A tool is a thin adapter over the crates above; it must not hold a parallel model or state between calls. `tools.json` is the tool catalog (`tools/list` serves it) and `contract.json` holds authority requests and artifact ports.
- `crates/avs-app`: the editor. It holds a copy of the project, sends each edit as operations with the revision it saw, and reloads when the session file changes. UI code must not reimplement an edit; pure calculations (time to pixels, snapping, drag to operation, key to command) live in modules without egui so they can be tested.
- `local-capability-manifest.json` and `schemas/v1/leaves/` are generated. Change `crates/avs-mcp/tools.json` or `contract.json`, then run `agent-video-studio-mcp --write-contract .` from the repository root with a binary built from the changed sources: both files are compiled into the binary, so the installed `~/.local/bin` copy writes the old contract until `scripts/install.sh` runs. Do not edit the generated files by hand. A test (`crates/avs-mcp/tests/stdio.rs`) fails when they drift.
- User media, project libraries, renders, caches, and logs never live in this repository. Renders go to the active project library (`exports/`, `previews/`) or an imported CodexVideo job's `avs/` folder. Cargo build output (`target/`) is ignored and disposable.

## Agent Tool Safety

- Agent tools must use stable identifiers for media, tracks, and clips.
- Tool responses must return structured data that an agent can inspect and reuse: `{ok: true, tool, ...}` on success, `{ok: false, tool, error: {code, message, details}}` on failure.
- Every timeline edit is one undoable project-session revision. The tools in `CONFIRMED_TOOLS` (`crates/avs-mcp/src/server.rs`) require `confirm: true`: currently `delete_clips`, `lift_range`, `extract_range`, `delete_clips_left_at_time`, `delete_clips_right_at_time` (tools that remove material from several clips at once) and `apply_auto_edit_plan`. When that list changes, update the matching tool descriptions in `tools.json` in the same change.
- Destructive or irreversible operations must require an explicit user decision before implementation. A timeline edit is not in that class, because the session's undo reverts it. What is: anything that removes or replaces data the session cannot bring back, such as deleting a project workspace (which is why it renames to `.deleted` and takes `confirmDataDirectory`). Such a tool must also take an explicit confirmation argument at run time.
- Render tools write only into AVS-owned folders (the active library's `exports/` and `previews/`, and a recorded CodexVideo job root's `avs/exports/` and `avs/previews/`) and may replace an earlier render there; the replacement happens only after the new file is complete. Do not widen those folders, and do not let a tool write a render anywhere else.
- External generation, paid provider calls, network uploads, or API-key-dependent media generation are out of scope unless the user explicitly requests them in the active task.
- Tests, self-tests, and manual runs must not touch the user's real libraries or `~/Library/Application Support/Agent Video Studio`. Build an `avs_session::Env` on a temporary directory, or start the editor with `--library <temp dir>`.

## UI And Verification

- The app is an editor surface, not a landing page. The first window is the usable workspace.
- UI changes must preserve the media library, preview, timeline, inspector, and agent activity panel as the core workspace anatomy unless the user changes the product direction.
- The operation model follows Final Cut Pro conventions for keys and edit behaviour (the user edits with it); do not copy its appearance. A new command needs a key or menu entry, a tooltip, and a line in the in-app key list.
- Theme, language, font size, and fonts belong to App Shell Foundation (`crates/avs-app/src/app_shell_foundation.rs`, a managed file generated from the App Shell Foundation eframe/egui template and recorded in `crates/avs-app/.app-shell-foundation/state.json`; change it by applying a newer template, not by hand). Do not add a second authority for them.
- For code changes, run `cargo test --workspace` and `cargo clippy --workspace --no-deps --all-targets -- -D warnings` when the checks are relevant to the change.
- For UI changes, verify the rendered window: `crates/avs-app/selftest/run.sh <script-name>` (a script in `crates/avs-app/selftest/`, such as `edit1`) drives the editor against an isolated fixture library and saves screenshots of the real window. It runs the debug build in `target/` (or `$AVS_TARGET_DIR`), not the installed app, so build it first as the script header says. Open the screenshots; a passing script is not a visual check. The display must be awake and unlocked.
- For MCP changes, add the check to `crates/avs-mcp/tests/stdio.rs` (it drives the JSON-RPC handler the stdio loop uses) rather than writing a separate smoke script; `cargo test --workspace` runs it.

## Documentation

- Keep `README.md` aligned with the actual crates, commands, and current limitations.
- When a schema or tool contract changes, update `tools.json` / `contract.json`, regenerate, and update README notes or tests in the same task.
- Do not promote exploratory notes into binding rules unless they belong in this `AGENTS.md`.
