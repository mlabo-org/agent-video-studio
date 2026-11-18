//! Export job state kept outside any process, one JSON file per job under
//! `<dataDirectory>/.export-jobs/<jobId>.json`. The exporter (a separate crate)
//! reports progress here, the MCP server or UI reads status and sets the cancel
//! flag, and either can run in a different process.
//!
//! Every change is a locked read-modify-write of one field group, so a progress
//! update never erases a cancel request and the reverse. Finished jobs never change again.

use crate::error::{Result, SessionError};
use crate::fsutil::{DEFAULT_LOCK_TIMEOUT, Lock, now_iso, read_optional, write_json};
use crate::session::LibraryPaths;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const JOB_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Starting,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn is_finished(self) -> bool {
        matches!(self, JobState::Completed | JobState::Failed | JobState::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportJob {
    pub schema_version: u32,
    pub job_id: String,
    pub state: JobState,
    /// 0.0 to 1.0.
    pub progress: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    /// Process id of the exporter, once it started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub cancel_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

/// A job plus whether its process is still there.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStatus {
    #[serde(flatten)]
    pub job: ExportJob,
    /// `None` when no pid is recorded or liveness cannot be checked.
    pub process_alive: Option<bool>,
    /// Not finished, but the recorded process is gone (it crashed or was killed).
    pub stale: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewExportJob {
    /// Chosen by the caller; generated (`export-<random>`) when omitted.
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub output_path: Option<String>,
    #[serde(default)]
    pub session_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "result")]
pub enum JobResult {
    Completed { output_path: Option<String> },
    Failed { error: String },
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct ExportJobs {
    directory: PathBuf,
}

impl ExportJobs {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        ExportJobs { directory: directory.into() }
    }

    pub fn for_library(paths: &LibraryPaths) -> Self {
        ExportJobs::new(paths.export_jobs_directory())
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn file(&self, job_id: &str) -> Result<PathBuf> {
        let valid = !job_id.is_empty()
            && job_id.len() <= 64
            && job_id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            && job_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !valid {
            return Err(SessionError::invalid("JOB_ID_INVALID", "A job id is 1-64 letters, digits, '_' or '-', starting with a letter or digit.").with("jobId", job_id));
        }
        Ok(self.directory.join(format!("{job_id}.json")))
    }

    fn lock(&self) -> Result<Lock> {
        Lock::acquire(&self.directory.join(".lock"), DEFAULT_LOCK_TIMEOUT)
    }

    fn read(&self, job_id: &str) -> Result<ExportJob> {
        let path = self.file(job_id)?;
        let bytes = read_optional(&path)?.ok_or_else(|| SessionError::not_found("JOB_NOT_FOUND", format!("No export job {job_id}.")).with("jobId", job_id))?;
        serde_json::from_slice(&bytes).map_err(|e| SessionError::invalid("JOB_INVALID", format!("Export job file {} is unreadable: {e}", path.display())).with("jobId", job_id))
    }

    /// Creates a job in state `starting`.
    pub fn create(&self, new: &NewExportJob) -> Result<ExportJob> {
        let job_id = match &new.job_id {
            Some(id) => id.clone(),
            None => format!("export-{}", uuid::Uuid::new_v4().simple().to_string().get(..12).unwrap_or("000000000000")),
        };
        let path = self.file(&job_id)?;
        let _lock = self.lock()?;
        if path.exists() {
            return Err(SessionError::conflict("JOB_EXISTS", format!("Export job {job_id} already exists.")).with("jobId", job_id));
        }
        let now = now_iso();
        let job = ExportJob {
            schema_version: JOB_SCHEMA_VERSION,
            job_id,
            state: JobState::Starting,
            progress: 0.0,
            output_path: new.output_path.clone(),
            pid: None,
            cancel_requested: false,
            error: None,
            session_revision: new.session_revision,
            message: None,
            created_at: now.clone(),
            updated_at: now,
            finished_at: None,
        };
        write_json(&path, &job)?;
        Ok(job)
    }

    pub fn get(&self, job_id: &str) -> Result<JobStatus> {
        Ok(status_of(self.read(job_id)?))
    }

    /// All jobs, newest first. Unreadable files are skipped.
    pub fn list(&self) -> Result<Vec<JobStatus>> {
        let reader = match fs::read_dir(&self.directory) {
            Ok(reader) => reader,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(SessionError::io("list", &self.directory, &e)),
        };
        let mut jobs: Vec<ExportJob> = reader
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let id = name.strip_suffix(".json")?;
                self.read(id).ok()
            })
            .collect();
        jobs.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.job_id.cmp(&b.job_id)));
        Ok(jobs.into_iter().map(status_of).collect())
    }

    fn update(&self, job_id: &str, change: impl FnOnce(&mut ExportJob) -> Result<()>) -> Result<ExportJob> {
        let path = self.file(job_id)?;
        self.read(job_id)?; // not found: report it without creating the folder or lock file
        let _lock = self.lock()?;
        let mut job = self.read(job_id)?;
        change(&mut job)?;
        job.updated_at = now_iso();
        write_json(&path, &job)?;
        Ok(job)
    }

    /// The exporter calls this when its process starts. Keeps `cancelling` if a cancel was already requested.
    pub fn mark_running(&self, job_id: &str, pid: u32) -> Result<ExportJob> {
        self.update(job_id, |job| {
            ensure_open(job)?;
            job.pid = Some(pid);
            if job.state == JobState::Starting {
                job.state = JobState::Running;
            }
            Ok(())
        })
    }

    /// The exporter reports progress (clamped to 0..=1). It never changes `cancelling` back to `running`.
    pub fn report_progress(&self, job_id: &str, progress: f64, message: Option<&str>) -> Result<ExportJob> {
        self.update(job_id, |job| {
            ensure_open(job)?;
            if job.state == JobState::Starting {
                job.state = JobState::Running;
            }
            if progress.is_finite() {
                job.progress = progress.clamp(0.0, 1.0);
            }
            if let Some(message) = message {
                job.message = Some(message.to_string());
            }
            Ok(())
        })
    }

    /// Asks the exporter to stop: sets the flag and state `cancelling`. A finished job is returned unchanged.
    pub fn request_cancel(&self, job_id: &str) -> Result<ExportJob> {
        self.update(job_id, |job| {
            if !job.state.is_finished() {
                job.cancel_requested = true;
                job.state = JobState::Cancelling;
            }
            Ok(())
        })
    }

    /// The exporter records the end of the job. Fails with `JOB_FINISHED` if it already ended.
    pub fn finish(&self, job_id: &str, result: &JobResult) -> Result<ExportJob> {
        self.update(job_id, |job| {
            ensure_open(job)?;
            match result {
                JobResult::Completed { output_path } => {
                    job.state = JobState::Completed;
                    job.progress = 1.0;
                    if output_path.is_some() {
                        job.output_path = output_path.clone();
                    }
                }
                JobResult::Failed { error } => {
                    job.state = JobState::Failed;
                    job.error = Some(error.clone());
                }
                JobResult::Cancelled => job.state = JobState::Cancelled,
            }
            job.finished_at = Some(now_iso());
            Ok(())
        })
    }

    /// Marks every unfinished job whose process is gone as failed. Returns those jobs.
    pub fn reap_stale(&self) -> Result<Vec<ExportJob>> {
        let mut reaped = Vec::new();
        for status in self.list()? {
            if status.stale {
                let job = self.update(&status.job.job_id, |job| {
                    if job.state.is_finished() {
                        return Ok(());
                    }
                    job.state = if job.cancel_requested { JobState::Cancelled } else { JobState::Failed };
                    if job.state == JobState::Failed {
                        job.error = Some("The export process ended without reporting a result.".into());
                    }
                    job.finished_at = Some(now_iso());
                    Ok(())
                })?;
                reaped.push(job);
            }
        }
        Ok(reaped)
    }
}

