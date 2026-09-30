//! squawk-engine: audio in, text out.
//!
//! The public API is described in docs/design.md ("squawk-engine").
//!
//! Threads (none of them the caller's):
//! - one **recognizer** thread owns the single Parakeet model and serves a
//!   priority queue: dictation jobs before meeting chunks;
//! - each [`DictationSession`] owns a **capture** thread holding the cpal
//!   input stream (cpal streams are `!Send`) and a **segmenter** that cuts
//!   committed speech at pauses and submits it while the user is talking;
//! - a meeting owns two capture threads (mic via cpal, or echo-cancelled
//!   through voice processing; system audio via
//!   ScreenCaptureKit) and a **writer** thread that merges finished chunks
//!   and rewrites the meeting file.
//!
//! Every public type is `Send`; [`Engine`] is also `Clone + Sync` (an `Arc`
//! inside), so the app can keep one and use it from any thread.

pub mod audio;
pub mod dictation;
mod engine;
mod error;
pub mod meeting;
pub mod model;
pub mod recognizer;
pub mod segmenter;
pub mod system_audio;
mod voice_processing;

pub use dictation::{DictationSession, Transcript};
pub use engine::{Engine, EngineConfig, EngineEvent};
pub use error::EngineError;
pub use meeting::{MeetingHandle, MeetingOptions, MeetingResult};

/// What the model eats: 16 kHz mono f32 in [-1, 1].
pub const SAMPLE_RATE: u32 = 16_000;

#[cfg(test)]
mod tests {
    use super::*;

    fn send<T: Send>() {}
    fn send_sync<T: Send + Sync>() {}

    #[test]
    fn public_types_cross_threads() {
        send_sync::<Engine>();
        send::<DictationSession>();
        send::<MeetingHandle>();
        send::<audio::MicCapture>();
        send::<system_audio::SystemAudioCapture>();
        send_sync::<recognizer::Recognizer>();
    }
}
