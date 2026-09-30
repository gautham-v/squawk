//! The controller thread: hotkey actions, IPC requests and popover clicks in;
//! dictations, pastes, meetings and [`Snapshot`]s out.
//!
//! A dictation, end to end:
//! 1. `StartRecording` → `engine.start_dictation()` immediately and publish
//!    `Recording` (the menu bar glyph follows the mic), then — while the user talks —
//!    read the front app and, if it is a terminal and `claude_code_mode` is
//!    on, `context::detect` + `VocabCache::get`.
//! 2. `EnterHandsFree` → publish `Recording { hands_free: true }`.
//! 3. `StopAndPaste` → publish `Transcribing`; `session.finish()` (if the
//!    front app changed meanwhile, the context is re-detected while the tail
//!    transcribes); `pipeline::finish` with the dictionary; if non-empty,
//!    paste, append to the day file, log the latency line; publish `Idle`.
//! 4. `Cancel(_)` → `session.cancel()`; publish `Idle`. Nothing is written.
//!
//! The notetaker (heads-up, call detection, maximum length) is
//! `squawk_core::notetaker::Notetaker`, ticked here once a
//! second with the call apps `mic_watch` reports and the calendar's events;
//! its prompt rides on the [`Snapshot`] to the panel under the menu bar icon.
//!
//! Everything slow happens here or on helper threads, never on main.

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, SecondsFormat};
use crossbeam_channel::{Receiver, Sender};
use squawk_core::cleanup::CleanupOptions;
use squawk_core::config::MeetingConfig;
use squawk_core::context::{self, Context, FrontApp, VocabCache};
use squawk_core::dictionary::DictionaryCache;
use squawk_core::hotkey::{Action, Timings};
use squawk_core::ipc::{Request, Response};
use squawk_core::notetaker::calls::CallId;
use squawk_core::notetaker::heads_up::UpcomingEvent;
use squawk_core::notetaker::{Notetaker, Outcome, Prompt, Reply, Setting, Settings, StopReason};
use squawk_core::status::{AppState, MeetingInfo, Permissions, StatusInfo};
use squawk_core::store::DictationEntry;
use squawk_core::{config_edit, pipeline, Config, ModelStatus, Paths, Store};
use squawk_engine::{
    DictationSession, Engine, EngineConfig, EngineEvent, MeetingHandle, MeetingOptions,
    MeetingResult,
};

use crate::hotkey::SharedMachine;
use crate::{calendar, frontmost, mic_watch, paste, permissions};

/// How often the notetaker is ticked.
const TICK: Duration = Duration::from_secs(1);
/// How often the calendar is re-read for heads-ups.
const CALENDAR_EVERY: Duration = Duration::from_secs(30);

/// What the dictation side is doing, for the menu bar and the popover.
#[derive(Debug, Clone, PartialEq)]
pub enum DictationPhase {
    Idle,
    Recording { since: Instant, hands_free: bool },
    Transcribing,
}

/// A meeting in progress, for the menu bar and the popover footer.
#[derive(Debug, Clone, PartialEq)]
pub struct MeetingSnap {
    pub title: String,
    pub path: PathBuf,
    pub since: Instant,
    pub started_at: DateTime<Local>,
    /// The mic dropped out and could not be reopened: "You" is no longer
    /// being recorded. Stays until the meeting ends.
    pub mic_lost: bool,
}

/// Everything the views need, published by the controller after every
/// change. Cheap to clone.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub dictation: DictationPhase,
    pub meeting: Option<MeetingSnap>,
    /// A stopped meeting's last chunks are still being transcribed.
    pub meeting_saving: bool,
    pub model: ModelStatus,
    pub permissions: Permissions,
    /// A malformed config.toml, shown as a muted line in the header.
    pub config_note: Option<String>,
    /// The last thing that went wrong (mic failed, paste failed), shown
    /// until the next successful dictation.
    pub last_error: Option<String>,
    pub dictations_this_run: u64,
    /// Bumped whenever a file under the data dir was written (a dictation, a
    /// meeting started or saved), so an open popover re-reads.
    pub revision: u64,
    /// Where the files are now (a config reload can move the data dir).
    pub paths: Paths,
    /// `[meeting]` as config.toml has it now (the Settings tab shows it).
    pub meeting_config: MeetingConfig,
    /// What the panel under the menu bar icon asks, if anything.
    pub prompt: Option<Prompt>,
    /// Calendar access, once it matters (heads-up on): `None` = not asked.
    pub calendar_access: Option<bool>,
}

impl Snapshot {
    pub fn initial(paths: Paths, model: ModelStatus, config_note: Option<String>) -> Snapshot {
        Snapshot {
            paths,
            dictation: DictationPhase::Idle,
            meeting: None,
            meeting_saving: false,
            model,
            permissions: Permissions::default(),
            config_note,
            last_error: None,
            dictations_this_run: 0,
            revision: 0,
            meeting_config: MeetingConfig::default(),
            prompt: None,
            calendar_access: None,
        }
    }
}

