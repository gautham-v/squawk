//! "S1-mini" by "Superwhisper": downloading it, and the thread that runs it.
//!
//! One GGUF file (`squawk_core::config::CLEANUP_MODEL_FILE`, 462 MB), run by
//! llama.cpp on the GPU (Metal). What goes in and what comes out is
//! `squawk_core::normalize`; this is the plumbing:
//! - [`download`]: like Parakeet's (stream to `.part`, resume with Range,
//!   check size and SHA-256, rename), but one file and no tarball;
//! - [`Normalizer`]: handle to the **normalizer** thread, the only owner of
//!   the model (~0.5 GB of weights plus its context). A thread of its own, so
//!   a cleanup never waits behind Parakeet's queue and Parakeet never waits
//!   behind a cleanup. [`Normalizer::normalize`] gives up after
//!   `normalize::BUDGET` and cancels the job (checked every token), so a
//!   slow answer costs the dictation its cleanup, never its paste.
//!
//! The model must be freed before the process exits: llama.cpp's Metal
//! device aborts in its static destructor if GPU memory it tracks is still
//! allocated (a crash report on every quit). So every normalizer thread is
//! registered, and an `atexit` hook stops them and waits for each to drop
//! its model; the last handle going away does the same during a run.
//!
//! Decoding is greedy: normalization is deterministic, and S1-mini was
//! trained for it.

use std::fs::File;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use llama_cpp_2::context::params::{KvCacheType, LlamaContextParams};
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use llama_cpp_2::TokenToStringError;
use squawk_core::config::CLEANUP_MODEL_FILE;
use squawk_core::normalize::{self, NormalizeError};
use squawk_core::ModelStatus;

use crate::error::EngineError;
use crate::model;

/// Size and SHA-256 of the file at `CLEANUP_MODEL_URL` (a pinned revision).
pub const MODEL_SIZE: u64 = 484_219_808;
pub const MODEL_SHA256: &str = "3b41ebe2502cbd03e811d5d16b022f5ab551eda58d62597d152f89535003c634";

/// Tokens of context: the prompt (~60 tokens of template plus the
/// transcript, at most `MAX_INPUT_TOKENS`) and the answer.
const N_CTX: u32 = 2048;

/// `<models_dir>/s1-mini-q4_k_m.gguf`
pub fn model_path(models_dir: &Path) -> PathBuf {
    models_dir.join(CLEANUP_MODEL_FILE)
}

/// The whole file is there (a partial download is `….part`).
pub fn is_installed(models_dir: &Path) -> bool {
    std::fs::metadata(model_path(models_dir)).is_ok_and(|m| m.len() == MODEL_SIZE)
}

/// Download the model into `models_dir`, reporting `Downloading`, then
/// `Extracting` while the checksum is checked. Blocking. Already there:
/// returns at once, without the network.
pub fn download(
    url: &str,
    models_dir: &Path,
    progress: &mut dyn FnMut(ModelStatus),
) -> Result<PathBuf, EngineError> {
    let path = model_path(models_dir);
    if is_installed(models_dir) {
        return Ok(path);
    }
    std::fs::create_dir_all(models_dir)?;
    // The app and `squawk model download` may race, as for Parakeet.
    let lock = File::create(models_dir.join(format!("{CLEANUP_MODEL_FILE}.lock")))?;
    lock.lock()?;
    if is_installed(models_dir) {
        return Ok(path);
    }
    let part = models_dir.join(format!("{CLEANUP_MODEL_FILE}.part"));
    model::fetch(url, &part, Some(MODEL_SIZE), progress)?;
    progress(ModelStatus::Extracting);
    if let Err(e) = model::verify_sha256(&part, MODEL_SHA256) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &path)?;
    Ok(path)
}

/// Something that cleans a transcript: S1-mini through llama.cpp, or a fake
/// in tests. Lives on the normalizer thread; `cancel` is checked between
/// tokens.
pub trait Clean {
    fn clean(&mut self, transcript: &str, cancel: &AtomicBool) -> Result<String, NormalizeError>;
}

/// Where a normalizer thread is.
#[derive(Debug, Clone, PartialEq)]
pub enum LoadState {
    Loading,
    Ready,
    Failed(String),
}

type LoadCell = Arc<(Mutex<LoadState>, Condvar)>;

/// Hands a loaded cleaner to the thread's job loop (or says it failed).
pub type Serve<'s> = &'s mut dyn FnMut(Result<&mut dyn Clean, EngineError>);

