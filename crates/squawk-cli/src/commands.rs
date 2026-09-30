//! Every subcommand but the TUI. Output is plain text for people (and for
//! Claude reading it); `--json` where a script would want it. The shapes
//! live in `format.rs`; this file does the reading, asking and printing.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use squawk_core::cleanup::CleanupOptions;
use squawk_core::config::CLEANUP_MODEL_URL;
use squawk_core::context::{Agent, Context, RepoVocab, Session};
use squawk_core::ipc::{self, Request, Response};
use squawk_core::normalize::Normalizer;
use squawk_core::{dictionary, pipeline, Config, Dictionary, ModelStatus, Paths, Store};
use squawk_engine::{model, normalizer, Engine, EngineConfig};

use crate::format;

/// What `squawk` says when the command needs the app and it is not there.
pub const NOT_RUNNING: &str = "squawk is not running. Open Squawk.app first.";

/// A failure with its own exit code and message, printed as is (no
/// "Error:" prefix). Everything else exits 1 with its message.
#[derive(Debug)]
pub struct Exit {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Exit {}

fn exit(code: i32, message: impl Into<String>) -> anyhow::Error {
    Exit {
        code,
        message: message.into(),
    }
    .into()
}

/// Paths, config and store, resolved once per run.
pub struct Env {
    pub paths: Paths,
    pub config: Config,
    /// A malformed config file; printed as a warning by commands that care.
    pub config_note: Option<String>,
    pub store: Store,
}

impl Env {
    pub fn load() -> anyhow::Result<Env> {
        let paths = Paths::detect()?;
        let (config, config_note) = Config::load(&paths.config_file);
        let paths = paths.with_config(&config);
        Ok(Env::new(paths, config, config_note))
    }

    pub fn new(paths: Paths, config: Config, config_note: Option<String>) -> Env {
        let store = Store::new(&paths);
        Env {
            paths,
            config,
            config_note,
            store,
        }
    }

    fn engine_config(&self) -> EngineConfig {
        EngineConfig::from_config(&self.paths, &self.config)
    }

    /// A malformed config falls back to defaults; say so on stderr for the
    /// commands whose behaviour the config changes.
    fn warn_config(&self) {
        if let Some(note) = &self.config_note {
            eprintln!("warning: {note}");
        }
    }
}

/// Write to stdout; a closed pipe (`squawk history | head`) is not an error.
fn emit(text: &str) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|_| out.flush()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => Ok(other?),
    }
}

/// One request to the running app. Not running → exit 2 with the message.
fn ask(env: &Env, request: &Request, timeout: Duration) -> anyhow::Result<Response> {
    // no socket file is simply "not running" (also covers a support dir
    // whose socket path would be too long for the app to bind at all)
    if !env.paths.socket.exists() {
        return Err(exit(2, NOT_RUNNING));
    }
    match ipc::send(&env.paths.socket, request, timeout) {
        Ok(Response::Error { message }) => Err(exit(1, message)),
        Ok(r) => Ok(r),
        Err(squawk_core::Error::NotRunning(_)) => Err(exit(2, NOT_RUNNING)),
        Err(e) => Err(e.into()),
    }
}

fn unexpected(r: Response) -> anyhow::Error {
    anyhow::anyhow!("unexpected answer from squawk: {r:?}")
}

pub fn last(env: &Env) -> anyhow::Result<()> {
    match env.store.last()? {
        Some(e) => emit(&format!("{}\n", e.text)),
        None => Err(exit(1, "No dictations yet.")),
    }
}

pub fn history(env: &Env, today: bool, n: usize, json: bool) -> anyhow::Result<()> {
    let entries = if today {
        let mut day = env.store.day(chrono::Local::now().date_naive())?;
        day.reverse();
        day.truncate(n);
        day
    } else {
        env.store.recent(n)?
    };
    if json {
        emit(&format::history_json(&entries))
    } else {
        emit(&format::history(&entries))
    }
}

pub fn meet_start(env: &Env, title: Option<String>) -> anyhow::Result<()> {
    let title = title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    match ask(env, &Request::MeetStart { title }, ipc::DEFAULT_TIMEOUT)? {
        Response::MeetingStarted(info) => emit(&format::meeting_started(&info)),
        r => Err(unexpected(r)),
    }
}