/// Messages to the controller thread.
pub enum Command {
    Hotkey(Action),
    /// From the socket; the reply goes back on `reply`.
    Ipc {
        request: Request,
        reply: Sender<Response>,
    },
    /// Footer "Record meeting".
    ToggleMeeting,
    /// Permissions may have changed (popover opened, tap retried).
    RecheckPermissions,
    /// Whether the event tap could be installed: the real Accessibility test.
    TapInstalled(bool),
    /// Re-read config.toml.
    Reload,
    Engine(EngineEvent),
    /// A stopped meeting finished writing (sent by the helper thread).
    MeetingSaved(Result<MeetingResult, String>),
    /// The call apps using the mic changed (from `mic_watch`).
    MicCalls(Vec<String>),
    /// A button on the prompt panel.
    Prompt(Reply),
    /// A Settings-tab change: written to config.toml, then reloaded.
    SetSetting(Setting),
    /// The calendar prompt was answered.
    CalendarAccess(bool),
    /// The calendar around now (read on a helper thread).
    CalendarEvents(Vec<UpcomingEvent>),
    /// Finish a running meeting, then answer on `done`.
    Quit {
        done: Sender<()>,
    },
}

/// Handle to the controller thread.
#[derive(Clone)]
pub struct Controller {
    tx: Sender<Command>,
}

impl Controller {
    /// Spawn the controller thread. `publish` is called on the controller
    /// thread with every new snapshot; it must hand off to the main thread
    /// (an unbounded futures channel drained by a gpui task) and return.
    pub fn spawn(
        base_paths: Paths,
        config: Config,
        config_note: Option<String>,
        engine: Engine,
        hotkeys: SharedMachine,
        publish: impl Fn(Snapshot) + Send + 'static,
    ) -> Controller {
        let (tx, rx) = crossbeam_channel::unbounded();
        let sink_tx = tx.clone();
        engine.set_event_sink(move |event| {
            let _ = sink_tx.send(Command::Engine(event));
        });
        let worker_tx = tx.clone();
        thread::Builder::new()
            .name("squawk-controller".into())
            .spawn(move || {
                let mut worker = Worker::new(
                    base_paths,
                    config,
                    config_note,
                    engine,
                    hotkeys,
                    Box::new(publish),
                    worker_tx,
                );
                worker.run(rx);
            })
            .expect("spawn controller thread");
        Controller { tx }
    }

    /// A controller whose commands land on `tx` (tests stand in for the
    /// thread).
    #[cfg(test)]
    pub(crate) fn from_sender(tx: Sender<Command>) -> Controller {
        Controller { tx }
    }

    /// Never blocks; a dropped controller drops the message.
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }

    /// Stop, finishing a running meeting first. Waits at most `timeout`.
    pub fn shutdown(&self, timeout: Duration) {
        let (done, wait) = crossbeam_channel::bounded(1);
        self.send(Command::Quit { done });
        let _ = wait.recv_timeout(timeout);
    }
}

/// The dictation being recorded.
struct Live {
    session: DictationSession,
    front: Option<FrontApp>,
    context: Option<Context>,
}

struct Worker {
    base_paths: Paths,
    paths: Paths,
    config: Config,
    engine: Engine,
    hotkeys: SharedMachine,
    publish: Box<dyn Fn(Snapshot) + Send>,
    tx: Sender<Command>,
    store: Store,
    dictionary: DictionaryCache,
    vocab: VocabCache,
    live: Option<Live>,
    meeting: Option<MeetingHandle>,
    tap_ok: Option<bool>,
    mic_requested: bool,
    screen_requested: bool,
    /// config.toml's mtime when last read, so opening the popover after an
    /// edit picks the edit up without a relaunch.
    config_mtime: Option<std::time::SystemTime>,
    snap: Snapshot,
    notetaker: Notetaker,
    /// The call apps using the mic, as last reported.
    mic_calls: Vec<String>,
    mic_watching: bool,
    /// The calendar around now, and when it was read.
    events: Vec<UpcomingEvent>,
    events_read: Option<Instant>,
    calendar_requested: bool,
    last_tick: Instant,
    /// Why the meeting being saved stopped, when it stopped on its own.
    stopped_because: Option<StopReason>,
    /// A calendar read is running on a helper thread.
    events_pending: bool,
    /// Quit was handled; the loop ends.
    stopping: bool,
}

impl Worker {
    fn new(
        base_paths: Paths,
        config: Config,
        config_note: Option<String>,
        engine: Engine,
        hotkeys: SharedMachine,
        publish: Box<dyn Fn(Snapshot) + Send>,
        tx: Sender<Command>,
    ) -> Worker {
        let paths = base_paths.clone().with_config(&config);
        let mut snap = Snapshot::initial(paths.clone(), engine.model_status(), config_note);
        snap.permissions = permissions::check();
        snap.meeting_config = config.meeting.clone();
        let notetaker = Notetaker::new(Settings::from(&config.meeting));
        Worker {
            store: Store::new(&paths),
            dictionary: DictionaryCache::new(paths.dictionary_file.clone()),
            vocab: VocabCache::new(),
            base_paths,
            paths,
            config,
            engine,
            hotkeys,
            publish,
            tx,
            live: None,
            meeting: None,
            tap_ok: None,
            mic_requested: false,
            screen_requested: false,
            config_mtime: None,
            snap,
            notetaker,
            mic_calls: Vec::new(),
            mic_watching: false,
            events: Vec::new(),
            events_read: None,
            calendar_requested: false,
            last_tick: Instant::now(),
            stopped_because: None,
            events_pending: false,
            stopping: false,
        }
    }

