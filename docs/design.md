# squawk — design

A fully local macOS dictation app. Hold fn, talk, let go: the words are pasted into whatever has
focus. Built for talking to Claude Code in a terminal, so when the front app is a terminal running
`claude` (or `codex`) it also turns spoken file names into `@mentions` and spells the repo's jargon
the way the repo does. Meetings are recorded as two tracks (you / them) and transcribed as they go.
Everything runs on the device: NVIDIA Parakeet TDT 0.6B v3 (int8 ONNX) through `transcribe-rs`.
No accounts, no API keys, no telemetry; the network is touched once, to download the model.

MIT, open source. Rust. The target is macOS 15 Sequoia on Apple Silicon. Nothing here uses
macOS 26 APIs (SpeechAnalyzer and friends are not there on 15).

This document describes how the pieces fit. Where it and a doc comment in the code disagree, the
code is right and this file is stale.

## Workspace

```
Cargo.toml                 workspace; shared deps in [workspace.dependencies]
crates/squawk-core         lib   config, config_edit, paths, store, dictionary, cleanup, pipeline, hotkey, ipc, status, text, context/, notetaker/
crates/squawk-engine       lib   audio, segmenter, model, recognizer, dictation, meeting, system_audio
crates/squawk-app          lib+bin "squawk-app", bundled as Squawk.app (LSUIElement)
crates/squawk-cli          bin "squawk"
scripts/bundle.sh          builds Squawk.app
```

Dependency direction: `core ← engine ← app`, `core ← engine ← cli`. The CLI links the engine for
`transcribe` and `model download` only.

---

## Files

Three roots (`squawk_core::Paths`):

| path | what | who writes | who reads |
|---|---|---|---|
| `~/squawk/dictations/YYYY-MM-DD.md` | one file per day of dictations | app | popover, CLI, TUI, Claude |
| `~/squawk/meetings/YYYY-MM-DD HHMM <title>.md` | one file per meeting | engine (via `Store::write_meeting`) | popover, CLI, TUI, Claude |
| `~/squawk/dictionary.txt` | the user's words | user, `squawk dict add`, Claude | app (every dictation, mtime-cached), CLI |
| `~/.config/squawk/config.toml` | settings | user (app writes a commented default on first run); the Settings tab writes `[meeting]` notetaker keys | app, CLI |
| `~/Library/Application Support/squawk/models/<dir>/` | the model | engine / `squawk model download` | engine |
| `~/Library/Application Support/squawk/squawk.sock` | IPC socket | app | CLI |
| `~/Library/Application Support/squawk/squawk.log` | log, one latency line per dictation | app, engine (via `log`) | people |
| `~/Library/Application Support/squawk/audio/` | WAVs, only with `keep_audio = true` | engine | people |

`~/squawk` moves with the config's `data_dir`. Env overrides for tests and a second instance:
`SQUAWK_DATA_DIR`, `SQUAWK_SUPPORT_DIR` (win over the config). `Paths::under(root)` builds a fully
contained layout for tests. Socket paths must stay under 104 bytes (the sun_path limit); the
default is ~60.

### Dictation day file

```markdown
# 2026-09-29

## 14:03:12 · Ghostty · squawk
Fix the resampler so it handles 48 kHz input.

## 14:05:40 · Safari
Thanks, that works.
```

- Heading: `## HH:MM:SS · <app>[ · <project>]`, separator is ` · ` (U+00B7 with spaces). `app` is
  the front app's localized name at paste time; `project` is the Claude Code/Codex session's cwd
  directory name, present only in Claude Code mode.
- Body: exactly the pasted text (without the trailing space), may span lines. A body line that would
  parse as an entry heading once leading backslashes are stripped is written with one extra leading
  backslash, and read back without it.
- Appended with `Store::append_dictation` (O_APPEND); file created with the `# date` line.
- Anything before the first entry heading, and `##` headings without a time, are ignored by the
  parser, so hand notes survive.
- API: `Store::{append_dictation, day, days, recent(n), last, day_path}`; pure
  `store::{format_entry, parse_day}`; `DictationEntry { at: NaiveDateTime, app, project: Option,
  text }`, `DictationEntry::source()` = "app · project".

### Meeting file

```markdown
---
title: "Weekly sync"
date: 2026-09-29T14:00:00-07:00
length: 00:42:10
status: recording
---

# Weekly sync

**You** 00:00:04
Morning. Can everyone hear me?

**Them** 00:00:09
Yes, loud and clear.
```

- Name: `YYYY-MM-DD HHMM <sanitized title>.md` (`store::meeting_file_name`, `sanitize_title`:
  `/ \ : * ? " < > |` → `-`, whitespace collapsed, ≤ 80 chars, empty → "Meeting");
  `Store::new_meeting_path` appends ` 2`, ` 3`… on collision.
- Front matter: `title` always double-quoted, `date` RFC 3339 with local offset, `length` HH:MM:SS,
  `status: recording` only while recording. The recorder rewrites the whole file atomically
  (`Store::write_meeting`, temp + rename) after every chunk; the final write drops `status`.
- Blocks: `**You** HH:MM:SS` / `**Them** HH:MM:SS` (offset from meeting start), then the text.
  Consecutive segments of one speaker fold into one block (`store::merge_segments`: sort by start,
  ties to You, drop empty, coalesce). With `echo_cancellation`, `store::drop_echoes` runs first
  (see "Meetings: echo").
- Parse: `store::parse_meeting` (lenient), `Store::meetings()` → `Vec<MeetingSummary>` newest first
  (hand-written files without front matter still list, titled/dated from the file name).

### dictionary.txt

```
# squawk dictionary: one entry per line.
Kubernetes
ChatGPT
cloud code -> Claude Code
```

- `Term` — written exactly this way whenever heard (case-insensitive, whole word). A term with
  inner capitals or a letter/digit mix (`ChatGPT`, `GitHub`, `gpt5`) also matches split forms
  ("chat GPT", "git hub", "GPT 5").
- `spoken -> written` — replacement; spoken words match across spaces/hyphens.
- Longest spoken form first. Never applied inside protected chunks (`@mentions`, paths, URLs,
  `code`, file names). `dictionary::add` appends with dedupe on the spoken form (case-insensitive)
  and creates the file with a header. `DictionaryCache` re-reads on mtime change.

### config.toml

Every key optional; unknown keys ignored; out-of-range values clamped; a malformed file loads the
defaults plus a human note (`Config::load -> (Config, Option<String>)`) that the popover and
`squawk status` show. `Config::write_default_if_missing` writes `config::DEFAULT_CONFIG_TOML`
(everything commented out).

| key | default | meaning |
|---|---|---|
| `data_dir` | `~/squawk` | root for dictations, meetings, dictionary |
| `keep_audio` | `false` | keep WAVs under support/audio |
| `[hotkey] tap_max_ms` | `300` (100–1000) | fn press shorter than this is a tap |
| `[hotkey] double_tap_ms` | `400` (150–1000) | window after a tap's release for the second tap |
| `[dictation] remove_fillers` | `true` | filler removal in cleanup |
| `[dictation] claude_code_mode` | `true` | @mentions + repo vocab in terminals running claude/codex |
| `[dictation] trailing_space` | `true` | paste one space after the text (never a newline) |
| `[dictation] paste_restore_ms` | `300` (50–5000) | restore the old clipboard this long after cmd+V |
| `[dictation] input_device` | `""` | input device name; empty = system default |
| `[dictation] max_secs` | `600` (10–3600) | a dictation is auto-finished (pasted) after this |
| `[meeting] chunk_secs` | `30` (5–300) | transcription chunk length per track |
| `[meeting] system_audio` | `true` | record the other side as "Them" |
| `[meeting] calendar_titles` | `true` | title from the current calendar event |
| `[meeting] echo_cancellation` | `true` | echo-cancelled mic + drop "You" lines that repeat "Them" (see "Meetings: echo") |
| `[meeting] heads_up_secs` | `15` (−1–3600; < 0 = off) | heads-up this long before a qualifying calendar event; 0 = at the start |
| `[meeting] detect_calls` | `true` | offer notes when a call app starts using the mic |
| `[meeting] max_minutes` | `120` (5–1440) | stop and save after this long, warning 2 min before |
| `[model] dir` | `parakeet-tdt-0.6b-v3-int8` | directory under models/ |
| `[model] url` | `https://blob.handy.computer/parakeet-v3-int8.tar.gz` | where to download it |
| `[model] threads` | `0` | ORT intra-op threads, 0 = ORT decides |

