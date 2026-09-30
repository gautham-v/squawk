//! Meeting recording: mic as "You", system audio as "Them".
//!
//! Each track is cut into ~`chunk_secs` pieces (at a pause near the target
//! length when there is one) and transcribed at meeting priority with
//! segment timestamps. Segments are offset to meeting time, merged with
//! `squawk_core::store::merge_segments`, and the whole file is rewritten via
//! `Store::write_meeting` after every chunk (front matter `status:
//! recording` until the end), so a crash loses at most one chunk.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Local};
use squawk_core::status::MeetingInfo;

use crate::error::EngineError;

#[derive(Debug, Clone, PartialEq)]
pub struct MeetingOptions {
    pub title: String,
    /// Where to write; the app gets it from `Store::new_meeting_path`.
    pub path: PathBuf,
    pub started_at: DateTime<Local>,
    pub chunk_secs: u64,
    /// Capture system audio as "Them". If ScreenCaptureKit fails the meeting
    /// continues mic-only and an `EngineEvent::MeetingWarning` is sent.
    pub system_audio: bool,
}

/// A meeting in progress. Dropping it stops it (as `stop`, result discarded).
pub struct MeetingHandle {
    opts: MeetingOptions,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeetingResult {
    pub path: PathBuf,
    pub title: String,
    pub length_secs: u64,
    /// Whether the "Them" track was actually captured.
    pub had_system_audio: bool,
}

impl MeetingHandle {
    pub fn options(&self) -> &MeetingOptions {
        &self.opts
    }

    pub fn elapsed(&self) -> Duration {
        todo!("engine agent")
    }

    /// For `Response::MeetingStarted` and the status response.
    pub fn info(&self) -> MeetingInfo {
        todo!("engine agent")
    }

    /// Stop both captures, transcribe the last chunks, write the final file
    /// (no `status: recording`). Blocks until done: call it off the main
    /// thread.
    pub fn stop(self) -> Result<MeetingResult, EngineError> {
        todo!("engine agent")
    }
}