    fn run(&mut self, rx: Receiver<Command>) {
        self.config_mtime = self.read_config_mtime();
        self.start_notetaker_inputs();
        self.publish();
        loop {
            let command = match rx.recv_timeout(TICK) {
                Ok(command) => Some(command),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            };
            // Commands first (a fn press must not wait on the notetaker).
            if let Some(command) = command {
                self.handle(command);
            }
            if self.stopping {
                return;
            }
            if self.last_tick.elapsed() >= TICK {
                self.tick();
            }
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Hotkey(action) => self.on_hotkey(action),
            Command::Ipc { request, reply } => self.on_ipc(request, reply),
            Command::ToggleMeeting => self.toggle_meeting(),
            Command::RecheckPermissions => {
                self.reload_if_config_changed();
                self.recheck_permissions();
                self.retry_model_if_needed();
            }
            Command::TapInstalled(ok) => {
                self.tap_ok = Some(ok);
                self.recheck_permissions();
            }
            Command::Reload => self.reload(),
            Command::Engine(event) => self.on_engine(event),
            Command::MeetingSaved(result) => {
                self.snap.meeting_saving = false;
                self.snap.revision += 1;
                let reason = self.stopped_because.take();
                match result {
                    Ok(saved) => {
                        if let Some(reason) = reason {
                            self.notetaker.saved(
                                Instant::now(),
                                saved.title,
                                saved.path,
                                saved.length_secs,
                                reason,
                            );
                            self.sync_prompt();
                        }
                    }
                    Err(e) => self.snap.last_error = Some(e),
                }
                self.publish();
            }
            Command::MicCalls(apps) => {
                self.mic_calls = apps;
                self.tick();
            }
            Command::Prompt(reply) => self.on_prompt(reply),
            Command::SetSetting(setting) => self.set_setting(setting),
            Command::CalendarEvents(events) => {
                self.events = events;
                self.events_pending = false;
            }
            Command::CalendarAccess(granted) => {
                self.snap.calendar_access = Some(granted);
                self.events_read = None;
                self.publish();
            }
            Command::Quit { done } => {
                if let Some(live) = self.live.take() {
                    live.session.cancel();
                }
                if let Some(handle) = self.meeting.take() {
                    if let Err(e) = handle.stop() {
                        log::warn!("meeting stop on quit: {e}");
                    }
                }
                let _ = done.send(());
                self.stopping = true;
            }
        }
    }

    fn publish(&self) {
        (self.publish)(self.snap.clone());
    }

    // ── Dictation ──────────────────────────────────────────────────────────

    fn on_hotkey(&mut self, action: Action) {
        match action {
            Action::StartRecording => self.start_recording(),
            Action::EnterHandsFree => {
                if let DictationPhase::Recording { hands_free, .. } = &mut self.snap.dictation {
                    *hands_free = true;
                    self.publish();
                }
            }
            Action::StopAndPaste => self.stop_and_paste(Instant::now()),
            Action::Cancel(reason) => {
                if let Some(live) = self.live.take() {
                    live.session.cancel();
                    paste::discard_prepared();
                    log::info!("dictation cancelled: {reason:?}");
                }
                self.snap.dictation = DictationPhase::Idle;
                self.publish();
            }
            Action::PasteLast => match self.store.last() {
                Ok(Some(entry)) => {
                    let text =
                        with_trailing_space(&entry.text, self.config.dictation.trailing_space);
                    if let Err(e) = paste::paste(&text, self.restore_after()) {
                        self.fail(e);
                    }
                }
                Ok(None) => {}
                Err(e) => self.fail(format!("could not read the last dictation: {e}")),
            },
            Action::CopyLast => match self.store.last() {
                Ok(Some(entry)) => paste::copy(&entry.text),
                Ok(None) => {}
                Err(e) => self.fail(format!("could not read the last dictation: {e}")),
            },
            Action::ToggleMeeting => self.toggle_meeting(),
        }
    }

