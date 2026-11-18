//! Auto-edit plans and read-only queries.

use avs_core::plan::{self, AutoEditPlan};
use avs_core::query::{self, CollectClipsInRange, MediaSearch};
use avs_core::{Operation, Project, apply, secs, validate};
use serde_json::{Value, json};

fn run(project: &Project, tool: &str, args: Value) -> Project {
    let operation: Operation = serde_json::from_value(json!({ "tool": tool, "args": args })).unwrap();
    apply(project, &operation).unwrap().0
}

/// A 20 s linked video/audio pair on V1/A1.
fn talk() -> Project {
    let mut project = Project::empty("p", "Talk");
    project = run(&project, "register_media", json!({ "id": "talk", "name": "Talk take 3", "type": "video", "durationSeconds": 20, "tags": ["Interview"], "transcript": "hello world" }));
    run(&project, "add_clips", json!({ "clips": [{ "id": "v", "trackId": "V1", "mediaId": "talk", "startSeconds": 0, "linkedAudio": { "id": "a", "trackId": "A1" } }] }))
}

fn silence_plan(project: &Project) -> plan::SilencePlan {
    let input = serde_json::from_value(json!({
        "planId": "plan-1", "producer": { "id": "stt" }, "trackIds": ["V1"],
        "silenceRanges": [
            { "id": "s1", "startSeconds": 2, "endSeconds": 4 },
            { "id": "s2", "startSeconds": 10, "endSeconds": 12 },
            { "id": "tiny", "startSeconds": 15, "endSeconds": 15.3 }
        ]
    }))
    .unwrap();
    plan::plan_silence_removal(project, 7, &input).unwrap()
}

#[test]
fn silence_plan_cuts_from_the_end_and_skips_short_ranges() {
    let project = talk();
    let result = silence_plan(&project);
    assert_eq!(result.plan.target.base_revision, 7);
    assert_eq!(result.skipped_ranges.len(), 1);
    let starts: Vec<f64> = result.plan.operations.iter().map(|step| step["startSeconds"].as_f64().unwrap()).collect();
    assert_eq!(starts, vec![10.1, 2.1], "later cut first, each shortened by the 0.1 s handles");

    let executed = plan::execute(&project, &result.plan).unwrap();
    // 1.8 s removed twice from a 20 s timeline, on the video and on its linked audio.
    assert_eq!(executed.project.duration(), secs(16.4));
    assert_eq!(executed.project.track("A1").unwrap().clips.len(), 3);
    assert!(executed.project.clips().all(|clip| clip.link.is_some()), "every piece is still paired");
    assert!(validate(&executed.project).ok);
    assert_eq!(executed.steps.len(), 2);
    assert_eq!(project.duration(), secs(20.0), "the input project is untouched");
}

#[test]
fn a_plan_can_use_any_edit_and_fails_as_a_whole_naming_the_step() {
    let project = talk();
    let make = |operations: Value| -> AutoEditPlan {
        serde_json::from_value(json!({ "schema": "avs.auto-edit-plan.v1", "planId": "x", "producer": { "id": "t" }, "target": { "projectId": "p", "baseRevision": 1 }, "operations": operations })).unwrap()
    };
    let good = make(json!([
        { "operationId": "cut", "type": "blade_at_time", "atSeconds": 5 },
        { "operationId": "quiet", "type": "set_clip_audio", "clipId": "a", "volumeDb": -6 }
    ]));
    assert_eq!(plan::execute(&project, &good).unwrap().project.clips().count(), 4);

    let bad = make(json!([
        { "operationId": "cut", "type": "blade_at_time", "atSeconds": 5 },
        { "operationId": "boom", "type": "delete_clip", "clipId": "nope" }
    ]));
    let error = plan::execute(&project, &bad).unwrap_err();
    assert_eq!(error.code, "CLIP_NOT_FOUND");
    assert_eq!(error.details["operationId"], "boom");

    let unknown = make(json!([{ "operationId": "x", "type": "format_disk" }]));
    assert_eq!(plan::execute(&project, &unknown).unwrap_err().code, "AUTO_EDIT_PLAN_UNSUPPORTED_OPERATION");

    let mut other_project = good.clone();
    other_project.target.project_id = "someone-else".into();
    assert_eq!(plan::execute(&project, &other_project).unwrap_err().code, "AUTO_EDIT_PLAN_PROJECT_MISMATCH");
}

#[test]
fn queries_report_what_is_on_the_timeline() {
    let project = talk();
    let inspection = query::inspect(&project);
    assert_eq!((inspection.clip_count, inspection.media_count, inspection.duration_seconds), (2, 1, secs(20.0)));
    assert_eq!(inspection.media_usage[0].clip_ids, vec!["v", "a"]);
    assert!(inspection.issues.is_empty());

    let found = query::search_media(&project, &MediaSearch { text: Some("TAKE hello".into()), tags: Some(vec!["interview".into()]), ..Default::default() });
    assert_eq!(found.len(), 1);
    assert!(query::search_media(&project, &MediaSearch { text: Some("missing".into()), ..Default::default() }).is_empty());

    let collected = query::collect_clips_in_range(&project, &CollectClipsInRange { start_seconds: secs(1.0), end_seconds: secs(2.0), track_ids: Some(vec!["V1".into()]), include_linked_companions: None }).unwrap();
    assert_eq!(collected.direct_clip_ids, vec!["v"]);
    assert_eq!(collected.linked_companion_clip_ids, vec!["a"]);
    assert_eq!(query::snapshot(&project).tracks.len(), 3);
}