### Log

`squawk.log`, appended, one line per event, via the `log` crate (the app installs a tiny logger
that writes `<RFC3339> <LEVEL> <target>: <message>`). Per dictation, exactly one info line from the
app's controller:

```
2026-09-29T14:03:12-07:00 INFO dictation: audio=20.4s segments=5 tail_ms=182 pipeline_ms=3 paste_ms=12 release_to_paste_ms=197 chars=312 app=Ghostty project=squawk context=claude
```

`release_to_paste_ms` is fn-up (the `StopAndPaste` action) to cmd+V posted — the number we
optimise. **Never log dictated text** (privacy); lengths only.

---

## squawk-core

`pub mod`: `cleanup`, `config`, `config_edit`, `context`, `dictionary`, `error`, `hotkey`,
`ipc`, `notetaker`, `paths`, `pipeline`, `status`, `store`, `text`. Re-exports: `Config`, `Dictionary`, `Error`, `Result`,
`Paths`, `AppState`, `ModelStatus`, `Store`, `VERSION`.

- `error::Error` — `Io`, `Config{path,message}`, `Json`, `NoHome`, `NotRunning(PathBuf)`, `Ipc(String)`.
- `paths::Paths` — `detect()`, `from_home`, `under(root)` (tests), `with_data_dir`,
  `with_support_dir`, `with_config(&Config)`, `ensure_dirs()`; `expand_tilde`.
- `config` — structs above; `Config::{load, try_load, parse, save, write_default_if_missing,
  clamped}`; `DEFAULT_MODEL_URL`, `DEFAULT_MODEL_DIR`, `DEFAULT_CONFIG_TOML`.
- `status` — `AppState`, `ModelStatus` (`is_ready`, `progress`, `label`), `Permissions`
  (`can_dictate`), `MeetingInfo`, `StatusInfo`, `format_elapsed` ("0:07", "1:02:05"), `format_hms`
  ("00:23:06"). All serde, snake_case tags.
- `text` — `Chunk{lead,core,trail}`, `chunks`, `join`, `is_protected`, `map_prose`, `capitalize`,
  `has_inner_case`.
- `cleanup` — `CleanupOptions{remove_fillers, fix_doubles}`, `strip` (fillers, stutters),
  `finalize` (spacing, capital first letter unless the word has inner case or is protected, end
  punctuation unless the last chunk is protected), `clean` = both.
  Filler rules (conservative): um/uh/erm… always go; `like`, `you know`, `kind of`, `sort of`,
  `basically` only when set off by commas (or sentence-initial with a comma; `, you know.` as a
  closing tag); a leading `so`/`basically` goes when followed by a comma or ≥ 3 more words (a bare `so` only
  before a clause opener: "So I think…" loses it, "So far so good" keeps it).
  Stutters collapse ("the the", "we should we should") except words that are doubled on purpose
  (`that that`, `had had`, `very very`, `no no`…).
- `dictionary` — `Entry`, `Dictionary::{parse, load, apply, entries, terms, is_empty}`, `add`,
  `DictionaryCache`.
- `pipeline::finish(raw, &Dictionary, Option<&context::Context>, &CleanupOptions) -> String` —
  `strip` → `context::apply` → `Dictionary::apply` → `finalize`. Empty = paste nothing.
- `store` — see Files.
- `config_edit` — `set_value(text, table, key, value)` / `set_in_file(path, …)` (atomic temp +
  rename) with `toml_edit`: comments, blank lines, order and unknown keys survive; a key that is
  only there as a commented default (`# max_minutes = 120`) is uncommented in place; a missing
  key is added to its table (created if needed). A file that does not parse is left alone.
- `hotkey` — the state machine, below.
- `ipc` — the protocol, below.
- `notetaker` — the meeting helpers, below.

### squawk-core::notetaker

Pure; every rule is driven by explicit times (`Instant` for timers, `DateTime<Local>` for the
calendar) so the tests use synthetic timestamps.