    fn start_recording(&mut self) {
        if let Some(stale) = self.live.take() {
            stale.session.cancel();
        }
        let session = match self.engine.start_dictation() {
            Ok(session) => session,
            Err(e) => {
                // A failed first-run download would otherwise never retry.
                if matches!(e, squawk_engine::EngineError::ModelMissing) {
                    self.engine.ensure_model();
                }
                self.hotkeys.force_idle();
                self.snap.dictation = DictationPhase::Idle;
                self.fail(e.to_string());
                return;
            }
        };
        self.snap.dictation = DictationPhase::Recording {
            since: session.started_at(),
            hands_free: false,
        };
        self.publish();

        if self.snap.permissions.microphone.is_none() && !self.mic_requested {
            self.mic_requested = true;
            let tx = self.tx.clone();
            permissions::request_microphone(move |_| {
                let _ = tx.send(Command::RecheckPermissions);
            });
        }

        let front = frontmost::front_app();
        let context = front.as_ref().and_then(|f| self.context_for(f));
        paste::prepare();
        self.live = Some(Live {
            session,
            front,
            context,
        });
    }

    /// The Claude Code / Codex session behind `front`, when there is one and
    /// the mode is on.
    fn context_for(&mut self, front: &FrontApp) -> Option<Context> {
        if !self.config.dictation.claude_code_mode || !context::is_terminal(&front.bundle_id) {
            return None;
        }
        let session = context::detect(front)?;
        let vocab = self.vocab.get(&session.cwd);
        Some(Context { session, vocab })
    }

    fn stop_and_paste(&mut self, released: Instant) {
        let Some(live) = self.live.take() else {
            self.snap.dictation = DictationPhase::Idle;
            self.publish();
            return;
        };
        self.snap.dictation = DictationPhase::Transcribing;
        self.publish();

        let front_now = frontmost::front_app();
        let moved = front_now.as_ref().map(|f| f.pid) != live.front.as_ref().map(|f| f.pid);
        let session = live.session;
        let (transcript, front, context) = if moved {
            // The user switched apps mid-dictation: re-detect while the tail
            // is still transcribing.
            thread::scope(|scope| {
                let finishing = scope.spawn(move || session.finish());
                let context = front_now.as_ref().and_then(|f| self.context_for(f));
                let transcript = finishing.join().unwrap_or_else(|_| {
                    Err(squawk_engine::EngineError::Transcribe("panicked".into()))
                });
                (transcript, front_now, context)
            })
        } else {
            (session.finish(), live.front, live.context)
        };

        let transcript = match transcript {
            Ok(t) => t,
            Err(e) => {
                self.snap.dictation = DictationPhase::Idle;
                self.fail(e.to_string());
                return;
            }
        };

        let pipeline_start = Instant::now();
        let opts = CleanupOptions {
            remove_fillers: self.config.dictation.remove_fillers,
            fix_doubles: true,
        };
        let text = pipeline::finish(
            &transcript.text,
            self.dictionary.get(),
            context.as_ref(),
            &opts,
        );
        let pipeline_ms = pipeline_start.elapsed().as_millis();

        let app = front
            .as_ref()
            .map(|f| f.name.clone())
            .unwrap_or_else(|| "Unknown".into());
        let project = context.as_ref().map(|c| c.session.project());
        let mut metrics = Metrics {
            audio_secs: transcript.audio_secs,
            segments: transcript.segments,
            tail_ms: transcript.tail_latency.as_millis(),
            pipeline_ms,
            paste_ms: 0,
            release_to_paste_ms: 0,
            chars: text.chars().count(),
            app: app.clone(),
            project: project.clone(),
            context: context.as_ref().map(|c| agent_name(c.session.agent)),
        };

        if text.is_empty() {
            metrics.release_to_paste_ms = released.elapsed().as_millis();
            log::info!(target: "dictation", "{}", metrics.line());
            self.snap.dictation = DictationPhase::Idle;
            self.publish();
            return;
        }

        let paste_start = Instant::now();
        let pasted = paste::paste(
            &with_trailing_space(&text, self.config.dictation.trailing_space),
            self.restore_after(),
        );
        metrics.paste_ms = paste_start.elapsed().as_millis();
        metrics.release_to_paste_ms = released.elapsed().as_millis();
        log::info!(target: "dictation", "{}", metrics.line());

        let entry = DictationEntry {
            at: Local::now().naive_local(),
            app,
            project,
            text,
        };
        let saved = self.store.append_dictation(&entry);

        self.snap.dictation = DictationPhase::Idle;
        self.snap.dictations_this_run += 1;
        self.snap.revision += 1;
        self.snap.last_error = match (pasted, saved) {
            (Err(e), _) => Some(e),
            (_, Err(e)) => Some(format!("could not save the dictation: {e}")),
            _ => None,
        };
        self.publish();
    }

    fn restore_after(&self) -> Duration {
        Duration::from_millis(self.config.dictation.paste_restore_ms)
    }

    fn fail(&mut self, message: String) {
        log::warn!("{message}");
        self.snap.last_error = Some(message);
        self.publish();
    }

    // ── Meetings ───────────────────────────────────────────────────────────

    fn toggle_meeting(&mut self) {
        if self.meeting.is_some() {
            self.stop_meeting(None, None);
        } else if let Err(e) = self.start_meeting(None, DEFAULT_TITLE, None) {
            self.fail(e);
        }
    }

