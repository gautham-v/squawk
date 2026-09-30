//! The controller thread: hotkey actions, IPC requests and popover clicks in;
//! dictations, pastes, meetings and [`Snapshot`]s out.
//!
//! A dictation, end to end:
//! 1. `StartRecording` → `engine.start_dictation()` immediately, then (still
//!    on this thread, while the user talks) read the front app, and if it is
//!    a terminal and `claude_code_mode` is on, `context::detect` +
//!    `VocabCache::get`. Publish `Recording`.
//! 2. `EnterHandsFree` → publish `Recording { hands_free: true }`.
//! 3. `StopAndPaste` → publish `Transcribing`; `session.finish()`; run
//!    `pipeline::finish` with the dictionary (via `DictionaryCache`) and the
//!    context from step 1; if non-empty, paste it into the app that was
//!    frontmost at step 1 (on the main thread), append the entry to the store,
//!    log the latency line; publish `Idle`.
//! 4. `Cancel(_)` → `session.cancel()`; publish `Idle`. Nothing is written.

use std::time::Instant;

use crossbeam_channel::Sender;
use squawk_core::hotkey::Action;
use squawk_core::ipc::{Request, Response};
use squawk_core::status::Permissions;
use squawk_core::ModelStatus;

/// What the dictation side is doing, for the menu bar and the popover.
#[derive(Debug, Clone, PartialEq)]
pub enum DictationPhase {
    Idle,
    Recording { since: Instant, hands_free: bool },
    Transcribing,
}

/// A meeting in progress, for the menu bar and the popover footer.
#[derive(Debug, Clone, PartialEq)]
pub struct MeetingSnap {
    pub title: String,
    pub path: std::path::PathBuf,
    pub since: Instant,
}

/// Everything the views need, published by the controller after every
/// change. Cheap to clone.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub dictation: DictationPhase,
    pub meeting: Option<MeetingSnap>,
    pub model: ModelStatus,
    pub permissions: Permissions,
    /// A malformed config.toml, shown as a muted line in the header.
    pub config_note: Option<String>,
    /// The last thing that went wrong (mic failed, paste failed), shown
    /// until the next successful dictation.
    pub last_error: Option<String>,
    /// Bumped when a dictation is saved, so an open History tab re-reads.
    pub dictations_this_run: u64,
}

/// Messages to the controller thread.
pub enum Command {
    Hotkey(Action),
    /// From the socket; the reply goes back on `reply`.
    Ipc {
        request: Request,
        reply: Sender<Response>,
    },
    /// Footer "Record meeting".
    ToggleMeeting,
    /// Permissions may have changed (popover opened, app re-activated).
    RecheckPermissions,
    /// Re-read config.toml.
    Reload,
    Quit,
}

/// Handle to the controller thread.
#[derive(Clone)]
pub struct Controller {
    tx: Sender<Command>,
}

impl Controller {
    /// Spawn the controller thread. `publish` is called on the controller
    /// thread with every new snapshot; it must hand off to the main thread
    /// (e.g. an unbounded futures channel drained by a gpui task) and return.
    pub fn spawn(
        paths: squawk_core::Paths,
        config: squawk_core::Config,
        config_note: Option<String>,
        engine: squawk_engine::Engine,
        publish: impl Fn(Snapshot) + Send + 'static,
    ) -> Controller {
        let _ = (paths, config, config_note, engine, publish);
        todo!("app agent")
    }

    /// Never blocks; a dropped controller drops the message.
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }
}