pub fn meet_stop(env: &Env) -> anyhow::Result<()> {
    // fail fast when the app is not there, before promising to wait
    ask(env, &Request::Ping, ipc::DEFAULT_TIMEOUT)?;
    eprintln!("Finishing…");
    match ask(env, &Request::MeetStop, ipc::MEET_STOP_TIMEOUT)? {
        Response::MeetingStopped {
            title,
            path,
            length_secs,
        } => emit(&format::meeting_stopped(&title, &path, length_secs)),
        r => Err(unexpected(r)),
    }
}

pub fn meet_list(env: &Env, n: usize) -> anyhow::Result<()> {
    let mut list = env.store.meetings()?;
    list.truncate(n);
    emit(&format::meetings(&list))
}

pub fn status(env: &Env, json: bool) -> anyhow::Result<()> {
    match ask(env, &Request::Status, ipc::DEFAULT_TIMEOUT) {
        Ok(Response::Status(info)) => {
            if json {
                emit(&format!("{}\n", serde_json::to_string_pretty(&info)?))
            } else {
                emit(&format::status(&info))
            }
        }
        Ok(r) => Err(unexpected(r)),
        Err(e) => match e.downcast::<Exit>() {
            Ok(Exit { code: 2, .. }) => {
                env.warn_config();
                let dir = env.engine_config().model_dir();
                let model = if model::is_installed(&dir) {
                    format!("model: installed ({})", dir.display())
                } else {
                    "model: not downloaded (squawk model download)".to_string()
                };
                Err(exit(2, format!("{NOT_RUNNING}\n{model}")))
            }
            Ok(other) => Err(other.into()),
            Err(e) => Err(e),
        },
    }
}

pub fn reload(env: &Env) -> anyhow::Result<()> {
    match ask(env, &Request::Reload, ipc::DEFAULT_TIMEOUT)? {
        Response::Ok => emit("Reloaded config.toml\n"),
        r => Err(unexpected(r)),
    }
}

pub fn dict_add(env: &Env, phrase: &str) -> anyhow::Result<()> {
    let Some(entry) = dictionary::Entry::parse(phrase) else {
        return Err(exit(
            1,
            "Nothing to add. Give a term (\"Kubernetes\") or \"spoken -> written\".",
        ));
    };
    let path = &env.paths.dictionary_file;
    if dictionary::add(path, phrase)? {
        emit(&format!("Added: {}\n", entry.to_line()))
    } else {
        let spoken = entry.spoken().to_lowercase();
        let existing = Dictionary::load(path)?
            .entries()
            .iter()
            .find(|e| e.spoken().to_lowercase() == spoken)
            .map(|e| e.to_line())
            .unwrap_or_else(|| entry.to_line());
        emit(&format!("Already there: {existing}\n"))
    }
}

pub fn dict_list(env: &Env) -> anyhow::Result<()> {
    let dict = Dictionary::load(&env.paths.dictionary_file)?;
    let out: String = dict
        .entries()
        .iter()
        .map(|e| format!("{}\n", e.to_line()))
        .collect();
    emit(&out)
}

pub fn model_download(env: &Env) -> anyhow::Result<()> {
    env.warn_config();
    let config = env.engine_config();
    let dir = config.model_dir();
    if model::is_installed(&dir) {
        emit(&format!("Model already installed: {}\n", dir.display()))?;
    } else {
        std::fs::create_dir_all(&config.paths.models_dir)?;
        let dir = with_progress(|progress| {
            model::download(
                &config.model.url,
                &config.paths.models_dir,
                &config.model.dir,
                progress,
            )
        })?;
        emit(&format!("Model ready: {}\n", dir.display()))?;
    }
    if !config.cleanup_model {
        return Ok(());
    }
    let s1 = normalizer::model_path(&config.paths.models_dir);
    if normalizer::is_installed(&config.paths.models_dir) {
        return emit(&format!(
            "S1-mini by Superwhisper already installed: {}\n",
            s1.display()
        ));
    }
    with_progress(|progress| {
        normalizer::download(CLEANUP_MODEL_URL, &config.paths.models_dir, progress)
    })?;
    emit(&format!(
        "S1-mini by Superwhisper ready: {}\n",
        s1.display()
    ))
}

