---
name: agent-video-studio
description: "Agent Video Studio AVS MCP: 動画を手動編集できる形に, CodexVideo handoff, timeline edit, validate, preview/export, plan_silence_removal, avs.auto-edit-plan.v1. Use for 他プラグイン操作や無音カット計画; excludes STT/TTS and generic ffmpeg."
---

# Agent Video Studio

This `SKILL.md` is the local execution contract for this skill when the skill is selected.
The host agent (Codex or Claude Code) must treat this file's trigger assumptions, workflow, tool boundaries, file boundaries, and output shape as binding instructions within this skill's scope.
This file does not override system instructions, developer instructions, explicit user requests, applicable `AGENTS.md` files (Claude Code reads them through `CLAUDE.md`), or more specific local execution contracts.

## Trigger

Use this skill when the user asks to:

- hand a CodexVideo job or `avs.codexvideo-handoff.v1` manifest to AVS
- handle requests phrased as `動画を手動編集できる形に` or `AVSでpreview/export`
- import with `import_handoff_manifest` or `import_codexvideo_job`
- operate Agent Video Studio project sessions, media library, timeline tracks, clips, captions, validation, preview render, or final export
- exchange versioned `avs.auto-edit-plan.v1` data with the host agent (Codex or Claude Code) or another plugin through MCP
- turn externally detected silence ranges into a dry-run edit plan with `plan_silence_removal`
- inspect or atomically apply agent-authored plans with `inspect_auto_edit_plan` and `apply_auto_edit_plan`
- open or inspect the Agent Video Studio UI for manual video editing
- make a CodexVideo output editable downstream instead of claiming CodexVideo Remotion/profile completion

Do not use this skill for normal CodexVideo generation, CodexVideo source-to-video authoring, acting as a substitute for Remotion/profile completion, one-off ffmpeg repairs, external publishing, account authentication, or unrelated video tooling.

## Source And Runtime Boundaries

- Treat the Agent Video Studio repository (the checkout this skill was loaded from; a plugin cache is only a copy of it) as the single plugin source-of-truth, shared by Codex and Claude Code, for source edits, plugin metadata, MCP config, skills, tests, and commits. The implementation is a Rust workspace under `crates/`.
- Both hosts run two installed artifacts built from that source by `scripts/install.sh`: the MCP server `~/.local/bin/agent-video-studio-mcp` (started by `.mcp.json`) and the editor `~/Applications/Agent Video Studio.app`. If the AVS MCP tools are missing, report that the server binary is not installed or the plugin is not loaded; do not substitute `cargo run`, ad hoc scripts, or ffmpeg.
- Do not edit plugin cache copies as primary source. After a Rust source change, the installed binaries are stale until `scripts/install.sh` runs (an install step: run it when handing over a working change or when the user asks, not otherwise, and say whether it ran). Plugin metadata (manifests, `.mcp.json`, this skill) does not go through the install; when a metadata change must be visible to a host, use that host's route: in Codex, refresh the installed copy of the `agent-video-studio` plugin; in Claude Code the plugin loads in place, so it takes effect at the next session start or `/reload-plugins`.
- Renders, imported media, and project data belong in the AVS Project Library (`exports/`, `previews/`, `media/`) or an imported CodexVideo job's `avs/` folder, never in source-controlled plugin files.

## Primary Workflow

1. For a CodexVideo handoff, use the exposed AVS MCP tools rather than ad hoc filesystem or ffmpeg scripts.
2. Import the handoff with `import_handoff_manifest` or `import_codexvideo_job`.
3. Validate the resulting session with `validate_timeline`.
4. Render with `render_preview` or `render_final_export` only after validation is acceptable for the user's goal.
5. Return structured IDs and mappings from MCP responses so later agents can reuse media, track, clip, caption, and session revision identifiers.

Every timeline edit is one undoable project-session revision. Pass `expectedRevision` to refuse an edit made against a stale view; a refused call changes nothing. `delete_clips`, `lift_range`, `extract_range`, `delete_clips_left_at_time`, `delete_clips_right_at_time`, and `apply_auto_edit_plan` require `confirm: true`. Tool results are `{ok: true, tool, ...}` or `{ok: false, tool, error: {code, message, details}}`; mutations return the new `revision` and a validation summary, not the whole project (read it with `get_timeline`).

The editor app and every AVS MCP process work on the same session file, so an edit made through MCP appears in the open editor within a moment, and the editor's edits are visible to the next tool call.

For isolated tests, set `AVS_PROJECT_LIBRARY_ROOT` to a task-local Project Library before starting the MCP server. It fixes only the data directory: the settings and recent-workspace files stay under `~/Library/Application Support/Agent Video Studio`, so a test that must not touch them also sets `HOME` to a task-local folder. For normal local use, let AVS use its default Project Library unless the user asks for an isolated or specific library.