    /// Start recording a meeting titled `title`, else after the calendar's
    /// current event, else `fallback`. `call` is the detected call it was
    /// started from, which it then follows.
    fn start_meeting(
        &mut self,
        title: Option<String>,
        fallback: &str,
        call: Option<CallId>,
    ) -> Result<MeetingInfo, String> {
        if self.meeting.is_some() {
            return Err("a meeting is already recording".into());
        }
        let title = meeting_title(
            title.as_deref(),
            self.config.meeting.calendar_titles,
            calendar::current_event_title,
            fallback,
        );
        let started_at = Local::now();
        let path = self.store.new_meeting_path(&started_at, &title);
        let system_audio = self.config.meeting.system_audio;
        if system_audio {
            let granted = permissions::screen_recording();
            self.snap.permissions.screen_recording = Some(granted);
            if !granted && !self.screen_requested {
                self.screen_requested = true;
                permissions::request_screen_recording();
            }
        }
        let opts = MeetingOptions {
            title: title.clone(),
            path: path.clone(),
            started_at,
            chunk_secs: self.config.meeting.chunk_secs,
            system_audio,
        };
        let handle = self.engine.start_meeting(opts).map_err(|e| e.to_string())?;
        let info = handle.info();
        self.snap.meeting = Some(MeetingSnap {
            title,
            path,
            since: Instant::now() - handle.elapsed(),
            started_at,
            mic_lost: false,
        });
        self.snap.revision += 1;
        self.meeting = Some(handle);
        self.notetaker
            .meeting_started(Instant::now(), &info.title, call);
        self.sync_prompt();
        self.publish();
        log::info!(target: "meeting", "started");
        Ok(info)
    }

    /// Stop on a helper thread (it blocks while the last chunks transcribe);
    /// `reply`, if any, is answered from there once the file is final.
    /// `reason` is set when the meeting stopped on its own (it is then
    /// announced once saved).
    fn stop_meeting(&mut self, reply: Option<Sender<Response>>, reason: Option<StopReason>) {
        let Some(handle) = self.meeting.take() else {
            if let Some(reply) = reply {
                let _ = reply.send(Response::Error {
                    message: "no meeting is recording".into(),
                });
            }
            return;
        };
        self.snap.meeting = None;
        self.snap.meeting_saving = true;
        if let Some(reason) = &reason {
            log::info!(target: "meeting", "stopping on its own: {reason:?}");
        }
        self.stopped_because = reason;
        self.notetaker.meeting_stopped();
        self.sync_prompt();
        self.publish();
        let tx = self.tx.clone();
        thread::Builder::new()
            .name("squawk-meeting-stop".into())
            .spawn(move || {
                let result = handle.stop().map_err(|e| e.to_string());
                if let Some(reply) = reply {
                    let _ = reply.send(match &result {
                        Ok(r) => Response::MeetingStopped {
                            title: r.title.clone(),
                            path: r.path.display().to_string(),
                            length_secs: r.length_secs,
                        },
                        Err(message) => Response::Error {
                            message: message.clone(),
                        },
                    });
                }
                log::info!(target: "meeting", "stopped");
                let _ = tx.send(Command::MeetingSaved(result));
            })
            .expect("spawn meeting stop thread");
    }

    // ── Notetaker ──────────────────────────────────────────────────────────

    /// Start what the settings need: the mic watcher (once; it runs for the
    /// life of the app) and, the first time the heads-up is on, the
    /// calendar prompt.
    fn start_notetaker_inputs(&mut self) {
        if self.notetaker.wants_mic() && !self.mic_watching {
            self.mic_watching = true;
            let tx = self.tx.clone();
            mic_watch::MicWatcher::start(std::process::id() as i32, move |apps| {
                let _ = tx.send(Command::MicCalls(apps));
            });
        }
        if self.notetaker.wants_calendar() {
            self.snap.calendar_access = calendar::access();
            if self.snap.calendar_access.is_none() && !self.calendar_requested {
                self.calendar_requested = true;
                let tx = self.tx.clone();
                calendar::request_access(move |granted| {
                    let _ = tx.send(Command::CalendarAccess(granted));
                });
            }
        }
    }

    /// Once a second: re-read the calendar when due, advance the notetaker,
    /// stop the meeting if it says so, publish a changed prompt.
    fn tick(&mut self) {
        let now = Instant::now();
        self.last_tick = now;
        if self.notetaker.wants_calendar() {
            let stale = self
                .events_read
                .is_none_or(|at| now.duration_since(at) >= CALENDAR_EVERY);
            if stale && !self.events_pending {
                // EventKit can take a while; never on this thread.
                self.events_read = Some(now);
                self.events_pending = true;
                let tx = self.tx.clone();
                thread::Builder::new()
                    .name("squawk-calendar".into())
                    .spawn(move || {
                        let events = calendar::upcoming_events().unwrap_or_default();
                        let _ = tx.send(Command::CalendarEvents(events));
                    })
                    .expect("spawn calendar read");
            }
        } else {
            self.events.clear();
        }
        let mic: &[String] = if self.notetaker.wants_mic() {
            &self.mic_calls
        } else {
            &[]
        };
        let stop = self.notetaker.tick(now, Local::now(), mic, &self.events);
        if let Some(reason) = stop {
            if self.meeting.is_some() {
                self.stop_meeting(None, Some(reason));
            }
        }
        if self.sync_prompt() {
            self.publish();
        }
    }

