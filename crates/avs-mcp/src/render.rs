//! Render tools: a short preview, a blocking final export, and a background export
//! job whose state lives in the library so any process can watch or cancel it.

use crate::protocol::ToolError;
use crate::server::{Server, ToolResult, expected_revision, invalid, session_error, to_json};
use avs_core::{Project, Time};
use avs_render::{CancelToken, ExportOptions, ExportResult, RenderError, check_output_within, export};
use avs_session::{ExportJobs, JobResult, NewExportJob, Session, SessionError};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEFAULT_PREVIEW: Time = Time::from_micros(10_000_000);

fn render_error(error: RenderError) -> ToolError {
    ToolError::new(error.code(), error.to_string())
}

/// Folders a tool may write renders to: the library's own `previews/` and `exports/`,
/// and the `avs/previews` and `avs/exports` folders of an imported CodexVideo job.
fn allowed_roots(session: &Session, project: &Project) -> Result<Vec<PathBuf>, ToolError> {
    let mut roots = vec![session.data_directory().join("previews"), session.data_directory().join("exports")];
    // Recorded by the handoff import, and only when the manifest lives inside that folder.
    let job_root = project.metadata.as_ref().and_then(|metadata| metadata.get("codexVideo")).and_then(|codex| codex.pointer("/job/jobRoot")).and_then(Value::as_str);
    if let Some(job_root) = job_root.map(PathBuf::from).filter(|root| root.is_absolute() && root.is_dir()) {
        roots.push(job_root.join("avs").join("previews"));
        roots.push(job_root.join("avs").join("exports"));
    }
    for root in &roots {
        std::fs::create_dir_all(root).map_err(|error| ToolError::new("OUTPUT_FOLDER_UNAVAILABLE", format!("Cannot create {}: {error}", root.display())))?;
    }
    Ok(roots)
}

fn output_path(arguments: &Value, roots: &[PathBuf], default: &Path) -> Result<PathBuf, ToolError> {
    let path = match arguments.get("outputPath") {
        None | Some(Value::Null) => default.to_path_buf(),
        Some(Value::String(path)) => PathBuf::from(path),
        Some(_) => return Err(invalid("outputPath must be a string.")),
    };
    check_output_within(&path, roots).map_err(|error| {
        ToolError::new(error.code(), error.to_string()).with_details(json!({ "allowedFolders": roots.iter().map(|root| root.display().to_string()).collect::<Vec<_>>() }))
    })?;
    Ok(path)
}

fn result_json(result: &ExportResult, revision: u64) -> Value {
    json!({ "revision": revision, "render": to_json(result), "externalUploads": false })
}

fn check_revision(expected: Option<u64>, actual: u64) -> Result<(), ToolError> {
    match expected {
        Some(expected) if expected != actual => Err(session_error(SessionError::revision_mismatch("REVISION_MISMATCH", "project session", expected, actual))),
        _ => Ok(()),
    }
}

