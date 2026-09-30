//! The one object the app and the CLI hold.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use squawk_core::config::{MeetingConfig, ModelConfig};
use squawk_core::{Config, ModelStatus, Paths};

use crate::dictation::{self, DictationSession, Input};
use crate::error::EngineError;
use crate::meeting::{MeetingHandle, MeetingOptions};
use crate::model;
use crate::recognizer::{LoadState, Priority, Recognizer};
use crate::segmenter::{Segmenter, SegmenterConfig};
use crate::SAMPLE_RATE;

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

    fn audio_dir(&self) -> Option<PathBuf> {
        self.keep_audio.then(|| self.paths.audio_dir.clone())
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
    /// The input device disappeared mid-recording (or could not be opened).
    MicLost(String),
}

type Sink = Arc<dyn Fn(EngineEvent) + Send + Sync>;
/// How engine threads report events: looks the sink up at call time, so a
/// sink set after a session started still hears from it.
pub(crate) type Emit = Arc<dyn Fn(EngineEvent) + Send + Sync>;

/// Holds a "one at a time" flag while alive.
pub(crate) struct BusyGuard(Arc<AtomicBool>);

impl BusyGuard {
    pub(crate) fn acquire(
        flag: &Arc<AtomicBool>,
        what: &'static str,
    ) -> Result<BusyGuard, EngineError> {
        if flag.swap(true, Ordering::AcqRel) {
            return Err(EngineError::Busy(what));
        }
        Ok(BusyGuard(flag.clone()))
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Cheap to clone; all clones share one model and one recognizer thread.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

struct Inner {
    config: Mutex<EngineConfig>,
    sink: Mutex<Option<Sink>>,
    status: Mutex<ModelStatus>,
    /// The recognizer and the model dir it loads.
    recognizer: Mutex<Option<(PathBuf, Recognizer)>>,
    ensuring: AtomicBool,
    dictating: Arc<AtomicBool>,
    meeting: Arc<AtomicBool>,
}

impl Inner {
    fn emit(&self, event: EngineEvent) {
        let sink = self.sink.lock().expect("sink lock").clone();
        if let Some(sink) = sink {
            sink(event);
        }
    }

    fn set_status(&self, status: ModelStatus) {
        let mut cur = self.status.lock().expect("status lock");
        if *cur == status {
            return;
        }
        *cur = status.clone();
        drop(cur);
        self.emit(EngineEvent::Model(status));
    }
}

impl Engine {
    /// Build the engine. Cheap: does not touch the model or the mic. Call
    /// [`Engine::ensure_model`] next.
    pub fn new(config: EngineConfig) -> Engine {
        let status = if model::is_installed(&config.model_dir()) {
            ModelStatus::Loading
        } else {
            ModelStatus::Missing
        };
        Engine {
            inner: Arc::new(Inner {
                config: Mutex::new(config),
                sink: Mutex::new(None),
                status: Mutex::new(status),
                recognizer: Mutex::new(None),
                ensuring: AtomicBool::new(false),
                dictating: Arc::new(AtomicBool::new(false)),
                meeting: Arc::new(AtomicBool::new(false)),
            }),
        }
    }

    /// Where to send [`EngineEvent`]s. Replaces any previous sink.
    pub fn set_event_sink(&self, sink: impl Fn(EngineEvent) + Send + Sync + 'static) {
        *self.inner.sink.lock().expect("sink lock") = Some(Arc::new(sink));
    }

    fn emitter(&self) -> Emit {
        let weak: Weak<Inner> = Arc::downgrade(&self.inner);
        Arc::new(move |event| {
            if let Some(inner) = weak.upgrade() {
                inner.emit(event);
            }
        })
    }

    /// Current model status. Never blocks.
    pub fn model_status(&self) -> ModelStatus {
        self.inner.status.lock().expect("status lock").clone()
    }

    /// Make the model ready in the background: download and unpack it if it
    /// is missing, then load it. Idempotent: a second call while one is in
    /// flight does nothing; after a failure it retries. Progress arrives as
    /// `EngineEvent::Model`.
    pub fn ensure_model(&self) {
        if self.inner.ensuring.swap(true, Ordering::AcqRel) {
            return;
        }
        let engine = self.clone();
        let spawned = std::thread::Builder::new()
            .name("squawk-model".into())
            .spawn(move || {
                engine.ensure_model_blocking();
                engine.inner.ensuring.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            self.inner.ensuring.store(false, Ordering::Release);
        }
    }

    fn ensure_model_blocking(&self) {
        let config = self.config();
        let dir = config.model_dir();
        if !model::is_installed(&dir) {
            let inner = self.inner.clone();
            let got = model::download(
                &config.model.url,
                &config.paths.models_dir,
                &config.model.dir,
                &mut |s| inner.set_status(s),
            );
            if let Err(e) = got {
                log::error!("model: {e}");
                self.inner.set_status(ModelStatus::Failed {
                    message: e.to_string(),
                });
                return;
            }
            log::info!("model: installed at {}", dir.display());
        }
        match self.recognizer() {
            Ok(r) => {
                let _ = r.wait_ready();
            }
            Err(e) => self.inner.set_status(ModelStatus::Failed {
                message: e.to_string(),
            }),
        }
    }

    /// The recognizer for the configured model, spawning it (and so loading
    /// the model) if there is none or the last load failed.
    fn recognizer(&self) -> Result<Recognizer, EngineError> {
        let config = self.config();
        let dir = config.model_dir();
        let mut slot = self.inner.recognizer.lock().expect("recognizer lock");
        if let Some((d, r)) = slot.as_ref() {
            let failed = matches!(r.state(), LoadState::Failed { .. });
            if *d == dir && !failed {
                return Ok(r.clone());
            }
        }
        if !model::is_installed(&dir) {
            return Err(EngineError::ModelMissing);
        }
        self.inner.set_status(ModelStatus::Loading);
        let weak = Arc::downgrade(&self.inner);
        let threads = config.model.threads;
        let r = Recognizer::spawn_with(
            dir.clone(),
            crate::recognizer::parakeet_loader(dir.clone(), threads),
            move |state| {
                let Some(inner) = weak.upgrade() else { return };
                inner.set_status(match state {
                    LoadState::Ready => ModelStatus::Ready,
                    LoadState::Failed { message, .. } => ModelStatus::Failed {
                        message: message.clone(),
                    },
                    LoadState::Loading => ModelStatus::Loading,
                });
            },
        );
        *slot = Some((dir, r.clone()));
        Ok(r)
    }

    /// Load the model on this thread and return once it is ready (the CLI's
    /// `transcribe`). `ModelMissing` if it is not on disk; never downloads.
    pub fn load_model_blocking(&self) -> Result<(), EngineError> {
        self.recognizer()?.wait_ready()
    }

    /// Open the mic and start a streaming dictation. Returns as soon as the
    /// input stream is requested (it does not wait for the first buffer), so
    /// it is safe to call straight from the fn-down handler. Works while the
    /// model is still loading: audio buffers, and `finish` waits for the
    /// model. `Busy` if a dictation is already running; `ModelMissing` if the
    /// model is not on disk (`ModelNotReady` while it is downloading).
    pub fn start_dictation(&self) -> Result<DictationSession, EngineError> {
        let device = self.config().input_device;
        self.start_dictation_from(Input::Mic(device))
    }

    /// A dictation fed from `samples` (16 kHz mono) instead of the mic,
    /// delivered in 20 ms blocks on a real-time schedule sped up by `speed`
    /// (1.0 = real time). The same segmenting, recognizer and `finish` path
    /// as the mic, for benchmarks and end-to-end tests.
    pub fn start_dictation_replay(
        &self,
        samples: Vec<f32>,
        speed: f32,
    ) -> Result<DictationSession, EngineError> {
        self.start_dictation_from(Input::Replay { samples, speed })
    }

    fn start_dictation_from(&self, input: Input) -> Result<DictationSession, EngineError> {
        let busy = BusyGuard::acquire(&self.inner.dictating, "a dictation")?;
        let recognizer = self.recognizer().map_err(|e| self.not_ready(e))?;
        let config = self.config();
        Ok(DictationSession::start(
            recognizer,
            input,
            config.max_dictation,
            config.audio_dir(),
            self.emitter(),
            busy,
        ))
    }

    /// `ModelMissing` while downloading reads better as "not ready".
    fn not_ready(&self, e: EngineError) -> EngineError {
        match (e, self.model_status()) {
            (
                EngineError::ModelMissing,
                s @ (ModelStatus::Downloading { .. } | ModelStatus::Extracting),
            ) => EngineError::ModelNotReady(s.label()),
            (e, _) => e,
        }
    }

    /// Start recording a meeting into `opts.path`. `Busy` if one is running.
    /// Runs alongside dictation: both share the recognizer, dictation first.
    pub fn start_meeting(&self, opts: MeetingOptions) -> Result<MeetingHandle, EngineError> {
        let busy = BusyGuard::acquire(&self.inner.meeting, "a meeting")?;
        let recognizer = self.recognizer().map_err(|e| self.not_ready(e))?;
        MeetingHandle::start(opts, self.config(), recognizer, self.emitter(), busy)
    }

    /// Transcribe 16 kHz mono samples on the recognizer at dictation
    /// priority; blocks. Raw model text: no cleanup. Long input is cut into
    /// ≤ 30 s pieces at pauses and joined.
    pub fn transcribe(&self, samples: &[f32]) -> Result<String, EngineError> {
        let recognizer = self.recognizer()?;
        let replies: Vec<_> = split_long(samples)
            .into_iter()
            .map(|piece| recognizer.submit(piece, Priority::Dictation))
            .collect();
        let mut parts = Vec::with_capacity(replies.len());
        for rx in replies {
            let got = rx
                .recv()
                .map_err(|_| EngineError::Transcribe("the recognizer stopped".into()))??;
            parts.push(got.text);
        }
        Ok(dictation::join_segments(&parts))
    }

    /// Apply a reloaded config. Takes effect for the next dictation/meeting;
    /// a changed model dir or URL triggers `ensure_model` again.
    pub fn update_config(&self, config: EngineConfig) {
        let model_changed = {
            let mut cur = self.inner.config.lock().expect("config lock");
            let changed =
                cur.model != config.model || cur.paths.models_dir != config.paths.models_dir;
            *cur = config;
            changed
        };
        if model_changed {
            *self.inner.recognizer.lock().expect("recognizer lock") = None;
            self.ensure_model();
        }
    }

    pub fn config(&self) -> EngineConfig {
        self.inner.config.lock().expect("config lock").clone()
    }
}

/// Pieces of at most ~30 s, cut at pauses, silent ones dropped. Short input
/// is one piece as is.
fn split_long(samples: &[f32]) -> Vec<Vec<f32>> {
    const MAX_SECS: f32 = 30.0;
    if samples.len() as f32 <= MAX_SECS * SAMPLE_RATE as f32 {
        return vec![samples.to_vec()];
    }
    let mut seg = Segmenter::new(SegmenterConfig {
        min_segment: 15.0,
        max_segment: MAX_SECS - 2.0,
        pause: 0.3,
        ..SegmenterConfig::default()
    });
    let mut pieces: Vec<Vec<f32>> = seg.push(samples).into_iter().map(|c| c.samples).collect();
    if let Some(tail) = seg.finish().filter(|t| t.has_speech) {
        pieces.push(tail.samples);
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_in(root: &std::path::Path) -> Engine {
        let paths = Paths::under(root);
        Engine::new(EngineConfig::from_config(&paths, &Config::default()))
    }

    #[test]
    fn missing_model_is_reported_not_downloaded() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine_in(tmp.path());
        assert_eq!(e.model_status(), ModelStatus::Missing);
        assert!(matches!(
            e.load_model_blocking(),
            Err(EngineError::ModelMissing)
        ));
        assert!(matches!(
            e.start_dictation(),
            Err(EngineError::ModelMissing)
        ));
        // A failed start does not leave the busy flag set.
        assert!(matches!(
            e.start_dictation(),
            Err(EngineError::ModelMissing)
        ));
        assert!(matches!(
            e.transcribe(&[0.0; 100]),
            Err(EngineError::ModelMissing)
        ));
    }

    #[test]
    fn config_from_core_config() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path());
        let mut c = Config::default();
        c.dictation.input_device = "  ".into();
        c.dictation.max_secs = 42;
        let ec = EngineConfig::from_config(&paths, &c);
        assert_eq!(ec.input_device, None);
        assert_eq!(ec.max_dictation, Duration::from_secs(42));
        assert_eq!(ec.model_dir(), paths.models_dir.join(&c.model.dir));
        assert_eq!(ec.audio_dir(), None);
        c.dictation.input_device = "USB Mic".into();
        c.keep_audio = true;
        let ec = EngineConfig::from_config(&paths, &c);
        assert_eq!(ec.input_device.as_deref(), Some("USB Mic"));
        assert_eq!(ec.audio_dir(), Some(paths.audio_dir.clone()));
    }

    #[test]
    fn busy_guard_is_exclusive_until_dropped() {
        let flag = Arc::new(AtomicBool::new(false));
        let g = BusyGuard::acquire(&flag, "a meeting").unwrap();
        assert!(matches!(
            BusyGuard::acquire(&flag, "a meeting"),
            Err(EngineError::Busy("a meeting"))
        ));
        drop(g);
        assert!(BusyGuard::acquire(&flag, "a meeting").is_ok());
    }

    #[test]
    fn status_changes_reach_the_sink_once() {
        let tmp = tempfile::tempdir().unwrap();
        let e = engine_in(tmp.path());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        e.set_event_sink(move |ev| s.lock().unwrap().push(ev));
        e.inner.set_status(ModelStatus::Extracting);
        e.inner.set_status(ModelStatus::Extracting);
        e.inner.set_status(ModelStatus::Loading);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                EngineEvent::Model(ModelStatus::Extracting),
                EngineEvent::Model(ModelStatus::Loading)
            ]
        );
    }

    #[test]
    fn long_input_is_split_under_thirty_seconds() {
        let secs = |n: usize| n as f32 / 16_000.0;
        let short = vec![0.1f32; 16_000 * 10];
        assert_eq!(split_long(&short).len(), 1);
        // 70 s of "speech" with a pause every 7 s.
        let mut long = Vec::new();
        for _ in 0..10 {
            long.extend((0..16_000 * 7).map(|i| (i as f32 * 0.05).sin() * 0.2));
            long.extend(vec![0.0f32; 8000]);
        }
        let pieces = split_long(&long);
        assert!(pieces.len() >= 3);
        assert!(pieces.iter().all(|p| secs(p.len()) <= 30.0));
        let total: usize = pieces.iter().map(Vec::len).sum();
        assert!(total as f32 >= long.len() as f32 * 0.9);
    }
}