    /// Copy the notetaker's prompt into the snapshot. Returns whether it
    /// changed.
    fn sync_prompt(&mut self) -> bool {
        let prompt = self.notetaker.prompt().cloned();
        if prompt == self.snap.prompt {
            return false;
        }
        match &prompt {
            Some(Prompt::Call { app, .. }) => {
                log::info!(target: "notetaker", "call detected: {app}")
            }
            Some(Prompt::HeadsUp { .. }) => log::info!(target: "notetaker", "heads-up"),
            _ => {}
        }
        self.snap.prompt = prompt;
        true
    }

    fn on_prompt(&mut self, reply: Reply) {
        match self.notetaker.reply(reply) {
            Outcome::Nothing | Outcome::Extended => {}
            Outcome::StartMeeting {
                title,
                fallback,
                call,
            } => {
                if let Err(e) = self.start_meeting(title, &fallback, call) {
                    self.fail(e);
                }
            }
            Outcome::Open(path) => {
                let _ = std::process::Command::new("/usr/bin/open")
                    .arg(path)
                    .spawn();
            }
        }
        self.sync_prompt();
        self.publish();
    }

    /// Write one `[meeting]` key to config.toml (keeping the rest of the
    /// file as it is), then reload it.
    fn set_setting(&mut self, setting: Setting) {
        let path = self.base_paths.config_file.clone();
        match config_edit::set_in_file(&path, "meeting", setting.key(), setting.value()) {
            Ok(()) => self.reload(),
            Err(e) => self.fail(format!("could not save the setting: {e}")),
        }
    }

    // ── IPC, config, permissions, engine ───────────────────────────────────

    fn on_ipc(&mut self, request: Request, reply: Sender<Response>) {
        let response = match request {
            Request::Ping => Response::Pong {
                version: squawk_core::VERSION.into(),
            },
            Request::Status => Response::Status(status_info(&self.snap, Instant::now())),
            Request::MeetStart { title } => match self.start_meeting(title, DEFAULT_TITLE, None) {
                Ok(info) => Response::MeetingStarted(info),
                Err(message) => Response::Error { message },
            },
            Request::MeetStop => {
                self.stop_meeting(Some(reply), None);
                return;
            }
            Request::Reload => {
                self.reload();
                Response::Ok
            }
        };
        let _ = reply.send(response);
    }

    fn read_config_mtime(&self) -> Option<std::time::SystemTime> {
        std::fs::metadata(&self.base_paths.config_file)
            .and_then(|m| m.modified())
            .ok()
    }

    fn reload_if_config_changed(&mut self) {
        let mtime = self.read_config_mtime();
        if self.config_mtime.is_none() {
            self.config_mtime = mtime;
        } else if mtime != self.config_mtime {
            log::info!("config.toml changed; reloading");
            self.reload();
        }
    }

    fn reload(&mut self) {
        self.config_mtime = self.read_config_mtime();
        let (config, note) = Config::load(&self.base_paths.config_file);
        self.paths = self.base_paths.clone().with_config(&config);
        if let Err(e) = self.paths.ensure_dirs() {
            log::warn!("could not create data dirs: {e}");
        }
        self.store = Store::new(&self.paths);
        self.dictionary = DictionaryCache::new(self.paths.dictionary_file.clone());
        self.engine
            .update_config(EngineConfig::from_config(&self.paths, &config));
        self.hotkeys.set_timings(Timings::from(&config.hotkey));
        self.notetaker.set_settings(Settings::from(&config.meeting));
        self.snap.meeting_config = config.meeting.clone();
        self.config = config;
        self.start_notetaker_inputs();
        self.sync_prompt();
        self.snap.paths = self.paths.clone();
        self.snap.config_note = note;
        self.snap.revision += 1;
        self.publish();
    }

    fn recheck_permissions(&mut self) {
        let mut checked = permissions::check();
        checked.accessibility =
            Some(checked.accessibility == Some(true) && self.tap_ok != Some(false));
        checked.screen_recording = self.snap.permissions.screen_recording;
        if checked != self.snap.permissions {
            self.snap.permissions = checked;
            self.publish();
        }
    }

    /// Opening the popover retries a model that is missing or failed (a
    /// download that died offline, say). `ensure_model` does nothing while
    /// one is already in flight and resumes a partial download.
    fn retry_model_if_needed(&mut self) {
        if matches!(
            self.engine.model_status(),
            ModelStatus::Missing | ModelStatus::Failed { .. }
        ) {
            self.engine.ensure_model();
        }
    }

    /// Whether an engine event about dictation `session` is about the live
    /// one (a late event from a cancelled session must not touch a newer).
    fn is_live(&self, session: u64) -> bool {
        self.live.as_ref().map(|l| l.session.id()) == Some(session)
    }

