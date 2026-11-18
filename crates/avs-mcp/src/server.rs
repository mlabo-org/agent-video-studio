//! The Agent Video Studio tools.
//!
//! Each tool is a thin adapter: timeline edits become an `avs_core::Operation` and go
//! through `Session::apply`, exactly as the editor's edits do. Nothing here keeps
//! state between calls; the active library is resolved from settings on every call,
//! so several MCP processes and the editor always see the same project.

use crate::protocol::{ToolError, ToolHost};
use crate::{handoff, render};
use avs_core::placement::{self, Placement};
use avs_core::plan::{self, AutoEditPlan};
use avs_core::query::{self, CollectClipsInRange, MediaSearch};
use avs_core::{MediaType, Operation, Time, validate};
use avs_session::{Applied, ConfirmedInput, CreateInput, Env, ImportRequest, Session, SessionError, SwitchInput};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

const CATALOG: &str = include_str!("../tools.json");

/// Tools whose arguments are exactly an `Operation` (plus `expectedRevision`).
const EDIT_TOOLS: &[&str] = &[
    "add_track", "reorder_track", "delete_track", "set_track_muted", "set_track_properties", "register_media", "delete_media", "add_clips", "move_clip", "move_clips",
    "trim_clip", "ripple_trim_clip", "roll_edit", "slip_clip", "slide_clip", "trim_clip_to_time", "trim_clips_to_time", "delete_left_at_time",
    "delete_right_at_time", "delete_clips_left_at_time", "delete_clips_right_at_time", "split_clip", "blade_at_time", "delete_clip", "delete_clips",
    "ripple_delete_clip", "close_gap_at_time", "lift_range", "extract_range", "set_clip_properties", "set_clip_audio", "set_caption_text",
    "add_captions", "set_project_settings",
];

/// Edits that remove material from several clips at once. They are undoable like
/// every edit, but an agent must state `confirm: true` so a bulk removal is never an
/// accident of a default argument.
const CONFIRMED_TOOLS: &[&str] = &["delete_clips", "lift_range", "extract_range", "delete_clips_left_at_time", "delete_clips_right_at_time", "apply_auto_edit_plan"];

pub struct Server {
    env: Env,
}

pub type ToolResult = Result<Value, ToolError>;

pub(crate) fn session_error(error: SessionError) -> ToolError {
    ToolError { code: error.code.clone(), message: error.message.clone(), details: error.details.clone() }
}

pub(crate) fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::new("INVALID_ARGUMENTS", message)
}

pub(crate) fn to_json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

pub(crate) fn parse<T: serde::de::DeserializeOwned>(arguments: &Value) -> Result<T, ToolError> {
    serde_json::from_value(arguments.clone()).map_err(|error| invalid(error.to_string()))
}

fn object(arguments: &Value) -> Result<Map<String, Value>, ToolError> {
    match arguments {
        Value::Object(fields) => Ok(fields.clone()),
        Value::Null => Ok(Map::new()),
        _ => Err(invalid("Tool arguments must be an object.")),
    }
}

pub(crate) fn expected_revision(fields: &Map<String, Value>) -> Result<Option<u64>, ToolError> {
    match fields.get("expectedRevision") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| invalid("expectedRevision must be a non-negative integer.")),
    }
}

fn text<'a>(fields: &'a Map<String, Value>, key: &str) -> Result<&'a str, ToolError> {
    fields.get(key).and_then(Value::as_str).ok_or_else(|| invalid(format!("{key} is required and must be a string.")))
}

/// Folds an older spelling of an argument into the current one. Both may be given
/// only if they agree.
fn alias(fields: &mut Map<String, Value>, current: &str, older: &str) -> Result<(), ToolError> {
    let Some(old_value) = fields.remove(older) else { return Ok(()) };
    match fields.get(current) {
        Some(value) if value != &old_value => Err(invalid(format!("{current} and {older} disagree; give one of them."))),
        Some(_) => Ok(()),
        None => {
            fields.insert(current.to_string(), old_value);
            Ok(())
        }
    }
}

