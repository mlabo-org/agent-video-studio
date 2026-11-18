//! The editor's link to the session file.
//!
//! The editor holds a copy of the project. Every edit goes to the session as
//! operations with the revision the copy came from; the session's answer replaces
//! the copy. Other writers (the MCP server driven by an agent) are noticed by
//! watching the session file and reloading it.

use avs_core::{Operation, Project};
use avs_session::{ActivityEntry, Applied, Env, Session, SessionError};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

pub const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// What the editor knows of the session right now.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub project: Project,
    pub revision: u64,
    pub updated_at: String,
    pub undo_depth: usize,
    pub redo_depth: usize,
    pub activity: Vec<ActivityEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reload {
    Unchanged,
    /// The file changed and was read again; `external` when the revision came from
    /// another writer.
    Reloaded { external: bool },
    Failed,
}

pub struct Store {
    pub env: Env,
    pub session: Session,
    pub snapshot: Option<Snapshot>,
    /// Why the session could not be read, while it cannot.
    pub load_error: Option<SessionError>,
    stamp: Option<(SystemTime, u64)>,
    /// The settings file as last looked at by `library_switched_elsewhere`.
    settings_stamp: Option<(SystemTime, u64)>,
    last_poll: Instant,
    /// Revisions this editor wrote; anything else is somebody else's edit.
    own_revisions: Vec<u64>,
    /// The revision when this library was opened; older activity predates the editor.
    pub opened_revision: u64,
    /// Own revisions of libraries opened earlier in this run, by data directory.
    earlier: std::collections::HashMap<PathBuf, Vec<u64>>,
}

