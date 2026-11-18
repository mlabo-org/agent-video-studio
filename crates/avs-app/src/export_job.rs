//! Final export on a worker thread with `avs_render::export`. The job is also
//! recorded with `avs_session::ExportJobs`, so an agent sees it through MCP
//! (`get_render_final_export_status`) and can cancel it there.

use crate::app::{AvsApp, StatusKind};
use avs_core::Project;
use avs_render::{CancelToken, ExportOptions};
use avs_session::{ExportJobs, JobResult, NewExportJob};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// The output path and the renderer's warnings, or why the export failed.
type ExportOutcome = Result<(PathBuf, Vec<String>), String>;

pub enum ExportState {
    /// Choosing the output; nothing runs yet.
    Ready,
    Running,
    Done(Result<PathBuf, String>),
}

pub struct ExportTask {
    pub output: PathBuf,
    pub state: ExportState,
    pub progress: Arc<Mutex<f64>>,
    pub job_id: Option<String>,
    pub warnings: Vec<String>,
    cancel: CancelToken,
    receiver: Option<Receiver<ExportOutcome>>,
    pub started: Option<Instant>,
}

impl ExportTask {
    pub fn new(output: PathBuf) -> ExportTask {
        ExportTask { output, state: ExportState::Ready, progress: Arc::new(Mutex::new(0.0)), job_id: None, warnings: Vec::new(), cancel: CancelToken::new(), receiver: None, started: None }
    }

    pub fn start(&mut self, project: Project, revision: u64, jobs: ExportJobs, media_root: PathBuf, ctx: eframe::egui::Context) {
        let job = jobs.create(&NewExportJob { job_id: None, output_path: Some(self.output.display().to_string()), session_revision: Some(revision) });
        let job_id = match job {
            Ok(job) => job.job_id,
            Err(error) => {
                self.state = ExportState::Done(Err(format!("{}: {}", error.code, error.message)));
                return;
            }
        };
        let _ = jobs.mark_running(&job_id, std::process::id());
        self.job_id = Some(job_id.clone());
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.state = ExportState::Running;
        self.started = Some(Instant::now());
        let progress = Arc::clone(&self.progress);
        let cancel = self.cancel.clone();
        let output = self.output.clone();
        std::thread::spawn(move || {
            let mut options = ExportOptions::new(output);
            options.cancel = Some(cancel.clone());
            let progress_jobs = jobs.clone();
            let progress_job = job_id.clone();
            let mut last_report = Instant::now();
            let repaint = ctx.clone();
            options.on_progress = Some(Box::new(move |update| {
                if let Ok(mut value) = progress.lock() {
                    *value = update.ratio;
                }
                repaint.request_repaint();
                // Reported at most twice a second; the job file is also where an agent's
                // cancel request arrives.
                if last_report.elapsed().as_millis() >= 500 || update.finished {
                    last_report = Instant::now();
                    let _ = progress_jobs.report_progress(&progress_job, update.ratio, None);
                    if progress_jobs.get(&progress_job).is_ok_and(|status| status.job.cancel_requested) {
                        cancel.cancel();
                    }
                }
            }));
            let root = media_root.clone();
            let resolver = move |source: &str| avs_session::resolve_media_source(&root, source).map_err(|error| error.message);
            let result = avs_render::export(&project, &resolver, options);
            let job_result = match &result {
                Ok(done) => JobResult::Completed { output_path: Some(done.output.display().to_string()) },
                Err(error) if error.is_cancelled() => JobResult::Cancelled,
                Err(error) => JobResult::Failed { error: error.to_string() },
            };
            let _ = jobs.finish(&job_id, &job_result);
            let _ = sender.send(result.map(|done| (done.output, done.warnings.into_iter().map(|warning| warning.message).collect())).map_err(|error| error.to_string()));
            ctx.request_repaint();
        });
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Cancelled from this window or by an agent (through the job file).
    pub fn was_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn ratio(&self) -> f64 {
        self.progress.lock().map_or(0.0, |value| *value)
    }

    fn poll(&mut self) -> bool {
        let Some(receiver) = &self.receiver else { return false };
        match receiver.try_recv() {
            Ok(result) => {
                match result {
                    Ok((path, warnings)) => {
                        self.warnings = warnings;
                        self.state = ExportState::Done(Ok(path));
                    }
                    Err(error) => self.state = ExportState::Done(Err(error)),
                }
                self.receiver = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.state = ExportState::Done(Err("the export worker stopped".into()));
                self.receiver = None;
                true
            }
        }
    }
}

impl AvsApp {
    /// ⌘E: asks where to write, starting in the library's `exports/` folder.
    pub fn open_export(&mut self) {
        if self.export.as_ref().is_some_and(|task| matches!(task.state, ExportState::Running)) {
            return;
        }
        let Some(project) = self.store.project().cloned() else { return };
        if project.duration() == avs_core::Time::ZERO {
            self.set_status(StatusKind::Info, self.tr("書き出すクリップがありません", "There is nothing to export"));
            return;
        }
        let folder = self.store.data_directory().join("exports");
        let _ = std::fs::create_dir_all(&folder);
        let safe: String = project.title.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' }).collect();
        let name = format!("{}.mp4", if safe.trim().is_empty() { "export" } else { safe.trim() });
        let chosen = rfd::FileDialog::new().set_title(self.tr("書き出し先", "Export To")).set_directory(&folder).set_file_name(&name).add_filter("MP4", &["mp4"]).save_file();
        let Some(mut output) = chosen else { return };
        if output.extension().is_none_or(|extension| !extension.eq_ignore_ascii_case("mp4")) {
            output.set_extension("mp4");
        }
        self.start_export(output);
    }

    pub fn start_export(&mut self, output: PathBuf) {
        let Some(project) = self.store.project().cloned() else { return };
        let mut task = ExportTask::new(output);
        let jobs = ExportJobs::for_library(self.store.session.paths());
        task.start(project, self.store.revision(), jobs, self.store.media_root(), self.ctx.clone());
        self.export = Some(task);
    }

    pub fn poll_export(&mut self) {
        let finished = self.export.as_mut().is_some_and(ExportTask::poll);
        if finished
            && let Some(task) = &self.export {
                match &task.state {
                    ExportState::Done(Ok(path)) => {
                        let text = format!("{} {}", self.tr("書き出しました:", "Exported:"), path.display());
                        self.set_status(StatusKind::Ok, text);
                    }
                    ExportState::Done(Err(_)) if task.was_cancelled() => {
                        self.set_status(StatusKind::Info, self.tr("書き出しをキャンセルしました", "Export cancelled"));
                    }
                    ExportState::Done(Err(error)) => {
                        let error = error.clone();
                        self.set_status(StatusKind::Error, error);
                    }
                    _ => {}
                }
            }
    }
}