struct Job {
    transcript: String,
    reply: Sender<Result<String, NormalizeError>>,
    cancel: Arc<AtomicBool>,
}

/// Handle to the normalizer thread. Clone freely; the thread exits (and
/// frees the model) when the last clone is dropped, or at process exit.
#[derive(Clone)]
pub struct Normalizer {
    jobs: Sender<Job>,
    load: LoadCell,
    budget: Duration,
}

impl Normalizer {
    /// Load S1-mini from `path` on a new thread, warm it up, then serve.
    pub fn spawn(path: PathBuf, on_loaded: impl FnOnce(&LoadState) + Send + 'static) -> Normalizer {
        Normalizer::spawn_with(s1_loader(path), normalize::BUDGET, on_loaded)
    }

    /// Spawn with any cleaner. `loader` runs on the new thread and calls
    /// `serve` with what it loaded; `serve` returns when the last handle is
    /// dropped. `on_loaded` runs there before the first job.
    pub fn spawn_with(
        loader: impl FnOnce(Serve) + Send + 'static,
        budget: Duration,
        on_loaded: impl FnOnce(&LoadState) + Send + 'static,
    ) -> Normalizer {
        let (jobs, rx) = crossbeam_channel::unbounded::<Job>();
        let (stop, stop_rx) = crossbeam_channel::bounded::<()>(1);
        let done = Done::default();
        let finished = done.clone();
        let load: LoadCell = Arc::new((Mutex::new(LoadState::Loading), Condvar::new()));
        let cell = load.clone();
        let spawned = std::thread::Builder::new()
            .name("squawk-normalizer".into())
            .spawn(move || {
                // Set only once the loader, and with it the model, is gone.
                let _done = SetOnDrop(finished);
                let mut on_loaded = Some(on_loaded);
                let mut served = false;
                loader(&mut |got| {
                    served = true;
                    match got {
                        Ok(cleaner) => {
                            set_state(&cell, LoadState::Ready);
                            if let Some(f) = on_loaded.take() {
                                f(&LoadState::Ready);
                            }
                            serve(cleaner, &rx, &stop_rx);
                        }
                        Err(e) => {
                            log::error!("cleanup model: {e}");
                            let state = LoadState::Failed(e.to_string());
                            set_state(&cell, state.clone());
                            if let Some(f) = on_loaded.take() {
                                f(&state);
                            }
                        }
                    }
                });
                if !served {
                    set_state(
                        &cell,
                        LoadState::Failed("the loader returned nothing".into()),
                    );
                }
            });
        match spawned {
            Ok(_) => register(Running { stop, done }),
            Err(e) => set_state(&load, LoadState::Failed(format!("could not start: {e}"))),
        }
        Normalizer { jobs, load, budget }
    }

    pub fn state(&self) -> LoadState {
        self.load.0.lock().expect("load lock").clone()
    }

    /// Block until loaded (or failed).
    pub fn wait_ready(&self) -> Result<(), EngineError> {
        let (lock, cvar) = &*self.load;
        let mut state = lock.lock().expect("load lock");
        while *state == LoadState::Loading {
            state = cvar.wait(state).expect("load lock");
        }
        match &*state {
            LoadState::Failed(message) => Err(EngineError::ModelLoad {
                path: PathBuf::from(CLEANUP_MODEL_FILE),
                message: message.clone(),
            }),
            _ => Ok(()),
        }
    }
}

impl normalize::Normalizer for Normalizer {
    /// `NotReady` until loaded; `Timeout` after the budget (the job is
    /// cancelled at its next token).
    fn normalize(&self, transcript: &str) -> Result<String, NormalizeError> {
        if self.state() != LoadState::Ready {
            return Err(NormalizeError::NotReady);
        }
        let (reply, answer) = crossbeam_channel::bounded(1);
        let cancel = Arc::new(AtomicBool::new(false));
        self.jobs
            .send(Job {
                transcript: transcript.to_string(),
                reply,
                cancel: cancel.clone(),
            })
            .map_err(|_| NormalizeError::Failed("the normalizer stopped".into()))?;
        match answer.recv_timeout(self.budget) {
            Ok(result) => result,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                cancel.store(true, Ordering::Release);
                Err(NormalizeError::Timeout)
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                Err(NormalizeError::Failed("the normalizer stopped".into()))
            }
        }
    }
}