/// What a mutation returns: where the session now stands and what the edit did. The
/// project itself is not echoed; `get_timeline` returns it.
pub(crate) fn applied_json(applied: &Applied) -> Value {
    let validation = validate(&applied.project);
    json!({
        "changed": applied.changed,
        "revision": applied.revision,
        "updatedAt": applied.updated_at,
        "outcomes": applied.outcomes,
        "undoDepth": applied.undo_depth,
        "redoDepth": applied.redo_depth,
        "durationSeconds": applied.project.duration(),
        "validation": { "ok": validation.ok, "errors": validation.errors, "warnings": validation.warnings },
        "warnings": applied.warnings,
    })
}

pub fn plan_hash(plan: &Value) -> String {
    // serde_json keeps object keys sorted, so this string is canonical.
    let canonical = serde_json::to_string(plan).unwrap_or_default();
    Sha256::digest(canonical.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Server {
    pub fn new(env: Env) -> Self {
        Self { env }
    }

    pub fn env(&self) -> &Env {
        &self.env
    }

    /// The session of the active library, created empty on first use.
    pub(crate) fn session(&self) -> Result<Session, ToolError> {
        let session = Session::for_env(&self.env).map_err(session_error)?;
        session.ensure(None).map_err(session_error)?;
        Ok(session)
    }

    fn edit(&self, name: &str, arguments: &Value) -> ToolResult {
        let mut fields = object(arguments)?;
        let expected = expected_revision(&fields)?;
        fields.remove("expectedRevision");
        fields.remove("confirm");
        if name == "move_clip" {
            alias(&mut fields, "startSeconds", "startTime")?;
        }
        let operation: Operation = serde_json::from_value(json!({ "tool": name, "args": fields })).map_err(|error| invalid(format!("{name}: {error}")))?;
        let applied = self.session()?.apply(&operation, expected).map_err(session_error)?;
        Ok(applied_json(&applied))
    }

    fn read(&self, name: &str, arguments: &Value) -> ToolResult {
        let state = self.session()?.load().map_err(session_error)?;
        let project = &state.project;
        Ok(match name {
            "get_timeline" => json!({ "revision": state.revision, "timeline": query::snapshot(project) }),
            "list_tracks" => json!({ "revision": state.revision, "tracks": query::track_summaries(project) }),
            "get_media" => json!({ "revision": state.revision, "media": project.media }),
            "inspect_timeline" => json!({ "revision": state.revision, "inspection": query::inspect(project) }),
            "validate_timeline" => json!({ "revision": state.revision, "validation": validate(project) }),
            "get_project_session_status" => json!({
                "revision": state.revision,
                "updatedAt": state.updated_at,
                "libraryId": state.library_id,
                "projectId": project.id,
                "title": project.title,
                "undoDepth": state.undo_depth,
                "redoDepth": state.redo_depth,
                "canUndo": state.undo_depth > 0,
                "canRedo": state.redo_depth > 0,
                "recentActivity": state.activity.iter().rev().take(20).collect::<Vec<_>>(),
                "warnings": state.warnings,
            }),
            "collect_clips_in_range" => {
                let input: CollectClipsInRange = parse(arguments)?;
                let collected = query::collect_clips_in_range(project, &input).map_err(|error| ToolError { code: error.code.into(), message: error.message, details: error.details })?;
                json!({ "revision": state.revision, "clips": collected })
            }
            "search_media" => {
                let fields = object(arguments)?;
                let kind: Option<MediaType> = match fields.get("kind") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(parse(value)?),
                };
                let search = MediaSearch {
                    text: fields.get("query").and_then(Value::as_str).map(str::to_string),
                    types: kind.map(|kind| vec![kind]),
                    tags: match fields.get("tags") {
                        None | Some(Value::Null) => None,
                        Some(value) => Some(parse(value)?),
                    },
                    max_results: fields.get("limit").and_then(Value::as_u64).map(|limit| limit as usize),
                };
                json!({ "revision": state.revision, "media": query::search_media(project, &search) })
            }
            _ => return Err(ToolError::new("UNKNOWN_TOOL", format!("No tool named {name}."))),
        })
    }

    // ------------------------------------------------------------ auto-edit plans

    fn dry_run(&self, plan_value: &Value) -> Result<(AutoEditPlan, Value, u64), ToolError> {
        let state = self.session()?.load().map_err(session_error)?;
        let plan: AutoEditPlan = parse(plan_value)?;
        if plan.target.base_revision != state.revision {
            return Err(ToolError::new("AUTO_EDIT_PLAN_STALE_REVISION", format!("The plan was made for revision {}, the session is at {}.", plan.target.base_revision, state.revision))
                .with_details(json!({ "planBaseRevision": plan.target.base_revision, "revision": state.revision })));
        }
        let execution = plan::execute(&state.project, &plan).map_err(|error| ToolError { code: error.code.into(), message: error.message, details: error.details })?;
        let projected = json!({
            "operationResults": execution.steps,
            "before": query::inspect(&state.project),
            "after": query::inspect(&execution.project),
            "validation": validate(&execution.project),
        });
        Ok((plan, projected, state.revision))
    }

    fn plan_tool(&self, name: &str, arguments: &Value) -> ToolResult {
        let fields = object(arguments)?;
        match name {
            "plan_silence_removal" => {
                let state = self.session()?.load().map_err(session_error)?;
                let input = parse(arguments)?;
                let built = plan::plan_silence_removal(&state.project, state.revision, &input).map_err(|error| ToolError { code: error.code.into(), message: error.message, details: error.details })?;
                let plan_value = to_json(&built.plan);
                let (_, projected, revision) = self.dry_run(&plan_value)?;
                Ok(json!({ "revision": revision, "plan": plan_value, "planHash": plan_hash(&plan_value), "policy": built.policy, "skippedRanges": built.skipped_ranges, "projected": projected, "mutated": false }))
            }
            "inspect_auto_edit_plan" => {
                let plan_value = fields.get("plan").ok_or_else(|| invalid("plan is required."))?;
                let (plan, projected, revision) = self.dry_run(plan_value)?;
                Ok(json!({ "revision": revision, "planId": plan.plan_id, "planHash": plan_hash(&to_json(&plan)), "projected": projected, "mutated": false }))
            }
            _ => {
                let plan_value = fields.get("plan").ok_or_else(|| invalid("plan is required."))?;
                let expected_hash = text(&fields, "expectedPlanHash")?;
                let expected = expected_revision(&fields)?.ok_or_else(|| invalid("expectedRevision is required."))?;
                let (plan, _, _) = self.dry_run(plan_value)?;
                let actual_hash = plan_hash(&to_json(&plan));
                if actual_hash != expected_hash {
                    return Err(ToolError::new("AUTO_EDIT_PLAN_HASH_MISMATCH", "The plan differs from the one that was inspected.").with_details(json!({ "expectedPlanHash": expected_hash, "planHash": actual_hash })));
                }
                if plan.target.base_revision != expected {
                    return Err(ToolError::new("AUTO_EDIT_PLAN_STALE_REVISION", "expectedRevision must equal the plan's baseRevision."));
                }
                let state = self.session()?.load().map_err(session_error)?;
                let operations: Vec<Operation> = plan::steps(&state.project, &plan)
                    .map_err(|error| ToolError { code: error.code.into(), message: error.message, details: error.details })?
                    .into_iter()
                    .map(|step| step.operation)
                    .collect();
                let applied = self.session()?.apply_many(&operations, Some(expected)).map_err(session_error)?;
                let mut result = applied_json(&applied);
                result["planId"] = json!(plan.plan_id);
                result["planHash"] = json!(actual_hash);
                Ok(result)
            }
        }
    }

    // ------------------------------------------------------------ library and media

    fn library_tool(&self, name: &str, arguments: &Value) -> ToolResult {
        let fields = object(arguments)?;
        let env = &self.env;
        let revision = |key: &str| -> Result<Option<u64>, ToolError> {
            match fields.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(value) => value.as_u64().map(Some).ok_or_else(|| invalid(format!("{key} must be a non-negative integer."))),
            }
        };
        Ok(match name {
            "get_project_library_settings" => json!({ "settings": avs_session::get_settings(env).map_err(session_error)? }),
            "set_project_library_settings" => json!({ "settings": avs_session::set_settings(env, text(&fields, "dataDirectory")?, revision("expectedRevision")?).map_err(session_error)? }),
            "reset_project_library_settings" => json!({ "settings": avs_session::reset_settings(env, revision("expectedRevision")?).map_err(session_error)? }),
            "get_project_io_status" => json!({ "status": avs_session::io_status(env).map_err(session_error)? }),
            "list_recent_project_workspaces" => json!({ "recent": avs_session::recent_list(env).map_err(session_error)? }),
            "forget_recent_project_workspace" => {
                json!({ "result": avs_session::recent_forget(env, text(&fields, "dataDirectory")?, revision("expectedRecentRevision")?).map_err(session_error)? })
            }
            "inspect_project_library_target" => json!({ "inspection": avs_session::inspect_target(env, text(&fields, "targetPath")?) }),
            "inspect_project_workspace_delete" => json!({ "plan": avs_session::inspect_delete(env, text(&fields, "targetPath")?) }),
            "inspect_project_workspace_restore" => json!({ "plan": avs_session::inspect_restore(env, text(&fields, "targetPath")?) }),
            "create_project_session" => json!({ "result": avs_session::create_workspace(env, &parse::<CreateInput>(arguments)?).map_err(session_error)? }),
            "switch_project_library" => json!({ "result": avs_session::switch_workspace(env, &parse::<SwitchInput>(arguments)?).map_err(session_error)? }),
            "delete_project_workspace" => json!({ "result": avs_session::delete_workspace(env, &parse::<ConfirmedInput>(arguments)?).map_err(session_error)? }),
            "restore_project_workspace" => json!({ "result": avs_session::restore_workspace(env, &parse::<ConfirmedInput>(arguments)?).map_err(session_error)? }),
            _ => return Err(ToolError::new("UNKNOWN_TOOL", format!("No tool named {name}."))),
        })
    }

    fn import_tool(&self, name: &str, arguments: &Value) -> ToolResult {
        let mut fields = object(arguments)?;
        let expected = expected_revision(&fields)?;
        for (current, older) in [("durationSeconds", "duration"), ("clipDurationSeconds", "clipDuration"), ("sourceStartSeconds", "sourceStart")] {
            alias(&mut fields, current, older)?;
        }
        let request: ImportRequest = parse(&Value::Object(fields.clone()))?;
        let session = self.session()?;
        if name == "import_media_file" {
            let imported = avs_session::import_media(&self.env, &session, &request, expected).map_err(session_error)?;
            let mut result = applied_json(&imported.applied);
            result["mediaId"] = json!(imported.media_id);
            result["source"] = json!(imported.source);
            result["probe"] = to_json(&imported.probe);
            return Ok(result);
        }

        // Validate the placement arguments before copying anything into the library.
        let time = |key: &str| -> Result<Option<Time>, ToolError> {
            match fields.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(value) => parse::<Time>(value).map(Some),
            }
        };
        let start = time("startSeconds")?.ok_or_else(|| invalid("startSeconds is required."))?;
        let clip_duration = time("clipDurationSeconds")?;
        let source_start = time("sourceStartSeconds")?;
        let track_id = text(&fields, "targetTrackId")?.to_string();
        let edit_mode = match fields.get("editMode") {
            None | Some(Value::Null) => Default::default(),
            Some(value) => parse(value)?,
        };
        let properties = match fields.get("properties") {
            None | Some(Value::Null) => None,
            Some(value) => Some(parse(value)?),
        };

        let prepared = avs_session::prepare_import(&self.env, &session, &request).map_err(session_error)?;
        let placed = (|| -> ToolResult {
            let state = session.load().map_err(session_error)?;
            let media = &prepared.register_media;
            let media_id = media.id.clone().unwrap_or_else(|| state.project.fresh_id("media-", |candidate| state.project.media.iter().any(|existing| existing.id == candidate)));
            let mut register = media.clone();
            register.id = Some(media_id.clone());
            let media_length = if media.media_type.is_still() { Time::from_micros(5_000_000) } else { media.duration_seconds - source_start.unwrap_or(Time::ZERO) };
            let placement = Placement {
                media_id: media_id.clone(),
                media_type: media.media_type,
                has_audio: prepared.probe.as_ref().is_some_and(|probe| probe.has_audio),
                track_id,
                start_seconds: start,
                duration_seconds: clip_duration,
                source_start_seconds: source_start,
                name: fields.get("clipName").and_then(Value::as_str).map(str::to_string),
                properties,
                linked_audio_track_id: fields.get("linkedAudioTargetTrackId").and_then(Value::as_str).map(str::to_string),
                edit_mode,
            };
            let mut operations = vec![Operation::RegisterMedia(register)];
            operations.extend(placement::operations(&state.project, &placement, clip_duration.unwrap_or(media_length)).map_err(|error| ToolError { code: error.code.into(), message: error.message, details: error.details })?);
            let applied = session.apply_many(&operations, expected.or(Some(state.revision))).map_err(session_error)?;
            let mut result = applied_json(&applied);
            result["mediaId"] = json!(media_id);
            result["source"] = json!(prepared.library.media_source);
            result["probe"] = to_json(&prepared.probe);
            Ok(result)
        })();
        if placed.is_err() {
            // Nothing was registered, so the copy would be an orphan in the library.
            avs_session::discard_import(&prepared);
        }
        placed
    }

    fn history_tool(&self, name: &str, arguments: &Value) -> ToolResult {
        let expected = expected_revision(&object(arguments)?)?;
        let session = self.session()?;
        let applied = if name == "undo_project" { session.undo(expected) } else { session.redo(expected) }.map_err(session_error)?;
        Ok(applied_json(&applied))
    }
}