/// Run a download with its progress on one stderr line, rewritten in place.
fn with_progress<T>(
    run: impl FnOnce(&mut dyn FnMut(ModelStatus)) -> Result<T, squawk_engine::EngineError>,
) -> anyhow::Result<T> {
    let mut stderr = std::io::stderr();
    let mut last_len = 0usize;
    // one line, rewritten in place; padded so a shorter line erases a longer
    let mut show = |line: String| {
        let pad = last_len.saturating_sub(line.chars().count());
        last_len = line.chars().count();
        let _ = write!(stderr, "\r{line}{}", " ".repeat(pad));
        let _ = stderr.flush();
    };
    let result = run(&mut |status| match status {
        ModelStatus::Downloading { downloaded, total } => {
            show(format::download_progress(downloaded, total))
        }
        ModelStatus::Extracting => show("Checking the download".into()),
        other => show(other.label()),
    });
    eprintln!();
    Ok(result?)
}

pub fn transcribe(env: &Env, file: &Path, raw: bool, cwd: Option<&Path>) -> anyhow::Result<()> {
    env.warn_config();
    let config = env.engine_config();
    if !model::is_installed(&config.model_dir()) {
        return Err(exit(1, "Run `squawk model download` first."));
    }
    // build the context first: a bad --cwd should fail before a 1 s load
    let context = match cwd {
        Some(dir) => Some(offline_context(dir)?),
        None => None,
    };
    let cleanup_model = config.cleanup_model;
    let engine = Engine::new(config);
    let t = Instant::now();
    engine.load_model_blocking()?;
    let normalizer = if cleanup_model {
        match engine.load_cleanup_model_blocking() {
            Ok(n) => Some(n),
            Err(e) => {
                if raw {
                    eprintln!("S1-mini: {e} (run `squawk model download`); rules only");
                }
                None
            }
        }
    } else {
        None
    };
    let load = t.elapsed();
    let samples = squawk_engine::audio::load_file(file)?;
    let audio_secs = samples.len() as f64 / squawk_engine::SAMPLE_RATE as f64;
    let t = Instant::now();
    let text = engine.transcribe(&samples)?;
    let run = t.elapsed();

    let dict = Dictionary::load(&env.paths.dictionary_file)?;
    let opts = CleanupOptions {
        remove_fillers: env.config.dictation.remove_fillers,
        fix_doubles: true,
    };
    let finished = pipeline::finish_with(
        &text,
        normalizer.as_ref().map(|n| n as &dyn Normalizer),
        &dict,
        context.as_ref(),
        &opts,
    );
    let clean = finished.text;
    if raw {
        let s1 = match &finished.model_text {
            Some(answer) => format!("s1:    {answer}\n"),
            None => String::new(),
        };
        emit(&format!(
            "raw:   {}\n{s1}clean: {}\n{}{}\n",
            text.trim(),
            clean,
            format::timings(audio_secs, load, run),
            format::cleanup_timing(&finished.cleanup.label(), finished.model_time)
        ))
    } else if clean.is_empty() {
        Err(exit(1, "No speech found."))
    } else {
        emit(&format!("{clean}\n"))
    }
}

