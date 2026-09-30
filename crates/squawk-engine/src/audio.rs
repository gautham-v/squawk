//! Mic capture (cpal), downmix, resampling to 16 kHz, and audio files.
//!
//! The mic is opened at the device's own rate and format (whatever
//! `default_input_config` says, usually 48 kHz f32), downmixed to mono, and
//! resampled to 16 kHz on the capture thread — never in the CoreAudio
//! callback, which only copies into a lock-free ring / channel.

use std::path::Path;

use crate::error::EngineError;

/// A running mic stream. The cpal stream lives on its own thread (cpal
/// streams are `!Send`); this handle is `Send`. Dropping it stops capture.
pub struct MicCapture {
    _private: (),
}

impl MicCapture {
    /// Open `device` (by name; `None` = system default) and deliver 16 kHz
    /// mono samples to `sink` in blocks of ~10–20 ms, on the capture thread.
    /// Returns once the stream is playing or has failed to open.
    pub fn start(
        device: Option<&str>,
        sink: impl FnMut(&[f32]) + Send + 'static,
    ) -> Result<MicCapture, EngineError> {
        let _ = (device, sink);
        todo!("engine agent")
    }

    /// The device actually opened.
    pub fn device_name(&self) -> &str {
        todo!("engine agent")
    }

    /// Stop and join the capture thread. Samples already delivered stay
    /// delivered; nothing arrives after this returns.
    pub fn stop(self) {
        todo!("engine agent")
    }
}

/// Names of the available input devices, default first.
pub fn input_devices() -> Vec<String> {
    todo!("engine agent")
}

/// Streaming resampler from any rate to 16 kHz mono.
pub struct Resampler {
    from_rate: u32,
}

impl Resampler {
    pub fn new(from_rate: u32) -> Resampler {
        Resampler { from_rate }
    }

    pub fn from_rate(&self) -> u32 {
        self.from_rate
    }

    /// Resample a block; may return fewer samples than a full ratio would
    /// suggest (the rest is held for the next block).
    pub fn process(&mut self, mono: &[f32]) -> Vec<f32> {
        let _ = mono;
        todo!("engine agent")
    }

    /// Whatever is held back.
    pub fn flush(&mut self) -> Vec<f32> {
        todo!("engine agent")
    }
}

/// Average interleaved channels to mono.
pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    let _ = (interleaved, channels);
    todo!("engine agent")
}

/// Any audio file macOS can read, as 16 kHz mono. WAV is read directly
/// (hound); anything else goes through `/usr/bin/afconvert` to a temp WAV.
pub fn load_file(path: &Path) -> Result<Vec<f32>, EngineError> {
    let _ = path;
    todo!("engine agent")
}

/// Write 16 kHz mono samples as a 16-bit WAV (for `keep_audio`).
pub fn write_wav(path: &Path, samples: &[f32]) -> Result<(), EngineError> {
    let _ = (path, samples);
    todo!("engine agent")
}