fn ensure_open(job: &ExportJob) -> Result<()> {
    if job.state.is_finished() {
        return Err(SessionError::conflict("JOB_FINISHED", format!("Export job {} already ended ({:?}).", job.job_id, job.state)).with("jobId", job.job_id.clone()));
    }
    Ok(())
}

fn status_of(job: ExportJob) -> JobStatus {
    let process_alive = job.pid.and_then(pid_is_alive);
    let stale = !job.state.is_finished() && process_alive == Some(false);
    JobStatus { job, process_alive, stale }
}

/// Whether a process with this id exists. `None` where that cannot be checked.
/// A reused pid reads as alive; compare with `createdAt` if that matters.
pub fn pid_is_alive(pid: u32) -> Option<bool> {
    pid_alive_impl(pid)
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn pid_alive_impl(pid: u32) -> Option<bool> {
    unsafe extern "C" {
        safe fn kill(pid: i32, signal: i32) -> i32;
    }
    const EPERM: i32 = 1;
    let pid = i32::try_from(pid).ok().filter(|p| *p > 0)?;
    // Signal 0 sends nothing; it only reports whether the process exists.
    if kill(pid, 0) == 0 {
        return Some(true);
    }
    Some(std::io::Error::last_os_error().raw_os_error() == Some(EPERM))
}

#[cfg(not(unix))]
fn pid_alive_impl(_pid: u32) -> Option<bool> {
    None
}
