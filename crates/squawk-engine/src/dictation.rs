//! One push-to-talk or hands-free dictation.
//!
//! Latency is the point of this module. While the user talks, the segmenter
//! cuts the audio at pauses and each committed segment is transcribed right
//! away, so by the time fn comes up only the tail since the last pause is
//! left. Target: `finish` returns within ~250 ms of being called for a 20 s
//! utterance on an M4.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::error::EngineError;

/// A dictation in progress. `Send`, so it can be started on the hotkey
/// thread and finished on a worker. Dropping it cancels.
pub struct DictationSession {
    started_at: Instant,
}

/// What a finished dictation produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    /// Raw model text, segments joined with single spaces. Empty when
    /// nothing was said (too short, or silence).
    pub text: String,
    /// Seconds of audio captured.
    pub audio_secs: f32,
    /// From the `finish` call to the text being ready: the number to log.
    pub tail_latency: Duration,
    /// How many segments were transcribed (committed ones plus the tail).
    pub segments: usize,
    /// The kept WAV, when `keep_audio` is on.
    pub audio_path: Option<PathBuf>,
}

impl DictationSession {
    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Stop the mic, transcribe what is left, and return the whole text.
    /// Blocks: call it off the main thread. Waits for the model if it is
    /// still loading.
    pub fn finish(self) -> Result<Transcript, EngineError> {
        todo!("engine agent")
    }

    /// Stop the mic and drop everything. Returns immediately; in-flight
    /// segment jobs are discarded when they complete.
    pub fn cancel(self) {
        todo!("engine agent")
    }
}
