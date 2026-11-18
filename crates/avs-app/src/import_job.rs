//! Media import off the UI thread: the copy into the library and the probe run on
//! a worker; registering (and placing) happens on the UI thread through the store,
//! so the whole import is one revision and one undo step.

use crate::store::Store;
use avs_core::Time;
use avs_session::{ImportRequest, PreparedImport, SessionError};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

pub struct ImportTask {
    pub file_name: String,
    /// Track and time to place the clip at once registered.
    pub place: Option<(String, Time)>,
    receiver: Receiver<Result<PreparedImport, SessionError>>,
}

impl ImportTask {
    pub fn start(store: &Store, path: PathBuf, place: Option<(String, Time)>) -> ImportTask {
        let env = store.env.clone();
        let session = store.session.clone();
        let file_name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let request = ImportRequest { source_path: path.display().to_string(), ..ImportRequest::default() };
            let _ = sender.send(avs_session::prepare_import(&env, &session, &request));
        });
        ImportTask { file_name, place, receiver }
    }

    pub fn poll(&mut self) -> Option<Result<PreparedImport, SessionError>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(SessionError::invalid("IMPORT_ABORTED", "The import worker stopped unexpectedly."))),
        }
    }
}