    fn on_engine(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Model(status) => {
                self.snap.model = status;
                self.publish();
            }
            EngineEvent::DictationTooLong { session } => {
                if self.is_live(session) {
                    self.hotkeys.force_idle();
                    self.stop_and_paste(Instant::now());
                }
            }
            EngineEvent::MeetingProgress { .. } => {
                self.snap.revision += 1;
                self.publish();
            }
            EngineEvent::MeetingWarning(message) => self.fail(message),
            EngineEvent::MicLost { session, message } => {
                if !self.is_live(session) {
                    log::info!("mic lost for an old dictation: {message}");
                    return;
                }
                // Keep what was said before the mic went: paste it as if fn
                // came up now.
                self.hotkeys.force_idle();
                self.stop_and_paste(Instant::now());
                self.fail(format!("microphone lost: {message}"));
            }
            EngineEvent::MeetingMicLost(message) => {
                if let Some(meeting) = self.snap.meeting.as_mut() {
                    meeting.mic_lost = true;
                }
                self.fail(format!("microphone lost: {message}"));
            }
        }
    }
}

fn with_trailing_space(text: &str, trailing_space: bool) -> String {
    if trailing_space {
        format!("{text} ")
    } else {
        text.to_string()
    }
}

fn agent_name(agent: context::Agent) -> &'static str {
    match agent {
        context::Agent::Claude => "claude",
        context::Agent::Codex => "codex",
    }
}

/// The title of a meeting nobody named.
pub const DEFAULT_TITLE: &str = "Meeting";

/// The meeting's title: what the user typed (or the heads-up's event), else
/// the calendar event happening now (when allowed), else `fallback`
/// ("Meeting", or "Zoom call" for one started from a detected call).
pub fn meeting_title(
    explicit: Option<&str>,
    calendar_titles: bool,
    calendar: impl FnOnce() -> Option<String>,
    fallback: &str,
) -> String {
    if let Some(title) = explicit.map(str::trim).filter(|t| !t.is_empty()) {
        return title.to_string();
    }
    if calendar_titles {
        if let Some(title) = calendar()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
        {
            return title;
        }
    }
    fallback.to_string()
}

/// What `squawk status` shows, from a snapshot.
pub fn status_info(snap: &Snapshot, now: Instant) -> StatusInfo {
    let state = match &snap.dictation {
        DictationPhase::Recording { since, hands_free } => AppState::Recording {
            hands_free: *hands_free,
            elapsed_ms: now.saturating_duration_since(*since).as_millis() as u64,
        },
        DictationPhase::Transcribing => AppState::Transcribing,
        DictationPhase::Idle => match not_ready_reason(snap) {
            Some(reason) => AppState::NotReady { reason },
            None => AppState::Idle,
        },
    };
    StatusInfo {
        version: squawk_core::VERSION.into(),
        state,
        model: snap.model.clone(),
        permissions: snap.permissions,
        meeting: snap.meeting.as_ref().map(|m| MeetingInfo {
            title: m.title.clone(),
            path: m.path.display().to_string(),
            started_at: m.started_at.to_rfc3339_opts(SecondsFormat::Secs, false),
            elapsed_secs: now.saturating_duration_since(m.since).as_secs(),
        }),
        config_note: snap.config_note.clone(),
        dictations_this_run: snap.dictations_this_run,
    }
}

/// Why fn would not work right now, if it would not.
pub fn not_ready_reason(snap: &Snapshot) -> Option<String> {
    if snap.permissions.accessibility == Some(false) {
        return Some("needs Accessibility".into());
    }
    if snap.permissions.microphone == Some(false) {
        return Some("needs Microphone".into());
    }
    if !snap.model.is_ready() {
        return Some(snap.model.label());
    }
    None
}

/// The numbers behind one dictation's log line.
#[derive(Debug, Clone, PartialEq)]
pub struct Metrics {
    pub audio_secs: f32,
    pub segments: usize,
    pub tail_ms: u128,
    pub pipeline_ms: u128,
    pub paste_ms: u128,
    pub release_to_paste_ms: u128,
    pub chars: usize,
    pub app: String,
    pub project: Option<String>,
    /// "claude"/"codex" in Claude Code mode.
    pub context: Option<&'static str>,
}

impl Metrics {
    /// `audio=20.4s segments=5 tail_ms=182 ... app=Ghostty project=squawk
    /// context=claude`. Lengths and timings only, never text.
    pub fn line(&self) -> String {
        let mut line = format!(
            "audio={:.1}s segments={} tail_ms={} pipeline_ms={} paste_ms={} release_to_paste_ms={} chars={} app={}",
            self.audio_secs,
            self.segments,
            self.tail_ms,
            self.pipeline_ms,
            self.paste_ms,
            self.release_to_paste_ms,
            self.chars,
            log_value(&self.app),
        );
        if let Some(project) = &self.project {
            line.push_str(&format!(" project={}", log_value(project)));
        }
        line.push_str(&format!(" context={}", self.context.unwrap_or("none")));
        line
    }
}

