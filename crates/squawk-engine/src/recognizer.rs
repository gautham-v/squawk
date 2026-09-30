//! The recognizer thread: the only owner of the model.
//!
//! One model instance (~700 MB resident) serves dictation and meetings.
//! Jobs queue by priority — every waiting dictation job runs before any
//! meeting chunk — and within a priority in submission order. A meeting
//! chunk already running is not interrupted; chunks are ≤ 35 s so the worst
//! wait is one chunk's inference (~1 s on an M4).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use crossbeam_channel::{select, Receiver, Sender};

use crate::error::EngineError;
use crate::model::{LoadedModel, Recognized};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Someone is waiting to paste.
    Dictation,
    /// Background meeting transcription.
    Meeting,
}

/// What the recognizer runs. [`LoadedModel`] in the app; a fake in tests.
pub trait Backend: Send {
    fn transcribe(&mut self, samples: &[f32]) -> Result<Recognized, EngineError>;
}

impl Backend for LoadedModel {
    fn transcribe(&mut self, samples: &[f32]) -> Result<Recognized, EngineError> {
        LoadedModel::transcribe(self, samples)
    }
}

pub type Reply = Receiver<Result<Recognized, EngineError>>;

struct Job {
    samples: Vec<f32>,
    reply: Sender<Result<Recognized, EngineError>>,
    /// Set by a cancelled dictation: skip the job instead of running it.
    cancelled: Option<Arc<AtomicBool>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LoadState {
    Loading,
    Ready,
    Failed { path: PathBuf, message: String },
}

impl LoadState {
    fn error(&self) -> Option<EngineError> {
        match self {
            LoadState::Failed { path, message } => Some(EngineError::ModelLoad {
                path: path.clone(),
                message: message.clone(),
            }),
            _ => None,
        }
    }
}

type LoadCell = Arc<(Mutex<LoadState>, Condvar)>;

/// Handle to the recognizer thread. Clone freely; the thread exits when the
/// last clone is dropped (after finishing queued jobs).
#[derive(Clone)]
pub struct Recognizer {
    dictation: Sender<Job>,
    meeting: Sender<Job>,
    load: LoadCell,
}

impl Recognizer {
    /// Spawn the thread and start loading the model from `model_dir`, then
    /// warm it up with one short inference. Jobs submitted before that
    /// finishes wait for it.
    pub fn spawn(model_dir: PathBuf, threads: usize) -> Recognizer {
        Recognizer::spawn_with(
            model_dir.clone(),
            parakeet_loader(model_dir, threads),
            |_| {},
        )
    }

    /// Spawn with any backend. `loader` runs on the new thread; `on_loaded`
    /// is called there once it returns (before any job runs).
    pub fn spawn_with(
        model_dir: PathBuf,
        loader: impl FnOnce() -> Result<Box<dyn Backend>, EngineError> + Send + 'static,
        on_loaded: impl FnOnce(&LoadState) + Send + 'static,
    ) -> Recognizer {
        let (dictation, dictation_rx) = crossbeam_channel::unbounded();
        let (meeting, meeting_rx) = crossbeam_channel::unbounded();
        let load: LoadCell = Arc::new((Mutex::new(LoadState::Loading), Condvar::new()));
        let cell = load.clone();
        let spawned = std::thread::Builder::new()
            .name("squawk-recognizer".into())
            .spawn(move || {
                let backend = match loader() {
                    Ok(b) => {
                        set_state(&cell, LoadState::Ready);
                        Some(b)
                    }
                    Err(e) => {
                        log::error!("model: {e}");
                        let message = match e {
                            EngineError::ModelLoad { message, .. } => message,
                            other => other.to_string(),
                        };
                        set_state(
                            &cell,
                            LoadState::Failed {
                                path: model_dir,
                                message,
                            },
                        );
                        None
                    }
                };
                on_loaded(&cell.0.lock().expect("load lock"));
                serve(backend, &cell, dictation_rx, meeting_rx);
            });
        if let Err(e) = spawned {
            set_state(
                &load,
                LoadState::Failed {
                    path: PathBuf::new(),
                    message: format!("could not start the recognizer thread: {e}"),
                },
            );
        }
        Recognizer {
            dictation,
            meeting,
            load,
        }
    }