## CodexVideo Handoff Contract

- AVS imports CodexVideo assets into the AVS Project Library for editing and rendering.
- Timeline clips must refer to AVS media IDs, not directly to CodexVideo raw asset paths.
- Preserve CodexVideo provenance from `metadata.codexVideo` and `manifest.job`.
- External output to a CodexVideo job directory is downstream AVS artifact storage only. It does not replace CodexVideo Remotion/profile completion.
- The import is one revision and one undo step. It records `manifest.job.jobRoot` only when the manifest file lives inside that folder; otherwise the import still succeeds with the warning `CODEXVIDEO_HANDOFF_JOB_ROOT_NOT_RECORDED`, and renders under that job root are refused.
- Render tools write an `.mp4` only under the active library's `exports/` or `previews/` folder, or under the recorded job root's `avs/exports/` or `avs/previews/`. Any other `outputPath` is refused with the list of allowed folders. A failed or cancelled export never touches an existing output file.
- `start_render_final_export` runs in the background; its job state lives in the library, so `get_render_final_export_status` and `cancel_render_final_export` work from any AVS process.

## Editor App

Open the editor with `open -a "Agent Video Studio"` only when the user asks to open or inspect the UI. It opens the active Project Library. Its keys and edit behaviour follow Final Cut Pro conventions; the in-app key list is under Help.
MCP headless operation remains the primary route for agent-to-agent handoff and timeline automation.
To verify a UI change, or to look at the editor without the user asking for their library, do not open it on the active Project Library. Start `~/Applications/Agent Video Studio.app/Contents/MacOS/agent-video-studio --library <temporary folder>` (settings and recents stay inside that folder), or run a scripted session with `crates/avs-app/selftest/run.sh <script-name>` (see Source Change Verification).

## Agent-Native Auto-Edit Plan Workflow

Use the MCP plan surface when the host agent (Codex or Claude Code) or another plugin owns analysis, transcription, narration authoring, TTS, or BGM selection:

1. Read the current AVS project/session, tracks, media IDs, and revision.
2. Import generated local audio or other assets with `import_media_file`; reuse the returned AVS media IDs in plan operations.
3. For externally detected silence ranges, call `plan_silence_removal` with stable range IDs and explicit target track IDs. This tool plans and dry-runs; it does not detect silence.
4. For a producer-authored inline `avs.auto-edit-plan.v1`, call `inspect_auto_edit_plan`. A plan operation is `{operationId, type, ...arguments}` where `type` is the name of any AVS timeline edit tool and the arguments are that tool's arguments. Operations execute sequentially, so every timestamp is interpreted against the timeline state immediately before that operation.
5. Reuse the exact returned `planHash`, unchanged plan content, and target revision with `apply_auto_edit_plan`; set `confirm: true` only after the projected operation results and validation are acceptable.
6. Treat the application as one session revision and one undo unit. Reuse the committed validation returned by `apply_auto_edit_plan`; call `validate_timeline` again only after a later independent edit invalidates that evidence before preview or final export.

Plans use stable `planId`, `producer`, `projectId`, `baseRevision`, and per-operation `operationId` values. Tool responses return structured plan hashes, operation results, projected or committed inspection, validation, and session revision data for reuse by later agents.

AVS owns deterministic timeline application. Keep silence detection, STT, script rewriting, TTS synthesis, beat analysis, and provider calls in their owning upstream capability; pass their results into AVS through MCP instead of adding hidden provider behavior to the editor.

## Source Change Verification

For source changes, apply the applicable `AGENTS.md` source-change rules and keep edits scoped to the requested AVS or plugin surface.

- For Rust changes under `crates/` (any crate, including the editor), run `cargo test --workspace` and `cargo clippy --workspace --no-deps --all-targets -- -D warnings` when they are relevant to the change.
- For a tool contract change, edit `crates/avs-mcp/tools.json` or `contract.json` and regenerate with `agent-video-studio-mcp --write-contract .` from the repository root, using a binary built from the changed sources (the catalog is compiled in, so the installed copy writes the old contract); the generated manifest and schemas are not edited by hand.
- For editor changes, also run `crates/avs-app/selftest/run.sh <script-name>` on the debug build (build it first as the script header says) and open its screenshots; a passing script is not a visual check.
- For plugin manifest, `.mcp.json`, or skill changes, run `cargo test -p avs-mcp --test plugin` (routing files, README, and this skill agree with the server), then apply `skill-md-clarifier` to a changed `SKILL.md` and `agents-md-clarifier` to a changed `AGENTS.md` before final reporting.

## Output Shape

When reporting AVS work, include changed files, MCP tool inputs used, session revision, validation result, generated artifact paths, tests or smokes run, unresolved risks, and whether `scripts/install.sh` was run (that is, whether the installed MCP server and editor contain the change).
