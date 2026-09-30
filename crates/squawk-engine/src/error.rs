use std::path::PathBuf;

/// Everything the engine can fail with. Messages are phrased for the popover
/// header and the CLI: short, and saying what to do where there is something
/// to do.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The model is not on disk. `squawk model download`, or the app
    /// downloads it on first run.
    #[error("the speech model is not downloaded")]
    ModelMissing,

    /// The model is on disk but not loaded yet (or loading failed).
    #[error("the speech model is not ready: {0}")]
    ModelNotReady(String),

    #[error("could not load the speech model from {path}: {message}")]
    ModelLoad { path: PathBuf, message: String },

    #[error("model download failed: {0}")]
    Download(String),

    /// No input device, the named device is gone, or the stream failed.
    #[error("microphone: {0}")]
    Mic(String),

    /// ScreenCaptureKit refused or failed; usually the Screen & System Audio
    /// Recording permission.
    #[error("system audio: {0}")]
    SystemAudio(String),

    #[error("transcription failed: {0}")]
    Transcribe(String),

    /// A dictation or meeting is already running where only one may be.
    #[error("{0} is already running")]
    Busy(&'static str),

    /// An audio file `load_file` could not read or convert.
    #[error("could not read audio {path}: {message}")]
    AudioFile { path: PathBuf, message: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Core(#[from] squawk_core::Error),
}
