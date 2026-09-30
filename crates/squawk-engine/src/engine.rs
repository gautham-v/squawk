//! The one object the app and the CLI hold.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use squawk_core::config::{MeetingConfig, ModelConfig};
use squawk_core::{Config, ModelStatus, Paths};

use crate::dictation::DictationSession;
use crate::error::EngineError;
use crate::meeting::{MeetingHandle, MeetingOptions};

/// What the engine needs from the config, resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineConfig {
    pub paths: Paths,
    pub model: ModelConfig,
    /// `None` is the system default input.
    pub input_device: Option<String>,
    /// Write a WAV per dictation/meeting under `paths.audio_dir`.
    pub keep_audio: bool,
    pub meeting: MeetingConfig,
    /// A dictation is cut off (finished normally) after this long.
    pub max_dictation: Duration,
}

impl EngineConfig {
    pub fn from_config(paths: &Paths, config: &Config) -> EngineConfig {
        let device = config.dictation.input_device.trim();
        EngineConfig {
            paths: paths.clone(),
            model: config.model.clone(),
            input_device: (!device.is_empty()).then(|| device.to_string()),
            keep_audio: config.keep_audio,
            meeting: config.meeting.clone(),
            max_dictation: Duration::from_secs(config.dictation.max_secs),
        }
    }

    /// `<models_dir>/<model.dir>`
    pub fn model_dir(&self) -> PathBuf {
        self.paths.models_dir.join(&self.model.dir)
    }
}

/// Things the engine reports without being asked. Delivered on engine
/// threads through the sink set with [`Engine::set_event_sink`]; the sink
/// must not block.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// The model's status changed (download progress at most ~4 times a
    /// second).
    Model(ModelStatus),
    /// A running dictation hit `max_dictation`; the session finishes itself
    /// and the app should call `finish` as if fn were released.
    DictationTooLong,
    /// The meeting file was rewritten with new transcript.
    MeetingProgress { path: PathBuf, elapsed_secs: u64 },
    /// Something went wrong in a meeting that does not stop it (e.g. system
    /// audio unavailable: the meeting continues mic-only).
    MeetingWarning(String),
    /// The input device disappeared mid-recording.
    MicLost(String),
}

type Sink = Arc<dyn Fn(EngineEvent) + Send + Sync>;

/// Cheap to clone; all clones share one model and one recognizer thread.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

struct Inner {
    config: std::sync::Mutex<EngineConfig>,
    sink: std::sync::Mutex<Option<Sink>>,
}

impl Engine {
    /// Build the engine. Cheap: does not touch the model or the mic. Call
    /// [`Engine::ensure_model`] next.
    pub fn new(config: EngineConfig) -> Engine {
        Engine {
            inner: Arc::new(Inner {
                config: std::sync::Mutex::new(config),
                sink: std::sync::Mutex::new(None),
            }),
        }
    }

    /// Where to send [`EngineEvent`]s. Replaces any previous sink.
    pub fn set_event_sink(&self, sink: impl Fn(EngineEvent) + Send + Sync + 'static) {
        *self.inner.sink.lock().expect("sink lock") = Some(Arc::new(sink));
    }

    /// Current model status. Never blocks.
    pub fn model_status(&self) -> ModelStatus {
        todo!("engine agent")
    }

    /// Make the model ready in the background: download and unpack it if it
    /// is missing, then load it. Idempotent: a second call while one is in
    /// flight does nothing; after a failure it retries. Progress arrives as
    /// `EngineEvent::Model`.
    pub fn ensure_model(&self) {
        todo!("engine agent")
    }

    /// Load the model on this thread and return once it is ready (the CLI's
    /// `transcribe`). `ModelMissing` if it is not on disk; never downloads.
    pub fn load_model_blocking(&self) -> Result<(), EngineError> {
        todo!("engine agent")
    }

    /// Open the mic and start a streaming dictation. Returns as soon as the
    /// input stream is requested (it does not wait for the first buffer), so
    /// it is safe to call straight from the fn-down handler. Works while the
    /// model is still loading: audio buffers, and `finish` waits for the
    /// model. `Busy` if a dictation is already running; `ModelMissing` if the
    /// model is not on disk.
    pub fn start_dictation(&self) -> Result<DictationSession, EngineError> {
        todo!("engine agent")
    }

    /// Start recording a meeting into `opts.path`. `Busy` if one is running.
    /// Runs alongside dictation: both share the recognizer, dictation first.
    pub fn start_meeting(&self, opts: MeetingOptions) -> Result<MeetingHandle, EngineError> {
        let _ = opts;
        todo!("engine agent")
    }

    /// Transcribe 16 kHz mono samples on the recognizer at dictation
    /// priority; blocks. Raw model text: no cleanup. Long input is cut into
    /// ≤ 30 s pieces at pauses and joined.
    pub fn transcribe(&self, samples: &[f32]) -> Result<String, EngineError> {
        let _ = samples;
        todo!("engine agent")
    }

    /// Apply a reloaded config. Takes effect for the next dictation/meeting;
    /// a changed model dir or URL triggers `ensure_model` again.
    pub fn update_config(&self, config: EngineConfig) {
        *self.inner.config.lock().expect("config lock") = config;
    }

    pub fn config(&self) -> EngineConfig {
        self.inner.config.lock().expect("config lock").clone()
    }
}