/// Claude Code mode as if a session were running in `dir`.
fn offline_context(dir: &Path) -> anyhow::Result<Context> {
    let cwd = dir
        .canonicalize()
        .map_err(|e| exit(1, format!("--cwd {}: {e}", dir.display())))?;
    let vocab = RepoVocab::build(&cwd);
    Ok(Context {
        session: Session {
            agent: Agent::Claude,
            pid: 0,
            cwd,
            tty: None,
        },
        vocab: Arc::new(vocab),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use squawk_core::status::MeetingInfo;
    use squawk_core::store::DictationEntry;

    fn env() -> (tempfile::TempDir, Env) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        (dir, Env::new(paths, Config::default(), None))
    }

    /// A stand-in for the app: answers `n` requests with `answer`, and
    /// hands back what it was asked.
    fn fake_app(
        env: &Env,
        n: usize,
        answer: impl Fn(&Request) -> Response + Send + 'static,
    ) -> std::thread::JoinHandle<Vec<Request>> {
        let listener = ipc::bind(&env.paths.socket).unwrap();
        std::thread::spawn(move || {
            let mut seen = Vec::new();
            for stream in listener.incoming().take(n) {
                ipc::serve_one(stream.unwrap(), |r| {
                    let a = answer(&r);
                    seen.push(r);
                    a
                })
                .unwrap();
            }
            seen
        })
    }

    #[test]
    fn meet_start_and_stop_talk_to_the_app() {
        let (_d, env) = env();
        let server = fake_app(&env, 3, |r| match r {
            Request::MeetStart { title } => Response::MeetingStarted(MeetingInfo {
                title: title.clone().unwrap_or_else(|| "Meeting".into()),
                path: "/m/a.md".into(),
                started_at: String::new(),
                elapsed_secs: 0,
            }),
            Request::Ping => Response::Pong {
                version: "0.1.0".into(),
            },
            Request::MeetStop => Response::MeetingStopped {
                title: "Standup".into(),
                path: "/m/a.md".into(),
                length_secs: 60,
            },
            _ => Response::Ok,
        });
        meet_start(&env, Some("  Standup ".into())).unwrap();
        meet_stop(&env).unwrap();
        let seen = server.join().unwrap();
        assert_eq!(
            seen,
            [
                Request::MeetStart {
                    title: Some("Standup".into())
                },
                Request::Ping,
                Request::MeetStop
            ]
        );
    }

    #[test]
    fn an_error_answer_is_exit_1_with_its_message() {
        let (_d, env) = env();
        let server = fake_app(&env, 1, |_| Response::Error {
            message: "a meeting is already recording".into(),
        });
        let exit = meet_start(&env, None)
            .unwrap_err()
            .downcast::<Exit>()
            .unwrap();
        assert_eq!(exit.code, 1);
        assert_eq!(exit.message, "a meeting is already recording");
        assert_eq!(server.join().unwrap(), [Request::MeetStart { title: None }]);
    }

    #[test]
    fn status_asks_the_app() {
        let (_d, env) = env();
        let server = fake_app(&env, 2, |_| {
            Response::Status(squawk_core::status::StatusInfo {
                version: "0.1.0".into(),
                state: squawk_core::AppState::Idle,
                model: ModelStatus::Ready,
                permissions: Default::default(),
                meeting: None,
                config_note: None,
                dictations_this_run: 0,
            })
        });
        status(&env, false).unwrap();
        status(&env, true).unwrap();
        assert_eq!(server.join().unwrap(), [Request::Status, Request::Status]);
    }

    #[test]
    fn reload_asks_the_app() {
        let (_d, env) = env();
        let server = fake_app(&env, 1, |_| Response::Ok);
        reload(&env).unwrap();
        assert_eq!(server.join().unwrap(), [Request::Reload]);
    }

    #[test]
    fn not_running_is_exit_2() {
        let (_d, env) = env();
        let err = ask(&env, &Request::Ping, Duration::from_secs(1)).unwrap_err();
        let exit = err.downcast::<Exit>().unwrap();
        assert_eq!(exit.code, 2);
        assert_eq!(exit.message, NOT_RUNNING);
    }

    #[test]
    fn status_when_not_running_mentions_the_model() {
        let (_d, env) = env();
        let exit = status(&env, false).unwrap_err().downcast::<Exit>().unwrap();
        assert_eq!(exit.code, 2);
        assert!(exit.message.starts_with(NOT_RUNNING));
        assert!(exit.message.contains("not downloaded"));
    }

    #[test]
    fn last_with_nothing_is_exit_1() {
        let (_d, env) = env();
        let exit = last(&env).unwrap_err().downcast::<Exit>().unwrap();
        assert_eq!(
            (exit.code, exit.message.as_str()),
            (1, "No dictations yet.")
        );
        env.store
            .append_dictation(&DictationEntry {
                at: NaiveDateTime::parse_from_str("2026-09-29 14:03:12", "%Y-%m-%d %H:%M:%S")
                    .unwrap(),
                app: "Ghostty".into(),
                project: None,
                text: "hello".into(),
            })
            .unwrap();
        assert!(last(&env).is_ok());
    }

    #[test]
    fn dict_add_rejects_empty_and_dedupes() {
        let (_d, env) = env();
        let exit = dict_add(&env, "  ")
            .unwrap_err()
            .downcast::<Exit>()
            .unwrap();
        assert_eq!(exit.code, 1);
        dict_add(&env, "cloud code -> Claude Code").unwrap();
        dict_add(&env, "Cloud Code -> something else").unwrap();
        let dict = Dictionary::load(&env.paths.dictionary_file).unwrap();
        assert_eq!(dict.entries().len(), 1);
    }

    #[test]
    fn transcribe_without_model_says_what_to_do() {
        let (_d, env) = env();
        let exit = transcribe(&env, Path::new("x.wav"), false, None)
            .unwrap_err()
            .downcast::<Exit>()
            .unwrap();
        assert_eq!(exit.message, "Run `squawk model download` first.");
    }

    #[test]
    fn offline_context_rejects_a_missing_dir() {
        assert!(offline_context(Path::new("/definitely/not/here")).is_err());
    }
}
