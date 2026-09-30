//! The recognizer thread: the only owner of the model.
//!
//! One model instance (~700 MB resident) serves dictation and meetings.
//! Jobs queue by priority — every waiting dictation job runs before any
//! meeting chunk — and within a priority in submission order. A meeting
//! chunk already running is not interrupted; chunks are ≤ 30 s so the worst
//! wait is one chunk's inference (~1 s on an M4).

use std::path::PathBuf;

use crossbeam_channel::Receiver;

use crate::error::EngineError;
use crate::model::Recognized;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Someone is waiting to paste.
    Dictation,
    /// Background meeting transcription.
    Meeting,
}

/// Handle to the recognizer thread. Clone freely.
#[derive(Clone)]
pub struct Recognizer {
    _private: (),
}

impl Recognizer {
    /// Spawn the thread and start loading the model from `model_dir`. Jobs
    /// submitted before the load finishes wait for it.
    pub fn spawn(model_dir: PathBuf, threads: usize) -> Recognizer {
        let _ = (model_dir, threads);
        todo!("engine agent")
    }

    /// Queue 16 kHz mono samples. The answer arrives on the returned channel.
    pub fn submit(
        &self,
        samples: Vec<f32>,
        priority: Priority,
    ) -> Receiver<Result<Recognized, EngineError>> {
        let _ = (samples, priority);
        todo!("engine agent")
    }
}