- `calls` — `MicUser {pid, bundle_id, name}`; `classify(user, own_pid) -> Option<Source>`:
  `Source::App(name)` for `CALL_APPS` (bundle id or a dotted prefix, so helpers count: Zoom,
  Microsoft Teams, FaceTime (and `avconferenced`), Slack, Webex, Discord), `Source::Browser
  {bundle_id, name}` for `BROWSERS` (Chrome/Arc/Edge/Brave helpers, Firefox, Safari's
  `com.apple.WebKit.GPU` → Safari); own pid → `None`. `call_site(title)` names the call site in a
  window title by whole words, case-sensitive ("Meet – abc-defg-hij" → Google Meet, "… |
  Microsoft Teams", "Zoom Meeting"; "zoom in on photos" is nothing). `CallTracker::update(now,
  active_apps) -> Vec<CallEvent>`: a call `Started` once an app has held the mic `MIN_USE` (3 s)
  without a break (a shorter use is forgotten), `Ended` once it has let go for `END_AFTER` (10 s);
  taking the mic back within that is the same call (`CallId`).
- `heads_up` — `UpcomingEvent {key, title, start, end, all_day, other_attendees, video_link,
  declined, cancelled}`; `is_meeting` (timed, not declined/cancelled, titled, other people or a
  call link), `is_due(now, lead)` from `start − lead` until `start + LATE` (60 s), `due(events,
  now, lead, done)` the soonest not yet offered; `has_video_link(texts)` (`VIDEO_HOSTS`).
- `Notetaker` — `new(Settings)`, `set_settings`, `tick(now, wall, mic_apps, events) ->
  Option<StopReason>`, `prompt() -> Option<&Prompt>`, `reply(Reply::{Accept, Dismiss}) ->
  Outcome`, `meeting_started(now, title, Option<CallId>)`, `meeting_stopped()`, `saved(..)`,
  `wants_mic()`, `wants_calendar()`. `Prompt::{HeadsUp, Call, StoppingSoon, Saved}`,
  `Outcome::{Nothing, StartMeeting{title, fallback, call}, Extended, Open(path)}`,
  `StopReason::MaxLength`, `Setting::{HeadsUpSecs, DetectCalls, MaxMinutes}` (`key()`,
  `value()`, `apply()`). Rules:
  - a started call with no meeting and `detect_calls` → `Prompt::Call` for 20 s (no answer =
    Not now); each call is offered once; its end takes its prompt down;
  - a call that starts during a meeting is not offered; the call a meeting was started from
    (else the one holding the mic then) counts as offered. A call ending never stops a meeting:
    some apps let go of the mic on mute, mid-call. Meetings stop at `max_length` or by hand
    (⌥M, the popover, `squawk meet stop`); an old `stop_when_call_ends` key in config.toml is
    ignored like any unknown key;
  - `max_length − 2 min` → `Prompt::StoppingSoon` (stays up); Accept adds 30 min and re-arms the
    warning; `max_length` → `StopReason::MaxLength`; a changed setting applies to the running
    meeting;
  - heads-up only with no meeting, once per event, up until 60 s after the start (at least 30 s
    on screen); Accept → `StartMeeting` with the event's title;
  - `Saved` (after the maximum-length stop) for 8 s, Accept → `Open(path)`;
  - the newest prompt replaces the one showing; starting a meeting clears it.

### squawk-core::context

The public items in `src/context/mod.rs`:

```rust
pub const TERMINAL_BUNDLE_IDS: &[&str];           // ghostty, Terminal, iTerm2, WezTerm, Warp, kitty, Alacritty
pub const AGENT_PROCESS_NAMES: &[(&str, Agent)];  // ("claude", Claude), ("codex", Codex)
pub struct FrontApp { pub bundle_id: String, pub name: String, pub pid: i32 }
pub enum Agent { Claude, Codex }
pub struct Session { pub agent: Agent, pub pid: i32, pub cwd: PathBuf, pub tty: Option<PathBuf> }
impl Session { pub fn project(&self) -> String }
pub fn project_name(cwd: &Path) -> String;
pub fn is_terminal(bundle_id: &str) -> bool;
pub fn detect(front: &FrontApp) -> Option<Session>;
pub struct RepoVocab { pub root: PathBuf, pub files: Vec<String>, pub words: Vec<String> }
impl RepoVocab { pub fn build(cwd: &Path) -> RepoVocab }
pub struct VocabCache; impl VocabCache { pub fn new() -> Self; pub fn get(&mut self, cwd: &Path) -> Arc<RepoVocab> }
pub struct Context { pub session: Session, pub vocab: Arc<RepoVocab> }
pub fn apply(text: &str, context: &Context) -> String;
```

Behaviour:

- **detect**: only if `is_terminal(front.bundle_id)`. Walk the process table with libproc
  (`proc_listallpids`, `proc_pidinfo(PROC_PIDTBSDINFO)` for ppid/comm/tty dev,
  `proc_pidinfo(PROC_PIDVNODEPATHINFO)` for cwd; the `libproc` crate or raw `libc` FFI — no
  subprocesses). Candidates: processes named `claude`/`codex` (match `pbi_comm`/`pbi_name`, and for
  `node`-hosted installs the executable path or argv containing `claude`) whose ancestor chain
  reaches `front.pid`. Several candidates (several tabs): pick the one whose controlling tty device
  (`/dev/ttysNNN`) has the most recent atime/mtime (the tab being typed in). Must finish in < 15 ms
  typical.
- **RepoVocab::build**: root = cwd. Files from `git -C <cwd> ls-files` (cap 20 000; outside git a
  bounded walk, depth ≤ 4, skipping target/node_modules/.git, cap 5 000). Words: file stems and
  names, directory names, Cargo/package names (`Cargo.toml` `name`, `package.json` `name`),
  headings and capitalised/camelCase/snake_case terms from `README.md`, `CLAUDE.md`, `AGENTS.md`,
  `CONTEXT.md` at the root. Dedupe; keep canonical casing.
- **VocabCache::get**: cache per cwd; rebuild when `<git toplevel>/.git/index` mtime changes, or
  after 60 s outside git. Warm hit < 1 ms.
- **apply** (< 5 ms, runs after `cleanup::strip`, before the dictionary):
  1. File references → `@path` relative to `root`, only when the match is unique and confident.
     Spoken forms to handle: "audio dot rs" / "audio.rs" → `@src/audio.rs`; "the claude md" /
     "claude dot md" → `@CLAUDE.md`; "cargo toml" / "cargo dot toml" → `@Cargo.toml`; "read me" →
     `@README.md`; paths spoken with "slash" ("source slash main dot rs" → `@src/main.rs`, "src"
     spoken as "source"). The optional "the"/"at" before a file reference is consumed ("look at
     the cargo toml" → "look at @Cargo.toml"). Ambiguous (two `mod.rs`) → leave the words alone
     unless the spoken form includes enough of the path to disambiguate.
  2. Jargon: a word or word pair that case-insensitively (and space/hyphen-insensitively) equals a
     vocab word gets the vocab casing ("squawk core" → `squawk-core` only if that exact name is in
     vocab; "tokio" → `Tokio` only if the repo writes it that way). Never change common English
     words (keep a small stoplist; require length ≥ 4 or inner caps/digits/`_`/`-`).
  3. Never touch protected chunks it did not create. Trailing sentence punctuation after a created
     mention stays outside it (`@src/audio.rs.` must not happen: `cleanup::finalize` already skips
     the period after a protected last chunk; mid-sentence commas after a mention are fine).
- Tests: table tests for apply (unique/ambiguous/stoplist cases) against hand-built `RepoVocab`s;
  a test that builds vocab from a temp git repo; detect is tested manually (document how in a doc
  comment).

### Hotkey state machine (`squawk_core::hotkey`)

Pure; inputs carry an `Instant`. Constants: `DEFAULT_TAP_MAX_MS = 300`, `DEFAULT_DOUBLE_TAP_MS =
350` (both configurable). Keycodes: fn 63, esc 53, C 8, V 9, M 46. Flag bits: shift 0x20000,
control 0x40000, option 0x80000, command 0x100000, fn 0x800000.

Inputs: `FnDown{mods}` / `FnUp` (flagsChanged, keycode 63), `ModsChanged{mods}` (other
flagsChanged), `KeyDown{keycode, mods, repeat}`, `Tick` (timer at `deadline()`).
Outputs: `Outcome{actions: Vec<Action>, swallow: bool}` with `Action::{StartRecording,
EnterHandsFree, StopAndPaste, Cancel(Tap|FnAsModifier|Escape|Reset), PasteLast, CopyLast,
ToggleMeeting}`.

| state | input | → state | actions | swallow |
|---|---|---|---|---|
| Idle | FnDown, no mods | Pressed | StartRecording | |
| Idle | FnDown with mods | Idle | – | |
| Idle | ctrl+cmd+V / ctrl+cmd+C / opt+M (exact mods) | Idle | PasteLast / CopyLast / ToggleMeeting (repeat: none) | yes |
| Pressed | FnUp, held < tap_max | TapReleased | – | |
| Pressed | FnUp, held ≥ tap_max | Idle | StopAndPaste | |
| Pressed | esc | Idle | Cancel(Escape) | yes |
| Pressed | any other KeyDown, or ModsChanged with mods | Idle | Cancel(FnAsModifier) | |
| TapReleased | (any input at/after up + double_tap) | Idle first | Cancel(Tap), then the input is handled from Idle | |
| TapReleased | FnDown, no mods, inside window | SecondPress | EnterHandsFree | |
| TapReleased | esc | Idle | Cancel(Escape) | yes |
| TapReleased | other KeyDown / FnDown with mods | Idle | Cancel(Tap) | |
| SecondPress | FnUp | HandsFree | – | |
| SecondPress | esc | Idle | Cancel(Escape) | yes |
| SecondPress | other KeyDown / ModsChanged with mods | Idle | Cancel(FnAsModifier) | |
| HandsFree | FnDown, no mods | HandsFreeFnDown | – | |
| HandsFree | esc | Idle | Cancel(Escape) | yes |
| HandsFree | anything else (typing is allowed) | HandsFree | – | |
| HandsFreeFnDown | FnUp | Idle | StopAndPaste | |
| HandsFreeFnDown | other KeyDown / ModsChanged with mods (fn as modifier) | HandsFree | – | |
| HandsFreeFnDown | esc | Idle | Cancel(Escape) | yes |

Recording starts on the first fn down (no speech lost to the gesture decision); a discarded tap
throws that audio away. `deadline()` is `up_at + double_tap` in TapReleased, else `None`.
`reset()` → Idle, returning `Cancel(Reset)` if a recording was running (call after the tap was
disabled by the system). `force_idle()` → Idle silently (the controller ended the dictation itself,
e.g. `max_secs`). flagsChanged events are never swallowed. Arrow/F-key keyDowns carry the fn bit
even without fn held, so fn state comes only from flagsChanged keycode 63.

### IPC (`squawk_core::ipc`)

Unix socket at `Paths::socket`. One request per connection: client writes one JSON line, server
answers one JSON line, closes. `ipc::send(socket, &Request, timeout)` (client, `NotRunning` if
nothing listens), `ipc::bind` (replaces a stale socket, refuses if a live instance answers),
`ipc::serve_one(stream, handler)`, `read_request`, `write_response`. Timeouts: `DEFAULT_TIMEOUT`
5 s, `MEET_STOP_TIMEOUT` 120 s.

Requests (`{"cmd": ...}`):

| request | response |
|---|---|
| `{"cmd":"ping"}` | `{"type":"pong","version":"0.1.0"}` |
| `{"cmd":"status"}` | `{"type":"status", ...StatusInfo}` |
| `{"cmd":"meet_start","title":"Standup"}` (title optional) | `{"type":"meeting_started", ...MeetingInfo}` or `error` ("a meeting is already recording") |
| `{"cmd":"meet_stop"}` | after the final write: `{"type":"meeting_stopped","title":..,"path":..,"length_secs":..}` or `error` ("no meeting is recording") |
| `{"cmd":"reload"}` | `{"type":"ok"}` after re-reading config.toml |
| anything unparseable | `{"type":"error","message":"bad request: ..."}` |

`StatusInfo { version, state: AppState, model: ModelStatus, permissions: Permissions, meeting:
Option<MeetingInfo>, config_note, dictations_this_run }`. `MeetingInfo { title, path, started_at
(RFC 3339), elapsed_secs }`.

---

## squawk-engine

Everything public is `Send`; `Engine` is `Clone + Send +
Sync`.

```rust
pub const SAMPLE_RATE: u32 = 16_000;

