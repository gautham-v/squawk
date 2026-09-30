//! System audio for meetings ("Them"), via ScreenCaptureKit audio-only
//! capture (macOS 13+; we target 15).
//!
//! An `SCStream` on the main display with `captures_audio = true`,
//! `excludes_current_process_audio = true`, sample rate 16 000 and one
//! channel (SCK resamples for us), and the smallest video size it accepts
//! (2×2, 1 fps) since video cannot be switched off entirely; only the
//! `Audio` output handler is added. Needs the "Screen & System Audio
//! Recording" permission; without it `start` fails with `SystemAudio`.
//!
//! (A Core Audio process tap, macOS 14.2+, is the alternative if SCK proves
//! unreliable; it would live behind this same API.)

use crate::error::EngineError;

/// A running system-audio capture. `Send`. Dropping it stops capture.
pub struct SystemAudioCapture {
    _private: (),
}

impl SystemAudioCapture {
    /// Start capturing; `sink` gets 16 kHz mono blocks on an SCK queue
    /// thread and must not block.
    pub fn start(
        sink: impl FnMut(&[f32]) + Send + 'static,
    ) -> Result<SystemAudioCapture, EngineError> {
        let _ = sink;
        todo!("engine agent")
    }

    pub fn stop(self) {
        todo!("engine agent")
    }
}

/// Whether the Screen & System Audio Recording permission is granted
/// (CGPreflightScreenCaptureAccess). Does not prompt.
pub fn has_permission() -> bool {
    todo!("engine agent")
}