    /// Queue 16 kHz mono samples. The answer arrives on the returned channel.
    pub fn submit(&self, samples: Vec<f32>, priority: Priority) -> Reply {
        self.submit_cancellable(samples, priority, None)
    }

    /// [`Recognizer::submit`], skipped (the reply channel just closes) if
    /// `cancelled` is set by the time the job's turn comes.
    pub fn submit_cancellable(
        &self,
        samples: Vec<f32>,
        priority: Priority,
        cancelled: Option<Arc<AtomicBool>>,
    ) -> Reply {
        let (reply, rx) = crossbeam_channel::bounded(1);
        let job = Job {
            samples,
            reply,
            cancelled,
        };
        let tx = match priority {
            Priority::Dictation => &self.dictation,
            Priority::Meeting => &self.meeting,
        };
        if let Err(e) = tx.send(job) {
            let _ = e.0.reply.send(Err(EngineError::ModelNotReady(
                "the recognizer stopped".into(),
            )));
        }
        rx
    }

    /// Current load state; never blocks.
    pub fn state(&self) -> LoadState {
        self.load.0.lock().expect("load lock").clone()
    }

    /// Block until the model is loaded (or failed to).
    pub fn wait_ready(&self) -> Result<(), EngineError> {
        let (lock, cvar) = &*self.load;
        let mut state = lock.lock().expect("load lock");
        while *state == LoadState::Loading {
            state = cvar.wait(state).expect("load lock");
        }
        match state.error() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Load Parakeet from `dir` and warm it up: the loader [`Recognizer::spawn`]
/// uses.
pub fn parakeet_loader(
    dir: PathBuf,
    threads: usize,
) -> impl FnOnce() -> Result<Box<dyn Backend>, EngineError> + Send + 'static {
    move || {
        let t = Instant::now();
        let mut model = LoadedModel::load(&dir, threads)?;
        let loaded = t.elapsed();
        model.warm_up()?;
        log::info!(
            "model: loaded in {} ms, warm-up {} ms",
            loaded.as_millis(),
            (t.elapsed() - loaded).as_millis()
        );
        Ok(Box::new(model) as Box<dyn Backend>)
    }
}

fn set_state(cell: &LoadCell, state: LoadState) {
    *cell.0.lock().expect("load lock") = state;
    cell.1.notify_all();
}

fn serve(
    mut backend: Option<Box<dyn Backend>>,
    cell: &LoadCell,
    dictation_rx: Receiver<Job>,
    meeting_rx: Receiver<Job>,
) {
    let mut dq: VecDeque<Job> = VecDeque::new();
    let mut mq: VecDeque<Job> = VecDeque::new();
    loop {
        if dq.is_empty() && mq.is_empty() {
            // Both senders live in every `Recognizer`, so they disconnect
            // together: one closed channel means we are done.
            select! {
                recv(dictation_rx) -> j => match j {
                    Ok(j) => dq.push_back(j),
                    Err(_) => return,
                },
                recv(meeting_rx) -> j => match j {
                    Ok(j) => mq.push_back(j),
                    Err(_) => return,
                },
            }
        }
        dq.extend(dictation_rx.try_iter());
        mq.extend(meeting_rx.try_iter());
        let Some(job) = dq.pop_front().or_else(|| mq.pop_front()) else {
            continue;
        };
        if job
            .cancelled
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            continue;
        }
        let result = match backend.as_mut() {
            Some(b) => b.transcribe(&job.samples),
            None => Err(cell
                .0
                .lock()
                .expect("load lock")
                .error()
                .unwrap_or(EngineError::ModelMissing)),
        };
        let _ = job.reply.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Records the first sample of every job it runs.
    struct Fake {
        log: Arc<Mutex<Vec<f32>>>,
    }

    impl Backend for Fake {
        fn transcribe(&mut self, samples: &[f32]) -> Result<Recognized, EngineError> {
            self.log.lock().unwrap().push(samples[0]);
            Ok(Recognized {
                text: format!("job {}", samples[0]),
                segments: vec![(0.0, 1.0, format!("job {}", samples[0]))],
            })
        }
    }

    /// A recognizer whose load blocks until `release` is sent.
    fn gated() -> (Recognizer, mpsc::Sender<()>, Arc<Mutex<Vec<f32>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (release, gate) = mpsc::channel::<()>();
        let l = log.clone();
        let r = Recognizer::spawn_with(
            PathBuf::from("/fake"),
            move || {
                gate.recv().unwrap();
                Ok(Box::new(Fake { log: l }) as Box<dyn Backend>)
            },
            |_| {},
        );
        (r, release, log)
    }

    #[test]
    fn dictation_jobs_run_before_meeting_jobs() {
        let (r, release, log) = gated();
        let replies = [
            r.submit(vec![10.0], Priority::Meeting),
            r.submit(vec![11.0], Priority::Meeting),
            r.submit(vec![1.0], Priority::Dictation),
            r.submit(vec![12.0], Priority::Meeting),
            r.submit(vec![2.0], Priority::Dictation),
        ];
        assert_eq!(r.state(), LoadState::Loading);
        release.send(()).unwrap();
        for rx in &replies {
            assert!(rx.recv().unwrap().is_ok());
        }
        assert_eq!(*log.lock().unwrap(), vec![1.0, 2.0, 10.0, 11.0, 12.0]);
        assert_eq!(r.state(), LoadState::Ready);
    }

    #[test]
    fn replies_carry_the_backend_result() {
        let (r, release, _) = gated();
        release.send(()).unwrap();
        r.wait_ready().unwrap();
        let got = r
            .submit(vec![7.0], Priority::Dictation)
            .recv()
            .unwrap()
            .unwrap();
        assert_eq!(got.text, "job 7");
    }

    #[test]
    fn cancelled_jobs_are_skipped() {
        let (r, release, log) = gated();
        let flag = Arc::new(AtomicBool::new(false));
        let a = r.submit_cancellable(vec![1.0], Priority::Dictation, Some(flag.clone()));
        let b = r.submit(vec![2.0], Priority::Dictation);
        flag.store(true, Ordering::Relaxed);
        release.send(()).unwrap();
        assert!(b.recv().unwrap().is_ok());
        assert!(a.recv().is_err(), "the reply channel closes unanswered");
        assert_eq!(*log.lock().unwrap(), vec![2.0]);
    }

    #[test]
    fn load_failure_fails_every_job() {
        let seen = Arc::new(Mutex::new(None));
        let s = seen.clone();
        let r = Recognizer::spawn_with(
            PathBuf::from("/models/x"),
            || {
                Err(EngineError::ModelLoad {
                    path: PathBuf::from("/models/x"),
                    message: "bad onnx".into(),
                })
            },
            move |st| *s.lock().unwrap() = Some(st.clone()),
        );
        assert!(matches!(r.wait_ready(), Err(EngineError::ModelLoad { .. })));
        let got = r.submit(vec![1.0], Priority::Meeting).recv().unwrap();
        match got {
            Err(EngineError::ModelLoad { message, .. }) => assert_eq!(message, "bad onnx"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            *seen.lock().unwrap(),
            Some(LoadState::Failed { .. })
        ));
    }

    #[test]
    fn thread_exits_when_handles_drop() {
        let (r, release, _) = gated();
        let weak = Arc::downgrade(&r.load);
        release.send(()).unwrap();
        r.wait_ready().unwrap();
        drop(r);
        for _ in 0..100 {
            if weak.upgrade().is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("recognizer thread still holds its state");
    }
}