pub struct EngineConfig { pub paths: Paths, pub model: ModelConfig, pub input_device: Option<String>,
                          pub keep_audio: bool, pub meeting: MeetingConfig, pub max_dictation: Duration }
impl EngineConfig { pub fn from_config(&Paths, &Config) -> Self; pub fn model_dir(&self) -> PathBuf }

pub enum EngineEvent { Model(ModelStatus), DictationTooLong{session}, MeetingProgress{path, elapsed_secs},
                       MeetingWarning(String), MicLost{session, message}, MeetingMicLost(String) }

impl Engine {
    pub fn new(EngineConfig) -> Engine;                       // cheap, no model, no mic
    pub fn set_event_sink(&self, impl Fn(EngineEvent) + Send + Sync + 'static);
    pub fn model_status(&self) -> ModelStatus;                // never blocks
    pub fn ensure_model(&self);                               // background download → extract → load; idempotent
    pub fn load_model_blocking(&self) -> Result<(), EngineError>;   // CLI; never downloads
    pub fn start_dictation(&self) -> Result<DictationSession, EngineError>;
    pub fn input_level(&self) -> InputLevel;                  // live dictation's mic RMS, 0 when none
    pub fn start_meeting(&self, MeetingOptions) -> Result<MeetingHandle, EngineError>;
    pub fn transcribe(&self, samples: &[f32]) -> Result<String, EngineError>;  // raw text, blocking
    pub fn update_config(&self, EngineConfig);
    pub fn config(&self) -> EngineConfig;
}

impl DictationSession { pub fn started_at(&self) -> Instant; pub fn elapsed(&self) -> Duration;
                        pub fn finish(self) -> Result<Transcript, EngineError>;  // blocks for the tail
                        pub fn cancel(self); }                                     // Drop = cancel
pub struct Transcript { pub text: String, pub audio_secs: f32, pub tail_latency: Duration,
                        pub segments: usize, pub audio_path: Option<PathBuf> }

pub struct MeetingOptions { pub title: String, pub path: PathBuf, pub started_at: DateTime<Local>,
                            pub chunk_secs: u64, pub system_audio: bool }
impl MeetingHandle { pub fn options(&self) -> &MeetingOptions; pub fn elapsed(&self) -> Duration;
                     pub fn info(&self) -> MeetingInfo; pub fn stop(self) -> Result<MeetingResult, EngineError>; }
pub struct MeetingResult { pub path: PathBuf, pub title: String, pub length_secs: u64, pub had_system_audio: bool }

pub enum EngineError { ModelMissing, ModelNotReady(String), ModelLoad{path,message}, Download(String),
                       Mic(String), SystemAudio(String), Transcribe(String), Busy(&'static str),
                       AudioFile{path,message}, Io, Core }
```

Supporting public modules: `audio` (`MicCapture::{start, device_name, stop}`,
`input_devices`, `Resampler::{new, process, flush}`, `downmix`, `load_file`, `write_wav`),
`segmenter` (`SegmenterConfig`, `Segmenter::{new, push, finish}`, `Cut{start, samples,
has_speech}`), `model` (`REQUIRED_FILES`, `is_installed`, `download`, `LoadedModel::{load,
transcribe}`, `Recognized{text, segments}`), `recognizer` (`Priority::{Dictation, Meeting}`,
`Recognizer::{spawn, submit}`), `system_audio` (`SystemAudioCapture::{start, stop}`,
`has_permission`).

### Threads

- **Recognizer**: one thread owns the only `ParakeetModel` (~700 MB). Priority queue: every queued
  dictation job before any meeting job; FIFO within a priority. Jobs submitted while loading wait.
- **Dictation capture**: `start_dictation` spawns a thread that opens the cpal input stream (cpal
  streams are `!Send`) at the device's default config, and returns without waiting for the first
  buffer. The CoreAudio callback only copies into a channel/ring; downmix + resample to 16 kHz +
  segmenting happen on the capture thread. Committed segments are submitted to the recognizer
  immediately (dictation priority); results are collected in order.
- **Meeting**: two capture threads (mic via cpal, or voice processing with `echo_cancellation`;
  system audio via ScreenCaptureKit) each feeding a
  segmenter configured for `chunk_secs` (min = chunk − 5 s, max = chunk + 5 s), and a writer thread
  that offsets each chunk's segments into meeting time, merges with `store::merge_segments`, and
  rewrites the file (`status: recording`) after each chunk, sending `MeetingProgress`.

### Latency (the core goal)

- Segmenter cuts at pauses (≥ 350 ms of silence once ≥ 3 s is buffered, shrinking to 150 ms as
  more is buffered; forced at 10 s at the quietest frame of the last 2 s; see "Changes during
  build" for the details and the early tail transcription). So when fn comes up only the tail since the last pause (usually
  < 3 s) is left to transcribe. `finish`: stop capture (flush resampler), submit the tail, wait for
  every outstanding segment, join texts with single spaces.
- Target: `finish` < 250 ms for a 20 s utterance on an M4 (Parakeet int8 is ~30× real time on
  CPU; a 3 s tail is ~100 ms). Log `tail_latency`. Consider pre-warming the model with one short
  silent inference after load (first inference pays ORT allocation costs).
- Opening the mic takes ~50–150 ms; that is fine because recording starts on fn *down*. Parakeet's
  default 250 ms leading-silence pad in `transcribe-rs` stays on for the first segment; later
  segments may use `transcribe_raw` with a small pad of your choosing. Do not keep the mic open
  while idle (the orange mic indicator must mean squawk is listening).
- Too little audio (< 0.25 s) or no speech frames at all → `Transcript.text = ""`, no inference.

### Model

- `transcribe_rs::onnx::parakeet::ParakeetModel::load(dir, &Quantization::Int8)`,
  `transcribe_with(&samples, &ParakeetParams{ timestamp_granularity: Some(Segment), .. })` (or the
  `SpeechModel` trait's `transcribe`). Parakeet v3 emits punctuation and capitals itself.
- Download (the only network access): `config.model.url` (default Handy's mirror,
  `https://blob.handy.computer/parakeet-v3-int8.tar.gz`, 478 517 071 bytes) with `ureq`, to
  `<models>/<dir>.tar.gz.part` (resume with Range when a partial exists), then gunzip+untar with
  `flate2`+`tar` into `<models>/<dir>.partial/`, **skipping AppleDouble `._*` and `PaxHeader`
  entries**; the tarball's top-level dir is `parakeet-tdt-0.6b-v3-int8/`. Rename to `<models>/<dir>`
  only when `REQUIRED_FILES` are all there; delete the tarball. Report `Downloading{downloaded,
  total}` at most 4×/s, then `Extracting`, `Loading`, `Ready` / `Failed{message}`.
- `ensure_model` at app start: installed → load; missing → download then load. Retried when the
  popover opens or fn is pressed while the model is missing or failed.

### Meetings: system audio

ScreenCaptureKit audio-only via the `screencapturekit` crate (v11, feature `macos_15_0`): an
`SCStream` on the main display, `captures_audio = true`, `excludes_current_process_audio = false` (see "Changes during build"),
`sample_rate = 16000`, `channel_count = 1`, minimal video (2×2, lowest frame rate), only an Audio
output handler. Needs "Screen & System Audio Recording". If it fails, the meeting continues
mic-only (`MeetingWarning`), `had_system_audio = false`. (A Core Audio process tap is the fallback
design if SCK proves unreliable; same API.)

### Meetings: echo

On speakers the other side comes back in through the mic and would be transcribed twice, once as
"Them" and once as "You". Two layers, both behind `[meeting] echo_cancellation` (default on):

1. **Voice processing on the meeting mic** (`voice_processing.rs`, `audio::MicMode::EchoCancelled`).
   `AVAudioEngine` with `inputNode.setVoiceProcessingEnabled(true)`: Apple's voice processing I/O
   (echo cancellation, noise suppression, AGC). It cancels whatever the output device plays, from
   any app, so the call never has to pass through squawk and ScreenCaptureKit is not its
   reference. Other-audio ducking is set to the minimum (`enableAdvancedDucking = false`,
   `duckingLevel = .min`). The input node reports 9 channels on a MacBook Pro; channel 0 is the
   processed voice. The output side (`mainMixerNode`) must exist before voice processing is turned
   on, or `start` fails with -10875. A hardware change stops the engine and posts
   `AVAudioEngineConfigurationChangeNotification`; that goes through the same reopen path as a
   lost cpal stream (reopened, silence for the gap). It always uses the default input: a named
   `input_device` other than the default, or a failure to start, falls back to plain cpal capture
   with a log line. Dictation never uses it (no ducking, lowest latency).
2. **Transcript filter** (`store::drop_echoes`, run by the meeting writer over all segments on
   every write, since a "Them" chunk can land after the "You" chunk its echo is in). Pool the
   words of the "Them" segments overlapping a "You" segment (each widened by 3 s). The "You"
   segment is dropped when it has ≥ 4 words, ≥ 75 % of them appear in the pool in order (LCS),
   and ≥ 50 % sit in a word pair the pool also has. Echo is their sentence again give or take a
   misheard word; a reply shares a phrase at most, and the pair test stops a long pool from
   matching scattered function words. Short answers ("Yes.", "Right, exactly.") are never
   dropped; a verbatim read-back of 4+ words within 3 s would be.

Measured on a MacBook Pro (M4) with its built-in speakers and mic, `say` at volume 44/100
(`probe meeting 17 [no-aec]`, two sentences, 25 words): without echo cancellation all 25 words
came back as "You", verbatim; with it, none (the mic's speech level fell ~46 dB, the transcript
filter had nothing left to drop). Run on the no-AEC transcripts, the filter alone dropped every
leaked block and kept every "Them" block.

Costs: other audio (the call itself, music) is ducked ~8 dB while a meeting records, in the
speakers and in the "Them" track alike (the default ducking level is ~30 dB); transcription of
"Them" was unaffected. Opening takes ~0.6–0.7 s instead of ~0.1 s, a reopen ~1.4 s. CPU: ~22 % of
one core in-process plus ~8 % in coreaudiod, against ~3 % for plain capture. Voice processing
also applies noise suppression and AGC to your own voice; the measurement above had no near-end
talker, so it says nothing about how that changes transcription of "You".

While voice processing runs, macOS hands every other client of that mic a signal ~40 dB down. A
dictation during such a meeting therefore does not open its own stream: it listens to the
meeting's (`audio::MicShare`, held by the engine and lent by the meeting while its mic is echo
cancelled), which also keeps the call out of the dictation. Its blocks come in ~100 ms pieces
(the `AVAudioEngine` tap), so a dictation during a meeting can be up to ~100 ms slower to paste.

### Audio files

`audio::load_file`: WAV via `hound` (any rate/channels → 16 kHz mono); anything else via
`/usr/bin/afconvert -f WAVE -d LEF32@16000 -c 1 <in> <tmp.wav>`. `write_wav`: 16-bit PCM 16 kHz
mono (for `keep_audio`, named `<support>/audio/<YYYY-MM-DD HHMMSS>.wav`).

### Tests

Unit tests for segmenter (synthetic tone/silence patterns: cut points, forced cuts, tail, silent
segments dropped), resampler (length ratio, a sine keeps its frequency), downmix, download
extraction (a small synthetic tar.gz with `._` entries), priority ordering in the recognizer (with a
fake model behind a trait), meeting chunk offsetting. A model-dependent test is `#[ignore]` and
runs when the model is installed (`cargo test -p squawk-engine -- --ignored`), transcribing a
generated or bundled short WAV (generate with `say -o x.aiff` + afconvert in the test if needed —
do not commit copyrighted audio).

---

## squawk-app

Bundle: `Squawk.app`, executable `squawk-app`, `CFBundleIdentifier = com.gauthamv.squawk`,
`LSUIElement = true`, `LSMinimumSystemVersion = 15.0`, `NSMicrophoneUsageDescription` ("Squawk
listens while you hold fn and turns your speech into text, on this Mac."),
`NSCalendarsFullAccessUsageDescription` + `NSCalendarsUsageDescription` ("Squawk offers to take
notes just before a meeting starts and names the notes after the event."). `scripts/bundle.sh` and a `Makefile` (`run`,
`install`, `test`, `check`, `bundle`). Signing uses a stable identity from the keychain if there
is one (Developer ID preferred, `CODESIGN_IDENTITY` overrides), else ad-hoc — sign with hardened runtime **and an entitlements file with
`com.apple.security.device.audio-input`**, or the mic is silently denied. A stable signature also
keeps the Accessibility grant across rebuilds. Also install the CLI: `make install` copies
`squawk` to `~/.local/bin`.

The status item and popover window plumbing (anchor under the item, toggle, Esc/outside-click/
focus-loss close, resize to content), theme tokens and fonts follow the usual GPUI menu bar app
pattern; EventKit supplies calendar titles.

### Startup (main.rs)

1. `Paths::detect()`, `Config::load`, `paths.with_config`, `ensure_dirs`,
   `Config::write_default_if_missing(paths.config_file)`; install the file logger.
2. `Engine::new(EngineConfig::from_config(..))`; sink → controller; `ensure_model()`.
3. `Controller::spawn(paths, config, note, engine, publish)` where `publish` hands the `Snapshot`
   to the main thread (unbounded futures channel drained by a gpui task).
4. `HotkeyTap::start(Timings::from(&config.hotkey), |a| controller.send(Command::Hotkey(a)))`. On
   `NotTrusted`: `permissions::prompt_accessibility()` once, show "Needs Accessibility" in the
   header, and retry `start` every 2 s until it succeeds (no relaunch needed after granting).
5. `ipc_server::spawn(&paths.socket, controller.clone())`. If another instance is running
   (`ipc::bind` refuses), log it and quit.
6. Status item (given `engine.input_level()` to poll while recording) + popover. The item
   animates on its own timer (see Menu bar); a 250 ms gpui timer repaints an open popover's
   header clock while a recording or meeting is counting.

### Controller (controller.rs)

One thread; owns `Engine`, the live `DictationSession`, the live `MeetingHandle`, `Store`,
`DictionaryCache`, `VocabCache`, and the `Snapshot`. Receives `Command`s:

- `Hotkey(StartRecording)`: `engine.start_dictation()` at once (errors → `last_error`, and
  `force_idle` the tap). Then, while the user talks: `frontmost::front_app()`; if
  `claude_code_mode` and `context::is_terminal`, `context::detect` + `vocab_cache.get(cwd)` →
  `Option<Context>`. If the mic permission is undetermined, request it (first dictation).
  Publish `Recording{since, hands_free: false}`.
- `Hotkey(EnterHandsFree)`: publish `Recording{hands_free: true}`.
- `Hotkey(StopAndPaste)` (or `EngineEvent::DictationTooLong`): publish `Transcribing`;
  `session.finish()`; `pipeline::finish(text, dict_cache.get(), ctx.as_ref(), &CleanupOptions{
  remove_fillers: config.dictation.remove_fillers, fix_doubles: true })`. If the front app changed
  since start, re-read it and redo detect. If non-empty: paste `text + (" " if trailing_space)` on
  the main thread (`paste::paste`, restore after `paste_restore_ms`), `store.append_dictation`
  (app = front app name, project = session project), write the log line, bump
  `dictations_this_run`. Publish `Idle`.
- `Hotkey(Cancel(_))`: `session.cancel()`, publish `Idle`. Nothing written.
- `Hotkey(PasteLast)` / `CopyLast`: `store.last()` → paste / copy (main thread).
- `Hotkey(ToggleMeeting)` / `Command::ToggleMeeting` / IPC `MeetStart`/`MeetStop`: start —
  title = explicit, else `calendar::current_event_title()` when `calendar_titles`, else "Meeting";
  `path = store.new_meeting_path(now, title)`; `engine.start_meeting(..)`. Stop — `handle.stop()`
  on a helper thread (it blocks), reply to IPC when done. Publish.
- `Ipc{Status}` → `StatusInfo` from the snapshot. `Reload` → re-read config, `engine.update_config`,
  tap `set_timings`.
- Engine `Model(status)` → snapshot. `MicLost` for the live session → finish and paste what was
  captured, then `last_error`. `MeetingMicLost` → "mic lost" on the meeting header line.
- Notetaker: the loop waits with `recv_timeout(1 s)` and ticks the `Notetaker` at least once a
  second (and on every `MicCalls`) with the call apps last reported by `mic_watch` and the
  calendar's events (`calendar::upcoming_events`, re-read every 30 s while the heads-up is on). A
  `StopReason` → the usual stop path; when the file is saved, `Notetaker::saved` puts up "Saved
  notes". `Prompt(Reply)` → `Notetaker::reply` → start a meeting (title: the event's, else the
  current calendar event, else the fallback, e.g. "Zoom call") / open the file. Every meeting
  start (⌥M, popover, IPC, prompt) calls `meeting_started`, every stop `meeting_stopped`.
  `SetSetting(Setting)` → `config_edit::set_in_file(config, "meeting", key, value)` then the
  normal reload. The mic watcher starts once `detect_calls` is on (and then runs for the app's
  life; samples are ignored while it is off); Calendar access is
  requested (in the background) the first time the heads-up is on and access is undetermined.
  `Snapshot` carries `meeting_config`, `prompt` and `calendar_access`.

### Paste (paste.rs)

Main thread. Save all pasteboard items (every type as `NSData`), `clearContents` +
`setString:forType:NSPasteboardTypeString`, post cmd+V (keycode 9 down/up with
`kCGEventFlagMaskCommand`, from a `CGEventSource` with `kCGEventSourceStateHIDSystemState` — and
explicitly set flags so a still-held fn/ctrl does not leak into the paste), then after
`paste_restore_ms` restore the saved items if `changeCount` is still ours. Terminals get no
newline, ever (squawk never submits). `copy` just sets the string.

### Menu bar (status_item.rs, menu_bar_icon.rs)

`MenuBarState` (`from_snapshot` precedence: recording > transcribing > meeting >
needs-attention > idle):

The item is only the glyph: five vertical rounded bars, 2 pt wide, 1.5 pt gaps, centred in a
16 × 18 pt template image (AppKit tints it for a light, dark or tinted menu bar). No text, no
colour; the image never changes size, so the item never changes width.

| state | drawing | timer |
|---|---|---|
| `Idle` | resting bars, 6/10/14/10/6 pt | none |
| `NeedsAttention` | resting bars at 36% opacity | none |
| `Recording` (push-to-talk and hands-free alike) | bars follow the mic, 3–16 pt | 24 fps |
| `Transcribing` | from the last live heights back to rest over 250 ms (cubic ease-out), then still | 30 fps, then none |
| `Meeting` | resting heights × (0.78 + 0.1·sin(2πt / 5 s)) | 8 fps |

Live bars: `DictationSession`'s capture callback stores each block's RMS in the engine's shared
`InputLevel` atomic; each frame reads it, gates it (−50 dBFS → 0, −18 dBFS → 1), smooths it (40 ms
attack, 150 ms release), takes the square root so quiet speech still moves the bars, and scales
per bar by 0.62/0.86/1/0.86/0.62 with a small per-bar jitter. Heights snap to 0.5 pt so an
unchanged frame is not redrawn. A settle already under way carries on into `Idle`.

The timer is a main-run-loop `NSTimer` (common modes, 10% tolerance) that exists only while the
state moves and is invalidated as soon as it stops. With `accessibilityDisplayShouldReduceMotion`
on, every state draws one still frame (recording: a frozen waveform) and no timer runs. The
maths is pure (`menu_bar_icon::Animator`) and tested; `examples/menu_bar_preview.rs --png DIR`
renders every state's frames on a light and a dark menu bar.

### Prompt panel (ui/panel.rs)

The notetaker's questions, as a 292 × 84 px `WindowKind::PopUp` (a non-activating NSPanel at pop-up
level, joining all Spaces and shown over full-screen apps), opened with `focus: false` so it never
takes focus from the call, centred under the item like the popover (`placement(.., width)`).
main.rs opens it while `snapshot.prompt` is `Some` and the popover is closed (the popover opening
takes it down; the 250 ms timer brings it back when the popover closes), updates it in place
when the prompt changes, and sends button presses as `Command::Prompt(Reply)`. Same material
and ink as the popover: one medium title line, one muted line, one or two small bordered
buttons (the first semibold). `panel_text(&Prompt)` is the pure wording:

| prompt | title | line | buttons |
|---|---|---|---|
| `HeadsUp` | event title | `15:00–15:30` | Record · Not now |
| `Call` | Call detected in Zoom | Start notes? | Start · Not now |
| `StoppingSoon` | Stopping in 2 min | Weekly sync · 2 h limit | Keep going +30 min |
| `Saved` | Saved notes · Weekly sync | 2:00:00 · reached the time limit | Open |

### Call detection (mic_watch.rs)

Thin Core Audio bindings (raw FFI; macOS 14.2+): `kAudioHardwarePropertyProcessObjectList`,
and per process object `kAudioProcessPropertyIsRunningInput`, `…PID`, `…BundleID` (plus
`proc_name` for daemons without a bundle id). `input_processes()` lists every process with input
running; `call_apps_among(users, own_pid)` classifies them (`notetaker::calls`), reading a
browser's window titles through Accessibility (`AXWindows` → `AXTitle` of every window of every
running instance) only when a browser is the one using the mic. `MicWatcher::start(own_pid,
on_change)` runs one thread woken by listener blocks on the process list, the device list, each
process's running-input property and each device's `kAudioDevicePropertyDeviceIsRunningSomewhere`
(the per-process property alone did not notify when a new process started its input; the
device one does). While a call app or a browser holds the mic it re-reads every second (a tab
title can change without the mic changing); otherwise it sleeps until woken (30 s safety poll).
`on_change` gets the sorted call-app names, only when they change. "Hey Siri" (`corespeechd`)
holds input much of the time and is never a call.

