//! `avs.auto-edit-plan.v1`: a list of edits another agent or plugin prepared, to be
//! inspected first and then applied as one unit.
//!
//! A plan step is `{"operationId": "...", "type": "<tool name>", ...arguments}`. Any
//! [`Operation`] may appear. Steps run in order and each one sees the timeline as the
//! previous step left it, so times in a step refer to that intermediate state.

use crate::error::{EditError, Res};
use crate::model::{Metadata, Project};
use crate::ops::{self, Operation, Outcome, RangeEdit};
use crate::time::Time;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const AUTO_EDIT_PLAN_SCHEMA: &str = "avs.auto-edit-plan.v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanProducer {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanTarget {
    pub project_id: String,
    pub base_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoEditPlan {
    pub schema: String,
    pub plan_id: String,
    pub producer: PlanProducer,
    pub target: PlanTarget,
    pub operations: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanStep {
    pub operation_id: String,
    pub operation: Operation,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepResult {
    pub operation_id: String,
    #[serde(rename = "type")]
    pub tool: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanExecution {
    pub project: Project,
    pub steps: Vec<StepResult>,
}

fn plan_error(code: &'static str, message: impl Into<String>) -> EditError {
    EditError::new(code, message)
}

/// Checks the plan's identity and turns its steps into operations. Does not look at
/// the revision: the session store compares `target.base_revision` under its lock.
pub fn steps(project: &Project, plan: &AutoEditPlan) -> Res<Vec<PlanStep>> {
    if plan.schema != AUTO_EDIT_PLAN_SCHEMA {
        return Err(plan_error("AUTO_EDIT_PLAN_UNSUPPORTED_SCHEMA", format!("Expected schema {AUTO_EDIT_PLAN_SCHEMA}, got {}.", plan.schema)));
    }
    if plan.plan_id.trim().is_empty() || plan.producer.id.trim().is_empty() {
        return Err(plan_error("AUTO_EDIT_PLAN_INVALID_IDENTITY", "planId and producer.id must not be blank."));
    }
    if plan.target.project_id != project.id {
        return Err(plan_error("AUTO_EDIT_PLAN_PROJECT_MISMATCH", format!("The plan targets project {}, the open project is {}.", plan.target.project_id, project.id))
            .with("planProjectId", plan.target.project_id.clone())
            .with("projectId", project.id.clone()));
    }
    if plan.operations.is_empty() {
        return Err(plan_error("AUTO_EDIT_PLAN_EMPTY", "A plan needs at least one operation."));
    }
    let mut seen: Vec<String> = Vec::new();
    let mut steps = Vec::new();
    for (index, raw) in plan.operations.iter().enumerate() {
        let Value::Object(fields) = raw else {
            return Err(plan_error("AUTO_EDIT_PLAN_INVALID_OPERATION", format!("operations[{index}] must be an object.")));
        };
        let mut args: Map<String, Value> = fields.clone();
        let operation_id = match args.remove("operationId") {
            Some(Value::String(id)) if !id.trim().is_empty() => id,
            _ => return Err(plan_error("AUTO_EDIT_PLAN_INVALID_OPERATION", format!("operations[{index}] needs a non-blank operationId."))),
        };
        if seen.contains(&operation_id) {
            return Err(plan_error("AUTO_EDIT_PLAN_DUPLICATE_OPERATION_ID", format!("operationId {operation_id} appears more than once.")).with("operationId", operation_id));
        }
        seen.push(operation_id.clone());
        let Some(Value::String(tool)) = args.remove("type") else {
            return Err(plan_error("AUTO_EDIT_PLAN_INVALID_OPERATION", format!("Operation {operation_id} needs a type.")).with("operationId", operation_id));
        };
        // Provenance a producer may attach to a step; not an argument of the edit.
        args.remove("sourceRangeIds");
        let operation: Operation = serde_json::from_value(json!({ "tool": tool, "args": args })).map_err(|error| {
            plan_error("AUTO_EDIT_PLAN_UNSUPPORTED_OPERATION", format!("Operation {operation_id} ({tool}) is not a valid edit: {error}"))
                .with("operationId", operation_id.clone())
                .with("type", tool.clone())
        })?;
        steps.push(PlanStep { operation_id, operation });
    }
    Ok(steps)
}

/// Runs every step in order on a copy of the project. Fails as a whole on the first
/// step that fails, naming it; the caller's project is never changed.
pub fn execute(project: &Project, plan: &AutoEditPlan) -> Res<PlanExecution> {
    let mut current = project.clone();
    let mut results = Vec::new();
    for step in steps(project, plan)? {
        let (next, outcome) = ops::apply(&current, &step.operation).map_err(|error| error.with("operationId", step.operation_id.clone()))?;
        current = next;
        results.push(StepResult { operation_id: step.operation_id, tool: step.operation.tool_name(), outcome });
    }
    Ok(PlanExecution { project: current, steps: results })
}

// ---------------------------------------------------------------- silence removal

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SilenceRange {
    pub id: String,
    pub start_seconds: Time,
    pub end_seconds: Time,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanSilenceRemoval {
    pub plan_id: String,
    pub producer: PlanProducer,
    pub silence_ranges: Vec<SilenceRange>,
    pub track_ids: Vec<String>,
    #[serde(default)]
    pub include_linked_companions: Option<bool>,
    #[serde(default)]
    pub preserve_leading_seconds: Option<Time>,
    #[serde(default)]
    pub preserve_trailing_seconds: Option<Time>,
    #[serde(default)]
    pub minimum_cut_seconds: Option<Time>,
    #[serde(default)]
    pub metadata: Option<Metadata>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SilencePolicy {
    pub preserve_leading_seconds: Time,
    pub preserve_trailing_seconds: Time,
    pub minimum_cut_seconds: Time,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedSilence {
    pub source_range_ids: Vec<String>,
    pub start_seconds: Time,
    pub end_seconds: Time,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SilencePlan {
    pub plan: AutoEditPlan,
    pub policy: SilencePolicy,
    pub skipped_ranges: Vec<SkippedSilence>,
}

/// Builds a plan that removes externally detected silences from the given tracks.
/// Each range is shortened by the preserve handles, ranges that touch are merged,
/// and the cuts are emitted from last to first so every step's times are valid on
/// the timeline as it stood before the plan.
pub fn plan_silence_removal(project: &Project, base_revision: u64, input: &PlanSilenceRemoval) -> Res<SilencePlan> {
    if input.plan_id.trim().is_empty() || input.producer.id.trim().is_empty() {
        return Err(plan_error("AUTO_EDIT_PLAN_INVALID_IDENTITY", "planId and producer.id must not be blank."));
    }
    if input.track_ids.is_empty() {
        return Err(plan_error("AUTO_EDIT_PLAN_TRACKS_REQUIRED", "trackIds must name at least one track."));
    }
    for track_id in &input.track_ids {
        project.track_index(track_id)?;
    }
    if input.silence_ranges.is_empty() {
        return Err(plan_error("AUTO_EDIT_PLAN_EMPTY_SILENCE_RANGES", "At least one detected silence range is required."));
    }
    let handle = |name: &str, value: Option<Time>, default_micros: i64| -> Res<Time> {
        let value = value.unwrap_or(Time::from_micros(default_micros));
        if value.is_negative() {
            return Err(plan_error("AUTO_EDIT_PLAN_INVALID_POLICY", format!("{name} must not be negative.")));
        }
        Ok(value)
    };
    let policy = SilencePolicy {
        preserve_leading_seconds: handle("preserveLeadingSeconds", input.preserve_leading_seconds, 100_000)?,
        preserve_trailing_seconds: handle("preserveTrailingSeconds", input.preserve_trailing_seconds, 100_000)?,
        minimum_cut_seconds: handle("minimumCutSeconds", input.minimum_cut_seconds, 250_000)?,
    };
    let timeline_end = project.duration();
    let mut seen: Vec<&str> = Vec::new();
    let mut cuts: Vec<(Time, Time, Vec<String>)> = Vec::new();
    let mut skipped = Vec::new();
    for range in &input.silence_ranges {
        if range.id.trim().is_empty() || seen.contains(&range.id.as_str()) {
            return Err(plan_error("AUTO_EDIT_PLAN_DUPLICATE_SILENCE_RANGE_ID", format!("Silence range id \"{}\" is blank or duplicated.", range.id)).with("rangeId", range.id.clone()));
        }
        seen.push(&range.id);
        if range.start_seconds.is_negative() || range.end_seconds <= range.start_seconds {
            return Err(plan_error("AUTO_EDIT_PLAN_INVALID_SILENCE_RANGE", "A silence range needs 0 <= startSeconds < endSeconds.").with("rangeId", range.id.clone()));
        }
        if range.end_seconds > timeline_end {
            return Err(plan_error("AUTO_EDIT_PLAN_SILENCE_RANGE_OUT_OF_BOUNDS", "A silence range ends after the timeline does.")
                .with("rangeId", range.id.clone())
                .with("projectDurationSeconds", timeline_end.secs()));
        }
        let start = range.start_seconds + policy.preserve_leading_seconds;
        let end = range.end_seconds - policy.preserve_trailing_seconds;
        if end - start < policy.minimum_cut_seconds || end <= start {
            skipped.push(SkippedSilence { source_range_ids: vec![range.id.clone()], start_seconds: range.start_seconds, end_seconds: range.end_seconds, reason: "below-minimum-cut" });
            continue;
        }
        cuts.push((start, end, vec![range.id.clone()]));
    }
    cuts.sort_by_key(|cut| (cut.0, cut.1));
    let mut merged: Vec<(Time, Time, Vec<String>)> = Vec::new();
    for cut in cuts {
        match merged.last_mut() {
            Some(last) if cut.0 <= last.1 => {
                last.1 = last.1.max(cut.1);
                last.2.extend(cut.2);
            }
            _ => merged.push(cut),
        }
    }
    if merged.is_empty() {
        return Err(plan_error("AUTO_EDIT_PLAN_NO_EFFECTIVE_SILENCE_RANGES", "No silence range is long enough once the preserve handles are applied."));
    }
    let include_linked = input.include_linked_companions.unwrap_or(true);
    let operations: Vec<Value> = merged
        .iter()
        .rev()
        .enumerate()
        .map(|(index, (start, end, source_ids))| {
            let range = RangeEdit { start_seconds: *start, end_seconds: *end, track_ids: Some(input.track_ids.clone()), include_linked_companions: Some(include_linked) };
            let mut step = serde_json::to_value(&range).unwrap_or_else(|_| json!({}));
            if let Value::Object(fields) = &mut step {
                fields.insert("operationId".into(), json!(format!("silence-cut-{:03}", index + 1)));
                fields.insert("type".into(), json!("extract_range"));
                fields.insert("sourceRangeIds".into(), json!(source_ids));
            }
            step
        })
        .collect();
    let mut metadata = input.metadata.clone().unwrap_or_default();
    metadata.insert(
        "silenceRemoval".into(),
        json!({
            "coordinateSpace": "current-timeline-before-plan",
            "operationOrder": "descending-time",
            "sourceRangeCount": input.silence_ranges.len(),
            "effectiveCutCount": operations.len(),
            "policy": policy,
        }),
    );
    Ok(SilencePlan {
        plan: AutoEditPlan {
            schema: AUTO_EDIT_PLAN_SCHEMA.into(),
            plan_id: input.plan_id.clone(),
            producer: input.producer.clone(),
            target: PlanTarget { project_id: project.id.clone(), base_revision },
            operations,
            metadata: Some(metadata),
        },
        policy,
        skipped_ranges: skipped,
    })
}