/// A value that stays one token: quoted when it has spaces ("Google Chrome").
fn log_value(value: &str) -> String {
    if value.is_empty() || value.contains(char::is_whitespace) || value.contains('"') {
        format!("\"{}\"", value.replace('"', "'"))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn test_paths() -> Paths {
        Paths::under(std::path::Path::new("/nonexistent/squawk-test"))
    }

    fn snap() -> Snapshot {
        let mut s = Snapshot::initial(test_paths(), ModelStatus::Ready, None);
        s.permissions = Permissions {
            accessibility: Some(true),
            microphone: Some(true),
            screen_recording: None,
        };
        s
    }

    #[test]
    fn explicit_title_beats_the_calendar_which_beats_meeting() {
        let cal = || Some("Weekly sync".to_string());
        let m = DEFAULT_TITLE;
        assert_eq!(meeting_title(Some(" Standup "), true, cal, m), "Standup");
        assert_eq!(meeting_title(None, true, cal, m), "Weekly sync");
        assert_eq!(meeting_title(Some("  "), true, cal, m), "Weekly sync");
        assert_eq!(meeting_title(None, false, cal, m), "Meeting");
        assert_eq!(meeting_title(None, true, || None, m), "Meeting");
        assert_eq!(meeting_title(None, true, || Some(" ".into()), m), "Meeting");
    }

    #[test]
    fn a_meeting_from_a_detected_call_falls_back_to_the_app() {
        assert_eq!(meeting_title(None, true, || None, "Zoom call"), "Zoom call");
        let cal = || Some("Design review".to_string());
        assert_eq!(meeting_title(None, true, cal, "Zoom call"), "Design review");
    }

    #[test]
    fn the_calendar_is_not_asked_when_titles_are_off() {
        let title = meeting_title(None, false, || panic!("calendar was read"), DEFAULT_TITLE);
        assert_eq!(title, "Meeting");
    }

    #[test]
    fn the_log_line_matches_the_spec() {
        let m = Metrics {
            audio_secs: 20.44,
            segments: 5,
            tail_ms: 182,
            pipeline_ms: 3,
            paste_ms: 12,
            release_to_paste_ms: 197,
            chars: 312,
            app: "Ghostty".into(),
            project: Some("squawk".into()),
            context: Some("claude"),
        };
        assert_eq!(
            m.line(),
            "audio=20.4s segments=5 tail_ms=182 pipeline_ms=3 paste_ms=12 \
             release_to_paste_ms=197 chars=312 app=Ghostty project=squawk context=claude"
        );
    }

    #[test]
    fn the_log_line_quotes_names_with_spaces_and_omits_a_missing_project() {
        let m = Metrics {
            audio_secs: 1.0,
            segments: 1,
            tail_ms: 90,
            pipeline_ms: 0,
            paste_ms: 4,
            release_to_paste_ms: 100,
            chars: 12,
            app: "Google Chrome".into(),
            project: None,
            context: None,
        };
        assert!(
            m.line().ends_with("app=\"Google Chrome\" context=none"),
            "{}",
            m.line()
        );
    }

    #[test]
    fn status_reports_recording_with_elapsed_time() {
        let now = Instant::now();
        let mut s = snap();
        s.dictation = DictationPhase::Recording {
            since: now - Duration::from_millis(7_250),
            hands_free: true,
        };
        let info = status_info(&s, now);
        assert_eq!(
            info.state,
            AppState::Recording {
                hands_free: true,
                elapsed_ms: 7_250
            }
        );
        assert_eq!(info.version, squawk_core::VERSION);
    }

    #[test]
    fn status_is_not_ready_until_model_and_permissions_are() {
        let now = Instant::now();
        assert_eq!(status_info(&snap(), now).state, AppState::Idle);

        let mut s = snap();
        s.model = ModelStatus::Loading;
        assert_eq!(
            status_info(&s, now).state,
            AppState::NotReady {
                reason: "Loading model".into()
            }
        );

        let mut s = snap();
        s.permissions.accessibility = Some(false);
        assert_eq!(
            status_info(&s, now).state,
            AppState::NotReady {
                reason: "needs Accessibility".into()
            }
        );
    }

    #[test]
    fn status_carries_the_meeting() {
        let now = Instant::now();
        let mut s = snap();
        s.meeting = Some(MeetingSnap {
            title: "Weekly sync".into(),
            path: "/tmp/m.md".into(),
            since: now - Duration::from_secs(724),
            started_at: Local::now(),
            mic_lost: false,
        });
        let meeting = status_info(&s, now).meeting.unwrap();
        assert_eq!(meeting.title, "Weekly sync");
        assert_eq!(meeting.elapsed_secs, 724);
        assert_eq!(meeting.path, "/tmp/m.md");
    }

    #[test]
    fn trailing_space_is_one_space_never_a_newline() {
        assert_eq!(with_trailing_space("Hi.", true), "Hi. ");
        assert_eq!(with_trailing_space("Hi.", false), "Hi.");
    }
}