Measured on this Mac (macOS 15.8) with `examples/mic_probe.rs`: `ffmpeg -f avfoundation -i :0`
showed up as `pid … bundle "" name "ffmpeg"` within the 0.5 s poll and was gone when it exited; a
separate Chrome instance on a local page titled "Meet – abc-defg-hij" holding the mic through
`getUserMedia` showed up as `com.google.Chrome.helper` (Google Chrome Helper) with the window
title "Meet – abc-defg-hij - Google Chrome" → Google Meet; Firefox Developer Edition used the mic
from its main process (`org.mozilla.firefoxdeveloperedition`), title "Meet – abc-defg-hij".
`mic_probe watch` (the watcher and a `Notetaker`) with the page taking the mic for 2 s, letting go
for 6 s, holding it 20 s, letting go 5 s, holding 8 s: the 2 s use was ignored, the call prompt
came 3.2 s into the 20 s hold, the 5 s gap stayed the same call, and the call ended 10 s after
the last release.

### Settings tab (ui/settings.rs)

Rows (`settings::rows(&MeetingConfig, calendar_access)`, pure): a "Meetings" caption, then
label + muted line + control — Heads-up before meetings (pop-up: Off / At start / 15 s / 1 min /
5 min), Detect calls (switch), Maximum recording length (pop-up: 30 min / 1 h / 2 h / 3 h / 4 h)
— a hairline, and "Edit config.toml" (muted, "Dictation, model"
on the right) which opens the file. A value off the menu (hand-edited `max_minutes = 90`) shows
as "1 h 30 min" with nothing checked. With the heads-up on and Calendar refused, its line
becomes "Needs Calendar access · Open Settings" (opens `Pane::Calendars`). Pop-ups are a
bordered value + ▾ that opens a small opaque menu (`deferred(anchored())`, check on the current
value; outside click or Esc closes it). Switches are 28 × 16 monochrome pills. A change is shown
at once (applied to the popover's copy of the snapshot) and emitted as
`PopoverEvent::SetSetting`; the controller writes the file and reloads, and the next snapshot
carries what the file says. Nothing here reads files in render.

### Popover (ui/)

GPUI, calm, minimal, monochrome. Width 340 px.
Header: one state line ("Ready · fn to talk"; "Recording 0:07"; "Meeting · Weekly sync · 12:04";
first-run: "Downloading model 42%" with a hairline progress bar, "Needs Accessibility" / "Needs
Microphone" with an "Open Settings" button (`permissions::open_pane`); plus the config note or last
error as a muted line). A one-time hint line until dismissed: "Set Keyboard › Press 🌐 key to › Do
nothing" (button opens `Pane::Keyboard`). Tabs: **History | Meetings | Dictionary | Settings**
(text tabs, selected in primary ink). History: the 50 most recent dictations; row = "app · project" left, time
right, then the text clamped to 2 lines; click copies ("Copied" flashes). Meetings: title, "Sep 29
14:00 · 42:10" (and "recording" for the live one); click opens the file (`open`). Dictionary: the
entries; "Edit" opens dictionary.txt. Settings: see "Settings tab". Footer: "Record meeting ⌥M"
/ "Stop meeting ⌥M". Keys (`ui::nav`, pure and tested): ← → switch tabs (wrapping), ↑ ↓ move a
selection (the hover wash) through History, Meetings or Dictionary and scroll it into view,
Enter copies the dictation / opens the meeting / opens dictionary.txt, Esc closes an open
pop-up menu, else the popover. No ⌘-number or Tab shortcuts, and no hints on screen. `examples/popover_preview.rs` renders it with fixture
data (generic names, `you@example.com` if an email is ever needed).

Opening has to feel like a native menu (visible within a frame of the click), and gpui re-renders
the whole view on every scroll event and hover change, so nothing slow runs on the open path or
in `render`:
- The window is made once, hidden, at startup (`Panel` in main.rs) and only moved, shown
  (`makeKeyAndOrderFront`) and hidden (`orderOut`) after that. A new gpui window costs a Metal
  renderer (~270 ms the first time, ~10 ms after); a reused one draws its next frame synchronously
  as it becomes key.
- `PopoverData` is read on the background executor at startup, on every `revision` change and on
  each open (files can change behind the app's back); the view shows the last read at once.
- `launch_at_login::is_enabled()` is a cached flag. `SMAppService.status` is a synchronous XPC
  call (80–300 ms measured) and used to run in the footer on every render: ~3 fps scrolling and
  up to a second before the popover settled. It is refreshed in the background at startup and
  on each open; `set_enabled` runs in the background too.
- The Dictionary list is a `uniform_list`: only visible rows are laid out (2,000 entries: 20 ms
  per frame before, ~2 ms after).
  ↑ ↓ scroll it with `dictionary_scroll.scroll_to_item` (by index, since off-screen rows are not
  laid out); History and Meetings use the plain list's `scroll_to_item`.
- The Settings tab renders from the snapshot's `meeting_config` and `calendar_access`, which the
  controller thread reads (on startup, on each open's `RecheckPermissions`, after each write).
- Each show re-focuses the view before `makeKeyAndOrderFront`, so the arrow keys work at once;
  Esc, an outside click and focus loss hide the window, never close it.

`SQUAWK_LOG=debug` logs "popover shown N ms after the click"; gpui's `ZED_MEASUREMENTS=1` prints
every frame's duration to stderr.

### Permissions (permissions.rs)

Accessibility (`AXIsProcessTrusted`; the tap failing to create is the real test), Microphone
(`AVCaptureDevice authorizationStatusForMediaType: AVMediaTypeAudio`), Screen & System Audio
Recording (`CGPreflightScreenCaptureAccess`, checked only when a meeting with system audio starts),
Calendars (`calendar::access()`, EventKit full access; asked only while the heads-up is on, or on
the first meeting for its title). Panes: `Pane::url()`.

### Tests

Pure pieces: `MenuBarState::from_snapshot`, header text for each snapshot, popover row
formatting (time, clamp), the Info.plist generator if written in Rust, the log line formatter,
meeting title choice (explicit > calendar > "Meeting").

---

## squawk-cli

Binary `squawk`. Clap surface is done in `main.rs`; implement `commands.rs` and `tui/`. Files are
read directly (no app needed) except `status`, `meet start`, `meet stop`. When the app is needed
and not running: print `squawk is not running. Open Squawk.app first.` to stderr, exit 2. Other
errors: message to stderr, exit 1. Never print ANSI colours when stdout is not a tty.

| command | output |
|---|---|
| `squawk` | TUI |
| `squawk last` | the text only (pipe-friendly: `squawk last \| pbcopy`); nothing yet → stderr "No dictations yet.", exit 1 |
| `squawk history [--today] [--n N] [--json]` | newest first, default 20; a date line when the day changes, then `14:03  Ghostty · squawk` and the text indented two spaces, blank line between. `--json`: one object per line `{"at":"2026-09-29T14:03:12","app":"Ghostty","project":"squawk","text":"..."}` |
| `squawk meet start [--title T]` | `Recording "T" → <path>` |
| `squawk meet stop` | waits (up to 120 s, prints `Finishing…` to stderr) then `Saved "T" (42:10) → <path>` |
| `squawk meet list [--n N]` | `2026-09-29 14:00  42:10  Weekly sync` (+ `  recording`) |
| `squawk status [--json]` | `squawk 0.1.0 · idle` / `recording 0:07` / `transcribing`, then aligned lines `model`, `mic`, `accessibility`, `screen audio`, `meeting`, `config` (note if any); `--json` prints the `StatusInfo`. Not running → the not-running message + whether the model is installed, exit 2 |
| `squawk dict add <phrase…>` | `Added: <entry>` / `Already there: <entry>` |
| `squawk dict list` | one entry per line as written in the file |
| `squawk model download` | progress on stderr, one updating line `Downloading 42%  201/478 MB`; then `Model ready: <dir>`; already installed → `Model already installed: <dir>` |
| `squawk transcribe <file> [--raw] [--cwd DIR]` | loads the model (`load_model_blocking`), `audio::load_file`, `engine.transcribe`, `pipeline::finish` with the dictionary (and, with `--cwd`, a `Context` built from `Session{cwd, agent: Claude, pid: 0, tty: None}` + `RepoVocab::build`). Prints the cleaned text. `--raw` also prints `raw:`, `clean:`, and `audio 12.3s · model load 1.4s · transcribe 410ms`. Model missing → "Run `squawk model download` first.", exit 1 |

Clipboard: `pbcopy`. Editor: `$VISUAL`, else `$EDITOR`, else `open`.

### TUI

ratatui 0.29 + crossterm 0.28. Calm and
minimal: the terminal's default fg/bg and ANSI palette only (`Color::Reset`, named ANSI colours or
indices 0–15; **no RGB**), so it follows the terminal's theme. No boxes: at most a dim vertical rule
between list and preview.

- Top line: `History   Meetings` (selected bold, other dim); the active search query on the right.
- Left (~40%): History rows = `14:03  Ghostty · squawk` + first line of the text (dim), grouped
  under dim date lines; Meetings rows = title + dim `Sep 29 14:00 · 42:10` (+ `recording`).
- Right: History = full text wrapped, with the meta line above; Meetings = the file's transcript
  (speaker labels bold, timestamps dim), scrollable.
- Keys: `↑/↓` or `j/k` move, `g/G` top/bottom, `/` search (type to filter case-insensitively over
  text/app/project/title, `enter` keeps the filter, `esc` clears), `enter` copy (History: the text;
  Meetings: the transcript) with "Copied" flashed in the key bar, `o` open the meeting in the editor
  (leave the alternate screen while it runs), `tab` switch, `r` reload, `q`/`ctrl-c` quit.
  `ctrl-d/ctrl-u` scroll the preview.
- Key bar (one dim line at the bottom): `↑↓ move  / search  ⏎ copy  o open  tab switch  q quit`.
- Loads the 500 most recent dictations and all meetings; re-reads when files change (poll mtimes
  every 2 s) so a live meeting grows on screen.
- State and key handling in `tui/app.rs` with unit tests (filtering, selection clamping across
  filter changes, tab switching); drawing in `tui/ui.rs`; styles only in `tui/theme.rs`.

---

## Repository hygiene

Never committed: `CLAUDE.md`, `AGENTS.md`, `.claude/`, secrets, models, audio, or personal data;
test fixtures use generic names and `you@example.com`. `.gitignore` covers `target/`, models,
`.claude/`, `CLAUDE.md`, `AGENTS.md`.

## Implementation notes

- Binaries that link `squawk-engine` link the Swift runtime (ScreenCaptureKit's bridge
  references `@rpath/libswift_Concurrency.dylib`) and abort in dyld at launch without an rpath to
  `/usr/lib/swift`. Every crate that builds a binary linking the engine (engine tests/examples,
  app, CLI) has a `build.rs` with `cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift`.
- `squawk` TUI: `o` on a History entry opens that day's dictation file (Meetings: the
  meeting file). Clipboard is `pbcopy`, not a crate.
- Paste runs on the controller thread, not main: NSPasteboard and CGEventPost are
  thread-safe, and the hop would put main-thread rendering between fn-up and cmd+V. So
  `paste::paste(text, restore_after)` / `paste::copy(text)` take no `MainThreadMarker`. A paste
  inside another's restore window reuses the first one's saved clipboard. Our write carries
  `org.nspasteboard.TransientType` so clipboard managers skip it.
- The fn `Machine` lives in `hotkey::SharedMachine` (created in main, shared by the tap,
  its ticker and the controller): `HotkeyTap::start(SharedMachine, on_action)` and
  `Controller::spawn(.., hotkeys: SharedMachine, publish)`. `Input::Tick` comes from a small ticker
  thread sleeping until `Machine::deadline()` rather than a CFRunLoopTimer. Raw CGEvent → `Input`
  translation is `hotkey::driver` (pure, tested with the real flag values).
- `Snapshot` gained `meeting_saving`, `revision` (bumped on every file write so an open
  popover re-reads) and `paths`; `MeetingSnap` gained `started_at`. `Command` gained
  `TapInstalled(bool)`, `Engine(EngineEvent)`, `MeetingSaved(..)`, `Quit { done }`.
- Calendar titles: when calendar access was never asked, the first meeting requests it
  in the background and is titled "Meeting" (a prompt must not delay the recording).
- The popover is a fixed 480 px tall with a scrolling list (tabs never make the window
  jump). The footer also has "Launch at login" and "Quit Squawk". Opening the popover re-reads
  config.toml if its mtime changed, so edits from "Edit config.toml" apply without a relaunch.
- `squawk-app/build.rs` adds the `/usr/lib/swift` rpath (see the Swift runtime note above).
- `RepoVocab` gained a private lookup index (built by `build`, or lazily), so it
  can no longer be written as a struct literal outside core: use `RepoVocab::build(cwd)` or the new
  `RepoVocab::new(root, files, words)`. `VocabCache::clear()` added. `files` includes untracked
  (not ignored) files and CLAUDE.md / AGENTS.md / CONTEXT.md even when gitignored; because
  untracked files move nothing in git, `VocabCache` also rebuilds entries older than 60 s inside
  git (a cold build is ~20–50 ms, done at recording start). `words` holds only rewrite targets
  (package names, hyphenated Cargo deps, the project name, camelCase/digit stems, doc terms), not
  every stem. Capitalised doc terms that are ordinary English words per `/usr/share/dict/words`
  ("Dock", "Privacy") are never recased, only joined. `detect` reads the process table with
  `PROC_PIDT_SHORTBSDINFO` (the only flavour that works for root-owned `login` between the
  terminal and the shell) and uses tty atime (last typed into) to pick among tabs.
- Segmenter, for latency: dictation `max_segment` is 10 s (not 20), and the pause
  needed to cut shrinks by 80 ms per second buffered past `min_segment`, from `pause` (0.35 s) to
  a new `SegmenterConfig::min_pause` (0.15 s; meetings 0.5 → 0.25 s). The cut lands on the quiet
  middle of the pause (frames within 2× of its quietest), so a soft trailing consonant stays with
  its word. The noise floor is min(quietest frame of the last 4 s, a slow tracker), so talking
  does not lift it. `Segmenter::speculate(after_silence)` / `speculation()` / `take_tail()` and
  `Cut::speculated` were added: 0.2 s after speech stops, a dictation transcribes everything
  pending ahead of time; if no speech follows, that result stands in for the tail (or for a cut
  of the same audio), so a release a beat after the last word has ~0 ms left to do.
- Joined dictation segments get their seams mended (`dictation::join_segments`):
  "pauses. And each" → "pauses, and each" for a small list of continuing words; a capitalised
  function word after an unpunctuated segment is lowercased.
- Additive API: `DictationSession::level()` (0..1 meter), `Engine::
  start_dictation_replay(samples, speed)` (the bench/test path: a clip fed through the real
  session), `MicCapture::{start_with_errors, start_with_mode, echo_cancelled}`, `audio::MicMode`,
  `audio::{rms, resample_all}`, `store::drop_echoes`,
  `model::{install_tarball, DEFAULT_TARBALL_SIZE, DEFAULT_TARBALL_SHA256}` (the default tarball is
  size- and SHA-256-checked), `recognizer::{Backend, LoadState, parakeet_loader}`,
  `Recognizer::{spawn_with, submit_cancellable, state, wait_ready}`.
- ScreenCaptureKit runs with `excludes_current_process_audio = false`: "current
  process" is the whole responsible app, so from a terminal it silenced every program started in
  that terminal; Squawk itself makes no sound. SCK delivers continuous (silent) buffers.
- `model.threads` is accepted but ignored: `transcribe-rs` builds its ORT sessions
  itself. Model segment texts are re-spaced from the full text (`transcribe-rs` drops the space
  before numbers in segment text: "about700").
- Mic loss: cpal pauses the stream when the input device disappears or changes its nominal
  sample rate (AirPods do the latter the moment their mic opens, switching to the headset
  profile). `MicCapture` reopens the device (the named one, else the default) with a fresh
  resampler and keeps feeding the same sink, inserting silence for the gap so meeting time stays
  on the wall clock. Only when reopening fails does it report loss: a dictation then finishes
  (pastes what it has) via `EngineEvent::MicLost { session, .. }`, and a meeting keeps going with
  "mic lost" in the header (`EngineEvent::MeetingMicLost`). Dictation events carry the session id
  so a late event from a cancelled session is ignored. `MicLost` is also sent when the mic cannot
  be opened at dictation start; cpal xruns are not reported. A named `input_device` that is
  missing falls back to the default.
- Model download: each request reads the body for at most 60 s and the download resumes with a
  Range request as long as each attempt makes progress, so a silent connection cannot hang it.
  An exclusive lock on `<models_dir>/<dir>.lock` keeps the app and `squawk model download` from
  writing the same partial file; the second one waits and finds the model installed. The app
  retries a missing or failed model whenever the popover opens or fn is pressed.
- `squawk reload` sends the IPC `reload` request. Bare extension words accept the
  same model spellings as after "dot" ("cargo tomel" → `@Cargo.toml`).
  `crates/squawk-engine/tests/end_to_end.rs` (ignored; needs the model) replays `say` speech in
  real time through the streaming session, then the full pipeline with a temp git repo as cwd.