fn set_state(cell: &LoadCell, state: LoadState) {
    *cell.0.lock().expect("load lock") = state;
    cell.1.notify_all();
}

/// Run jobs until every handle is gone or `stop` says so. A job whose
/// caller already gave up is skipped.
fn serve(cleaner: &mut dyn Clean, rx: &Receiver<Job>, stop: &Receiver<()>) {
    loop {
        let job = crossbeam_channel::select! {
            recv(rx) -> job => match job {
                Ok(job) => job,
                Err(_) => return,
            },
            recv(stop) -> _ => return,
        };
        if job.cancel.load(Ordering::Acquire) {
            continue;
        }
        let t = Instant::now();
        let result = cleaner.clean(&job.transcript, &job.cancel);
        log::debug!(
            "cleanup model: {} chars in {} ms",
            job.transcript.len(),
            t.elapsed().as_millis()
        );
        let _ = job.reply.send(result);
    }
}

/// A normalizer thread, as the exit hook sees it.
struct Running {
    stop: Sender<()>,
    done: Done,
}

/// Set when a normalizer thread has dropped its model. A plain condvar, not
/// a channel: the exit hook runs after thread-locals are gone, and channel
/// waits need them.
#[derive(Clone, Default)]
struct Done(Arc<(Mutex<bool>, Condvar)>);

impl Done {
    fn is_set(&self) -> bool {
        *self.0 .0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait(&self, timeout: Duration) {
        let (lock, cvar) = &*self.0;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = cvar.wait_timeout_while(guard, timeout, |done| !*done);
    }
}

/// Held by the thread; sets its [`Done`] when the thread function returns
/// (after the model was dropped), however it returns.
struct SetOnDrop(Done);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.0 .0;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
        cvar.notify_all();
    }
}

static RUNNING: Mutex<Vec<Running>> = Mutex::new(Vec::new());

/// How long the exit hook waits for one thread (a job in flight finishes
/// its current token first).
const EXIT_WAIT: Duration = Duration::from_secs(3);

fn register(running: Running) {
    static HOOK: OnceLock<()> = OnceLock::new();
    HOOK.get_or_init(|| {
        // SAFETY: registers a plain `extern "C" fn` with no preconditions.
        unsafe {
            libc::atexit(stop_all);
        }
    });
    let mut all = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    // Forget threads that have already finished.
    all.retain(|r| !r.done.is_set());
    all.push(running);
}

/// At exit: stop every normalizer thread and wait for its model to be
/// freed, before llama.cpp's own static destructors run. (Registered after
/// llama.cpp's statics were constructed, so it runs before they are
/// destroyed.)
extern "C" fn stop_all() {
    let all = std::mem::take(&mut *RUNNING.lock().unwrap_or_else(|e| e.into_inner()));
    for r in &all {
        let _ = r.stop.try_send(());
    }
    for r in &all {
        r.done.wait(EXIT_WAIT);
    }
}

/// llama.cpp may be initialised once per process.
fn backend() -> Result<&'static LlamaBackend, EngineError> {
    static BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();
    BACKEND
        .get_or_init(|| {
            // SAFETY: plain function pointers with no user data; set before
            // anything logs.
            unsafe {
                llama_cpp_sys_2::llama_log_set(Some(forward_log), std::ptr::null_mut());
                llama_cpp_sys_2::ggml_log_set(Some(forward_log), std::ptr::null_mut());
            }
            LlamaBackend::init().map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|message| EngineError::ModelLoad {
            path: PathBuf::from(CLEANUP_MODEL_FILE),
            message: message.clone(),
        })
}

/// llama.cpp's and ggml's log lines go to squawk's log instead of stderr:
/// warnings and errors as such, the chatter (device info, tensor loading) at
/// debug.
unsafe extern "C" fn forward_log(
    level: llama_cpp_sys_2::ggml_log_level,
    text: *const std::ffi::c_char,
    _user: *mut std::ffi::c_void,
) {
    if text.is_null() {
        return;
    }
    // SAFETY: llama.cpp passes a NUL-terminated string valid for the call.
    let text = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
    let text = text.trim_end();
    if text.is_empty() || text == "." {
        return;
    }
    match level {
        llama_cpp_sys_2::GGML_LOG_LEVEL_ERROR => log::error!(target: "llama", "{text}"),
        llama_cpp_sys_2::GGML_LOG_LEVEL_WARN => log::warn!(target: "llama", "{text}"),
        _ => log::debug!(target: "llama", "{text}"),
    }
}