impl ToolHost for Server {
    fn tools(&self) -> Vec<Value> {
        serde_json::from_str(CATALOG).unwrap_or_default()
    }

    fn call(&self, name: &str, arguments: &Value) -> ToolResult {
        if CONFIRMED_TOOLS.contains(&name) && arguments.get("confirm") != Some(&Value::Bool(true)) {
            return Err(ToolError::new("CONFIRMATION_REQUIRED", format!("{name} needs confirm: true.")));
        }
        match name {
            _ if EDIT_TOOLS.contains(&name) => self.edit(name, arguments),
            "get_timeline" | "list_tracks" | "get_media" | "inspect_timeline" | "validate_timeline" | "get_project_session_status" | "collect_clips_in_range" | "search_media" => {
                self.read(name, arguments)
            }
            "plan_silence_removal" | "inspect_auto_edit_plan" | "apply_auto_edit_plan" => self.plan_tool(name, arguments),
            "import_media_file" | "import_media_file_to_timeline" => self.import_tool(name, arguments),
            "undo_project" | "redo_project" => self.history_tool(name, arguments),
            "import_handoff_manifest" | "import_codexvideo_job" => handoff::call(self, arguments),
            "render_preview" | "render_final_export" | "start_render_final_export" | "get_render_final_export_status" | "cancel_render_final_export" => render::call(self, name, arguments),
            _ => self.library_tool(name, arguments),
        }
    }

    fn server_info(&self) -> Value {
        json!({ "name": "agent-video-studio", "title": "Agent Video Studio", "version": env!("CARGO_PKG_VERSION") })
    }

    fn instructions(&self) -> String {
        "Agent Video Studio edits a local video project. Read with get_timeline / inspect_timeline, edit with the timeline tools (every edit is one undoable revision; pass expectedRevision to refuse a stale edit), and render with render_preview or start_render_final_export. The editor app shows the same session live. Media, track and clip ids are stable; reuse the ids tools return.".into()
    }
}