pub fn call(server: &Server, name: &str, arguments: &Value) -> ToolResult {
    let session = server.session()?;
    let jobs = ExportJobs::for_library(session.paths());
    match name {
        "get_render_final_export_status" => {
            jobs.reap_stale().map_err(session_error)?;
            let status = match arguments.get("jobId").and_then(Value::as_str) {
                Some(job_id) => Some(jobs.get(job_id).map_err(session_error)?),
                None => jobs.list().map_err(session_error)?.into_iter().max_by(|a, b| a.job.created_at.cmp(&b.job.created_at)),
            };
            return Ok(match status {
                Some(status) => json!({ "job": status }),
                None => json!({ "job": Value::Null, "message": "No export job has been started in this library." }),
            });
        }
        "cancel_render_final_export" => {
            let job_id = match arguments.get("jobId").and_then(Value::as_str) {
                Some(job_id) => job_id.to_string(),
                None => jobs
                    .list()
                    .map_err(session_error)?
                    .into_iter()
                    .filter(|status| !status.job.state.is_finished())
                    .max_by(|a, b| a.job.created_at.cmp(&b.job.created_at))
                    .map(|status| status.job.job_id)
                    .ok_or_else(|| ToolError::new("JOB_NOT_FOUND", "No export job is running."))?,
            };
            let job = jobs.request_cancel(&job_id).map_err(session_error)?;
            return Ok(json!({ "job": job, "reason": arguments.get("reason") }));
        }
        _ => {}
    }

    let state = session.load().map_err(session_error)?;
    check_revision(expected_revision(arguments.as_object().unwrap_or(&Default::default()))?, state.revision)?;
    let roots = allowed_roots(&session, &state.project)?;
    let resolver_session = Session::new(session.paths().clone());
    let resolver = move |source: &str| -> Result<PathBuf, String> { resolver_session.resolve_media_source(source).map_err(|error| error.message) };

    match name {
        "render_preview" => {
            let output = output_path(arguments, &roots, &session.data_directory().join("previews").join("preview.mp4"))?;
            let mut options = ExportOptions::new(output);
            options.duration = Some(match arguments.get("durationSeconds") {
                None | Some(Value::Null) => DEFAULT_PREVIEW,
                Some(value) => serde_json::from_value::<Time>(value.clone()).ok().filter(|duration| duration.is_positive()).ok_or_else(|| invalid("durationSeconds must be positive."))?,
            });
            let result = export(&state.project, &resolver, options).map_err(render_error)?;
            Ok(result_json(&result, state.revision))
        }
        "render_final_export" => {
            let output = output_path(arguments, &roots, &session.data_directory().join("exports").join("final-export.mp4"))?;
            let result = export(&state.project, &resolver, ExportOptions::new(output)).map_err(render_error)?;
            Ok(result_json(&result, state.revision))
        }
        _ => {
            jobs.reap_stale().map_err(session_error)?;
            if let Some(running) = jobs.list().map_err(session_error)?.into_iter().find(|status| !status.job.state.is_finished()) {
                return Err(ToolError::new("EXPORT_ALREADY_RUNNING", format!("Export job {} is still {:?}.", running.job.job_id, running.job.state)).with_details(json!({ "job": running })));
            }
            let output = output_path(arguments, &roots, &session.data_directory().join("exports").join("final-export.mp4"))?;
            let job = jobs
                .create(&NewExportJob { job_id: None, output_path: Some(output.display().to_string()), session_revision: Some(state.revision) })
                .map_err(session_error)?;
            let job_id = job.job_id.clone();
            let project = state.project.clone();
            let thread_jobs = jobs.clone();
            std::thread::spawn(move || run_job(&thread_jobs, &job_id, &project, &resolver, output));
            Ok(json!({ "job": job, "revision": state.revision }))
        }
    }
}

/// Runs one export on a background thread, mirroring progress into the job file and
/// stopping when any process sets the job's cancel flag.
fn run_job(jobs: &ExportJobs, job_id: &str, project: &Project, resolver: &(dyn Fn(&str) -> Result<PathBuf, String> + Send), output: PathBuf) {
    let _ = jobs.mark_running(job_id, std::process::id());
    let cancel = CancelToken::new();
    let mut options = ExportOptions::new(output.clone());
    options.cancel = Some(cancel.clone());
    let (progress_jobs, progress_id) = (jobs.clone(), job_id.to_string());
    let mut last_report = Instant::now() - Duration::from_secs(1);
    options.on_progress = Some(Box::new(move |progress| {
        // The job file is the only channel to other processes; touch it a few times a
        // second, not on every ffmpeg progress line.
        if last_report.elapsed() < Duration::from_millis(300) && !progress.finished {
            return;
        }
        last_report = Instant::now();
        if let Ok(job) = progress_jobs.report_progress(&progress_id, progress.ratio, None)
            && job.cancel_requested
        {
            cancel.cancel();
        }
    }));
    let resolver = |source: &str| resolver(source);
    let result = match export(project, &resolver, options) {
        Ok(result) => JobResult::Completed { output_path: Some(result.output.display().to_string()) },
        Err(RenderError::Cancelled) => JobResult::Cancelled,
        Err(error) => JobResult::Failed { error: error.to_string() },
    };
    let _ = jobs.finish(job_id, &result);
}