/// Load the GGUF at `path` onto the GPU, warm it up, and serve.
pub fn s1_loader(path: PathBuf) -> impl FnOnce(Serve) + Send + 'static {
    move |serve| {
        let t = Instant::now();
        let loaded = S1Mini::load_and(&path, |s1| {
            let loaded_ms = t.elapsed().as_millis();
            // One short answer so Metal compiles its kernels now, not during
            // the first dictation.
            let _ = s1.clean("um, hello there", &AtomicBool::new(false));
            log::info!(
                "cleanup model: loaded in {loaded_ms} ms, warm-up {} ms",
                t.elapsed().as_millis() - loaded_ms
            );
            serve(Ok(s1));
        });
        if let Err(e) = loaded {
            serve(Err(e));
        }
    }
}

/// The model and one context on it. The context borrows the model, so both
/// live on the loader's stack for as long as the thread serves.
pub struct S1Mini<'m> {
    model: &'m LlamaModel,
    ctx: LlamaContext<'m>,
}

impl S1Mini<'_> {
    /// Load `path` and run `f` with it. Blocking.
    pub fn load_and<R>(path: &Path, f: impl FnOnce(&mut S1Mini) -> R) -> Result<R, EngineError> {
        let fail = |message: String| EngineError::ModelLoad {
            path: path.to_path_buf(),
            message,
        };
        if !path.is_file() {
            return Err(EngineError::ModelMissing);
        }
        let backend = backend()?;
        let params = LlamaModelParams::default().with_n_gpu_layers(999);
        let model =
            LlamaModel::load_from_file(backend, path, &params).map_err(|e| fail(e.to_string()))?;
        // q8_0 keys and values halve the context's memory (~120 MB at 2048
        // tokens) with no visible effect on this task.
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(N_CTX))
            .with_n_batch(N_CTX)
            .with_type_k(KvCacheType::Q8_0)
            .with_type_v(KvCacheType::Q8_0);
        let ctx = model
            .new_context(backend, ctx_params)
            .map_err(|e| fail(e.to_string()))?;
        let mut s1 = S1Mini { model: &model, ctx };
        Ok(f(&mut s1))
    }
}