fn file_stamp(path: &PathBuf) -> Option<(SystemTime, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

impl Store {
    /// Opens the session the settings point at, creating an empty project when the
    /// library has none yet.
    pub fn open(env: Env) -> Store {
        let (session, load_error) = match Session::for_env(&env) {
            Ok(session) => (session, None),
            Err(error) => (Session::in_directory(env.default_data_directory()), Some(error)),
        };
        let settings_stamp = file_stamp(&env.settings_file());
        let mut store = Store { env, session, snapshot: None, load_error, stamp: None, settings_stamp, last_poll: Instant::now(), own_revisions: Vec::new(), opened_revision: 0, earlier: Default::default() };
        if store.load_error.is_none() {
            match store.session.ensure(None) {
                Ok(state) => {
                    store.opened_revision = state.revision;
                    store.stamp = file_stamp(&store.session.paths().session_file);
                    store.snapshot = Some(Snapshot {
                        project: state.project,
                        revision: state.revision,
                        updated_at: state.updated_at,
                        undo_depth: state.undo_depth,
                        redo_depth: state.redo_depth,
                        activity: state.activity,
                    });
                }
                Err(error) => store.load_error = Some(error),
            }
        }
        store
    }

    pub fn project(&self) -> Option<&Project> {
        self.snapshot.as_ref().map(|snapshot| &snapshot.project)
    }

    pub fn revision(&self) -> u64 {
        self.snapshot.as_ref().map_or(0, |snapshot| snapshot.revision)
    }

    /// The library the settings name now, when that is no longer the one this
    /// store has open: another process (an agent's `switch_project_library`)
    /// changed the active library. Reads the settings only when their file changed.
    pub fn library_switched_elsewhere(&mut self) -> Option<PathBuf> {
        if self.env.library_root_active() || self.env.session_path_active() {
            return None;
        }
        let stamp = file_stamp(&self.env.settings_file());
        if stamp == self.settings_stamp {
            return None;
        }
        self.settings_stamp = stamp;
        let active = avs_session::get_settings(&self.env).ok()?.data_directory;
        (active.as_path() != self.session.data_directory()).then_some(active)
    }

    pub fn data_directory(&self) -> PathBuf {
        self.session.data_directory().to_path_buf()
    }

    pub fn media_root(&self) -> PathBuf {
        self.session.paths().media_root()
    }

    pub fn is_own(&self, revision: u64) -> bool {
        self.own_revisions.contains(&revision)
    }

    /// Re-reads the session when its file changed. Cheap when nothing changed (one
    /// `stat`), so it is called every frame and throttles itself.
    pub fn poll(&mut self, force: bool) -> Reload {
        if !force && self.last_poll.elapsed() < POLL_INTERVAL {
            return Reload::Unchanged;
        }
        self.last_poll = Instant::now();
        let stamp = file_stamp(&self.session.paths().session_file);
        if !force && stamp == self.stamp && self.load_error.is_none() {
            return Reload::Unchanged;
        }
        match self.session.load() {
            Ok(state) => {
                self.stamp = stamp;
                self.load_error = None;
                let previous = self.revision();
                let external = state.revision != previous && !self.is_own(state.revision);
                let changed = state.revision != previous;
                self.snapshot = Some(Snapshot {
                    project: state.project,
                    revision: state.revision,
                    updated_at: state.updated_at,
                    undo_depth: state.undo_depth,
                    redo_depth: state.redo_depth,
                    activity: state.activity,
                });
                if changed { Reload::Reloaded { external } } else { Reload::Unchanged }
            }
            Err(error) => {
                // A half-written file is never seen (writers rename into place), so a
                // failure here is real: show it and keep the last good copy.
                self.stamp = stamp;
                self.load_error = Some(error);
                Reload::Failed
            }
        }
    }

    fn adopt(&mut self, applied: &Applied) {
        self.own_revisions.push(applied.revision);
        if self.own_revisions.len() > 64 {
            self.own_revisions.remove(0);
        }
        // The activity log is only in the file; read it back (cheap compared with the write).
        let activity = self.session.load().map(|state| state.activity).unwrap_or_else(|_| self.snapshot.as_ref().map(|s| s.activity.clone()).unwrap_or_default());
        self.snapshot = Some(Snapshot {
            project: applied.project.clone(),
            revision: applied.revision,
            updated_at: applied.updated_at.clone(),
            undo_depth: applied.undo_depth,
            redo_depth: applied.redo_depth,
            activity,
        });
        self.stamp = file_stamp(&self.session.paths().session_file);
    }

    /// Sends operations as one revision (one undo step). When another writer got in
    /// first, the session is read again and the same operations are tried once more
    /// on the new revision: operations name clips by id, so they still mean the same
    /// edit.
    pub fn apply(&mut self, operations: &[Operation]) -> Result<Applied, SessionError> {
        if operations.is_empty() {
            return Err(SessionError::invalid("NO_OPERATIONS", "Nothing to apply."));
        }
        let mut attempt = 0;
        loop {
            let expected = self.snapshot.as_ref().map(|snapshot| snapshot.revision);
            match self.session.apply_many(operations, expected) {
                Ok(applied) => {
                    self.adopt(&applied);
                    return Ok(applied);
                }
                Err(error) if error.is_revision_mismatch() && attempt == 0 => {
                    attempt += 1;
                    self.poll(true);
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn undo(&mut self) -> Result<Applied, SessionError> {
        self.step(true)
    }

    pub fn redo(&mut self) -> Result<Applied, SessionError> {
        self.step(false)
    }

    fn step(&mut self, undo: bool) -> Result<Applied, SessionError> {
        let mut attempt = 0;
        loop {
            let expected = self.snapshot.as_ref().map(|snapshot| snapshot.revision);
            let result = if undo { self.session.undo(expected) } else { self.session.redo(expected) };
            match result {
                Ok(applied) => {
                    if applied.changed {
                        self.adopt(&applied);
                    }
                    return Ok(applied);
                }
                Err(error) if error.is_revision_mismatch() && attempt == 0 => {
                    attempt += 1;
                    self.poll(true);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Points the store at whatever library the settings now name (after a switch).
    pub fn reopen(&mut self) {
        let mut earlier = std::mem::take(&mut self.earlier);
        earlier.insert(self.data_directory(), std::mem::take(&mut self.own_revisions));
        let env = self.env.clone();
        *self = Store::open(env);
        self.own_revisions = earlier.remove(&self.data_directory()).unwrap_or_default();
        self.earlier = earlier;
    }
}