impl Clean for S1Mini<'_> {
    fn clean(&mut self, transcript: &str, cancel: &AtomicBool) -> Result<String, NormalizeError> {
        let failed = |e: &dyn std::fmt::Display| NormalizeError::Failed(e.to_string());
        let own = self
            .model
            .str_to_token(transcript, AddBos::Never)
            .map_err(|e| failed(&e))?;
        if own.len() > normalize::MAX_INPUT_TOKENS {
            return Err(NormalizeError::TooLong);
        }
        let prompt = self
            .model
            .str_to_token(&normalize::chat_prompt(transcript), AddBos::Never)
            .map_err(|e| failed(&e))?;
        let room = (N_CTX as usize).saturating_sub(prompt.len());
        let max_new = normalize::max_new_tokens(own.len()).min(room);

        self.ctx.clear_kv_cache();
        let mut batch = LlamaBatch::new(prompt.len().max(1), 1);
        let last = prompt.len() as i32 - 1;
        for (i, token) in prompt.iter().enumerate() {
            batch
                .add(*token, i as i32, &[0], i as i32 == last)
                .map_err(|e| failed(&e))?;
        }
        self.ctx.decode(&mut batch).map_err(|e| failed(&e))?;

        let mut sampler = LlamaSampler::greedy();
        let mut out = Vec::new();
        let start = prompt.len() as i32;
        for pos in start..start + max_new as i32 {
            if cancel.load(Ordering::Acquire) {
                return Err(NormalizeError::Timeout);
            }
            let token = sampler.sample(&self.ctx, batch.n_tokens() - 1);
            sampler.accept(token);
            if self.model.is_eog_token(token) {
                break;
            }
            out.extend(self.piece(token).map_err(|e| failed(&e))?);
            batch.clear();
            batch.add(token, pos, &[0], true).map_err(|e| failed(&e))?;
            self.ctx.decode(&mut batch).map_err(|e| failed(&e))?;
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

impl S1Mini<'_> {
    /// The bytes of one token (a multi-byte character may span tokens, so
    /// the text is decoded once, at the end).
    fn piece(&self, token: LlamaToken) -> Result<Vec<u8>, TokenToStringError> {
        match self.model.token_to_piece_bytes(token, 32, false, None) {
            Err(TokenToStringError::InsufficientBufferSpace(need)) => self
                .model
                .token_to_piece_bytes(token, need.unsigned_abs() as usize, false, None),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use squawk_core::normalize::Normalizer as _;

    /// Upper-cases after an optional delay, watching `cancel`.
    struct Upper {
        delay: Duration,
    }

    impl Clean for Upper {
        fn clean(&mut self, t: &str, cancel: &AtomicBool) -> Result<String, NormalizeError> {
            let until = Instant::now() + self.delay;
            while Instant::now() < until {
                if cancel.load(Ordering::Acquire) {
                    return Err(NormalizeError::Timeout);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(t.to_uppercase())
        }
    }

    fn upper(delay: Duration, budget: Duration) -> Normalizer {
        Normalizer::spawn_with(move |serve| serve(Ok(&mut Upper { delay })), budget, |_| {})
    }

    #[test]
    fn answers_once_loaded() {
        let n = upper(Duration::ZERO, Duration::from_secs(5));
        n.wait_ready().unwrap();
        assert_eq!(n.normalize("send it").unwrap(), "SEND IT");
        assert_eq!(n.normalize("again").unwrap(), "AGAIN");
    }

    #[test]
    fn too_slow_is_a_timeout_and_the_next_job_still_runs() {
        let n = upper(Duration::from_millis(300), Duration::from_millis(50));
        n.wait_ready().unwrap();
        let t = Instant::now();
        assert_eq!(n.normalize("slow"), Err(NormalizeError::Timeout));
        assert!(t.elapsed() < Duration::from_millis(250));
        // The cancelled job stops early instead of holding the thread.
        let n2 = Normalizer {
            budget: Duration::from_secs(5),
            ..n.clone()
        };
        let t = Instant::now();
        assert_eq!(n2.normalize("next").unwrap(), "NEXT");
        assert!(
            t.elapsed() < Duration::from_millis(900),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn a_failed_load_is_never_asked() {
        let n = Normalizer::spawn_with(
            |serve| serve(Err(EngineError::ModelMissing)),
            Duration::from_secs(1),
            |_| {},
        );
        assert!(n.wait_ready().is_err());
        assert!(matches!(n.state(), LoadState::Failed(_)));
        assert_eq!(n.normalize("x"), Err(NormalizeError::NotReady));
    }

    #[test]
    fn not_ready_while_loading() {
        let (go, wait) = crossbeam_channel::bounded::<()>(0);
        let n = Normalizer::spawn_with(
            move |serve| {
                let _ = wait.recv();
                serve(Ok(&mut Upper {
                    delay: Duration::ZERO,
                }))
            },
            Duration::from_secs(1),
            |_| {},
        );
        assert_eq!(n.normalize("x"), Err(NormalizeError::NotReady));
        go.send(()).unwrap();
        n.wait_ready().unwrap();
        assert_eq!(n.normalize("x").unwrap(), "X");
    }

    #[test]
    fn a_missing_file_is_not_installed_and_needs_no_network_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_installed(tmp.path()));
        std::fs::write(model_path(tmp.path()), b"short").unwrap();
        assert!(
            !is_installed(tmp.path()),
            "a truncated file is not the model"
        );
    }

    /// The real model, from `SQUAWK_S1_MINI` (a path to the GGUF):
    /// `SQUAWK_S1_MINI=… cargo test -p squawk-engine --release -- --ignored s1_mini`
    #[test]
    #[ignore]
    fn s1_mini_resolves_a_self_correction() {
        let path = PathBuf::from(std::env::var("SQUAWK_S1_MINI").expect("SQUAWK_S1_MINI"));
        let n = Normalizer::spawn(path, |_| {});
        n.wait_ready().unwrap();
        let t = Instant::now();
        let out = n
            .normalize(
                "so um i need to like send the the report by uh friday no wait make that thursday",
            )
            .unwrap();
        eprintln!("{out:?} in {} ms", t.elapsed().as_millis());
        let out = normalize::tidy_output(&out);
        assert!(
            out == "I need to send the report by Thursday."
                || out == "So I need to send the report by Thursday.",
            "{out}"
        );
        assert_eq!(
            n.normalize("Um. Uh.").map(|s| normalize::tidy_output(&s)),
            Ok(String::new())
        );
    }
}
