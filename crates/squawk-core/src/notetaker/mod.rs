//! The notetaker: the three `[meeting]` settings the popover's Settings tab
//! edits, and the pure state machine behind them.
//!
//! - **Heads-up** before a calendar event with other people or a call link
//!   ([`heads_up`]): a prompt with Record / Not now.
//! - **Detect calls** ([`calls`]): when a call app has used the mic for 3 s,
//!   a prompt with Start / Not now. Not now (or ignoring it) holds for the
//!   rest of that call.
//! - **Maximum recording length**: a prompt two minutes before (Keep going
//!   adds 30 minutes), then stop and save, then a brief "Saved notes" prompt
//!   with Open.
//!
//! Otherwise a meeting stops only when the user stops it (⌥M, the popover,
//! `squawk meet stop`). A call app letting go of the mic never stops one:
//! some do that on mute, mid-call.
//!
//! [`Notetaker`] takes explicit times (a monotonic `Instant` for the timers,
//! wall-clock `DateTime<Local>` for the calendar), so every rule is tested
//! with synthetic timestamps. The app feeds it once a second and after every
//! change, shows [`Notetaker::prompt`] in a small panel under the menu bar
//! icon, and does what [`Notetaker::tick`] and [`Notetaker::reply`] return.

pub mod calls;
pub mod heads_up;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};

use crate::config::MeetingConfig;
use calls::{CallEvent, CallId, CallTracker};
use heads_up::UpcomingEvent;

/// The heads-up choices, in seconds before the start (-1 = off).
pub const HEADS_UP_CHOICES: [i64; 5] = [-1, 0, 15, 60, 300];
/// The maximum-length choices, in minutes.
pub const MAX_LENGTH_CHOICES: [u64; 5] = [30, 60, 120, 180, 240];
/// How long before the maximum length the warning appears.
pub const WARN_BEFORE: Duration = Duration::from_secs(120);
/// What Keep going adds.
pub const EXTEND_BY: Duration = Duration::from_secs(30 * 60);
/// How long a call prompt waits for an answer (no answer = not now).
pub const CALL_PROMPT_FOR: Duration = Duration::from_secs(20);
/// The least time a heads-up stays up.
pub const HEADS_UP_MIN_FOR: Duration = Duration::from_secs(30);
/// How long "Saved notes" stays up.
pub const SAVED_FOR: Duration = Duration::from_secs(8);

/// "Off", "At start", "15 s", "1 min", "5 min".
pub fn heads_up_label(secs: i64) -> String {
    match secs {
        s if s < 0 => "Off".into(),
        0 => "At start".into(),
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

/// "30 min", "1 h", "2 h", "1 h 30 min".
pub fn max_length_label(minutes: u64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// One Settings-tab change: the `[meeting]` key it writes and its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    HeadsUpSecs(i64),
    DetectCalls(bool),
    MaxMinutes(u64),
}

impl Setting {
    pub fn key(self) -> &'static str {
        match self {
            Setting::HeadsUpSecs(_) => "heads_up_secs",
            Setting::DetectCalls(_) => "detect_calls",
            Setting::MaxMinutes(_) => "max_minutes",
        }
    }

    pub fn value(self) -> toml_edit::Value {
        match self {
            Setting::HeadsUpSecs(v) => v.into(),
            Setting::MaxMinutes(v) => (v as i64).into(),
            Setting::DetectCalls(v) => v.into(),
        }
    }

    /// Apply to an in-memory config (what the file will say once written).
    pub fn apply(self, meeting: &mut MeetingConfig) {
        match self {
            Setting::HeadsUpSecs(v) => meeting.heads_up_secs = v,
            Setting::DetectCalls(v) => meeting.detect_calls = v,
            Setting::MaxMinutes(v) => meeting.max_minutes = v,
        }
    }
}

/// The settings, in the units the state machine uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// `None` = no heads-up.
    pub heads_up: Option<Duration>,
    pub detect_calls: bool,
    pub max_length: Duration,
}

impl From<&MeetingConfig> for Settings {
    fn from(m: &MeetingConfig) -> Self {
        Settings {
            heads_up: u64::try_from(m.heads_up_secs).ok().map(Duration::from_secs),
            detect_calls: m.detect_calls,
            max_length: Duration::from_secs(m.max_minutes * 60),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings::from(&MeetingConfig::default())
    }
}

/// Why a meeting stopped on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// It reached the maximum length.
    MaxLength,
}

/// What the panel under the menu bar icon shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Prompt {
    /// Record / Not now.
    HeadsUp {
        key: String,
        title: String,
        start: DateTime<Local>,
        end: DateTime<Local>,
    },
    /// Start / Not now.
    Call { call: CallId, app: String },
    /// Keep going +30 min.
    StoppingSoon { title: String, limit: Duration },
    /// Open.
    Saved {
        title: String,
        path: PathBuf,
        length_secs: u64,
        reason: StopReason,
    },
}

/// A button on the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// Record / Start / Keep going / Open.
    Accept,
    /// Not now.
    Dismiss,
}

/// What the app should do after a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Nothing,
    /// Start a meeting. `title` is the event's; without one the app picks
    /// the calendar's current event, else `fallback`.
    StartMeeting {
        title: Option<String>,
        fallback: String,
        call: Option<CallId>,
    },
    /// The limit moved (nothing else to do).
    Extended,
    /// Open this meeting file.
    Open(PathBuf),
}

#[derive(Debug, Clone)]
struct Shown {
    prompt: Prompt,
    /// Taken down at this time, if ever.
    until: Option<Instant>,
}

#[derive(Debug, Clone)]
struct Watch {
    title: String,
    started: Instant,
    limit: Duration,
    warned: bool,
}

#[derive(Debug, Clone)]
pub struct Notetaker {
    settings: Settings,
    tracker: CallTracker,
    shown: Option<Shown>,
    meeting: Option<Watch>,
    /// Calls already offered (answered or not).
    offered: HashSet<CallId>,
    /// Events already offered.
    heads_up_done: HashSet<String>,
}

impl Notetaker {
    pub fn new(settings: Settings) -> Notetaker {
        Notetaker {
            settings,
            tracker: CallTracker::default(),
            shown: None,
            meeting: None,
            offered: HashSet::new(),
            heads_up_done: HashSet::new(),
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// New settings from config.toml. A prompt the change turns off goes
    /// away; a running meeting takes the new maximum length.
    pub fn set_settings(&mut self, settings: Settings) {
        let old = self.settings;
        self.settings = settings;
        let hide = match self.shown.as_ref().map(|s| &s.prompt) {
            Some(Prompt::Call { .. }) => !settings.detect_calls,
            Some(Prompt::HeadsUp { .. }) => settings.heads_up.is_none(),
            _ => false,
        };
        if hide {
            self.shown = None;
        }
        if old.max_length != settings.max_length {
            if let Some(m) = self.meeting.as_mut() {
                m.limit = settings.max_length;
                m.warned = false;
                if matches!(
                    self.shown.as_ref().map(|s| &s.prompt),
                    Some(Prompt::StoppingSoon { .. })
                ) {
                    self.shown = None;
                }
            }
        }
    }

    /// What the panel shows now.
    pub fn prompt(&self) -> Option<&Prompt> {
        self.shown.as_ref().map(|s| &s.prompt)
    }

    /// Whether the mic needs watching at all.
    pub fn wants_mic(&self) -> bool {
        self.settings.detect_calls
    }

    /// Whether the calendar needs reading.
    pub fn wants_calendar(&self) -> bool {
        self.settings.heads_up.is_some()
    }

    fn show(&mut self, prompt: Prompt, until: Option<Instant>) {
        self.shown = Some(Shown { prompt, until });
    }

    /// Advance to `now`: `mic` is the call apps using the mic, `events` the
    /// calendar around now. Returns a reason when the running meeting
    /// should stop.
    pub fn tick(
        &mut self,
        now: Instant,
        wall: DateTime<Local>,
        mic: &[String],
        events: &[UpcomingEvent],
    ) -> Option<StopReason> {
        let mut stop = None;

        for event in self.tracker.update(now, mic) {
            match event {
                // A call that starts during a meeting is not offered: the
                // notes are already running.
                CallEvent::Started(call) => {
                    if self.meeting.is_none()
                        && self.settings.detect_calls
                        && self.offered.insert(call.id)
                    {
                        let app = call.app.clone();
                        self.show(
                            Prompt::Call { call: call.id, app },
                            Some(now + CALL_PROMPT_FOR),
                        );
                    }
                }
                CallEvent::Ended(call) => {
                    self.offered.remove(&call.id);
                    if matches!(self.prompt(), Some(Prompt::Call { call: id, .. }) if *id == call.id)
                    {
                        self.shown = None;
                    }
                }
            }
        }

        if let Some(m) = self.meeting.as_mut() {
            let elapsed = now.saturating_duration_since(m.started);
            if elapsed >= m.limit {
                stop = Some(StopReason::MaxLength);
            } else if !m.warned && elapsed + WARN_BEFORE >= m.limit {
                m.warned = true;
                let prompt = Prompt::StoppingSoon {
                    title: m.title.clone(),
                    limit: m.limit,
                };
                self.show(prompt, None);
            }
        }

        if let (Some(lead), None) = (self.settings.heads_up, &self.meeting) {
            let lead = chrono::Duration::from_std(lead).unwrap_or_default();
            let done = &self.heads_up_done;
            if let Some(event) = heads_up::due(events, wall, lead, |k| done.contains(k)) {
                self.heads_up_done.insert(event.key.clone());
                let left = (event.start + heads_up::LATE - wall)
                    .to_std()
                    .unwrap_or_default()
                    .max(HEADS_UP_MIN_FOR);
                let prompt = Prompt::HeadsUp {
                    key: event.key.clone(),
                    title: event.title.trim().to_string(),
                    start: event.start,
                    end: event.end,
                };
                self.show(prompt, Some(now + left));
            }
        }

        if self
            .shown
            .as_ref()
            .and_then(|s| s.until)
            .is_some_and(|until| now >= until)
        {
            self.shown = None;
        }
        stop
    }

    /// A button on the panel.
    pub fn reply(&mut self, reply: Reply) -> Outcome {
        let Some(shown) = self.shown.take() else {
            return Outcome::Nothing;
        };
        if reply == Reply::Dismiss {
            return Outcome::Nothing;
        }
        match shown.prompt {
            Prompt::HeadsUp { title, .. } => Outcome::StartMeeting {
                fallback: title.clone(),
                title: Some(title),
                call: None,
            },
            Prompt::Call { call, app } => Outcome::StartMeeting {
                title: None,
                fallback: format!("{app} call"),
                call: Some(call),
            },
            Prompt::StoppingSoon { .. } => {
                if let Some(m) = self.meeting.as_mut() {
                    m.limit += EXTEND_BY;
                    m.warned = false;
                }
                Outcome::Extended
            }
            Prompt::Saved { path, .. } => Outcome::Open(path),
        }
    }

    /// A meeting started (from a prompt, ⌥M, the popover or the CLI). The
    /// call it was started from (`call`, else whichever call has the mic
    /// now) counts as offered, so it is not offered again after the
    /// meeting stops.
    pub fn meeting_started(&mut self, now: Instant, title: &str, call: Option<CallId>) {
        let call = call
            .and_then(|id| self.tracker.get(id))
            .or_else(|| self.tracker.current())
            .map(|c| c.id);
        if let Some(id) = call {
            self.offered.insert(id);
        }
        self.meeting = Some(Watch {
            title: title.to_string(),
            started: now,
            limit: self.settings.max_length,
            warned: false,
        });
        // Whatever the panel was offering, the user now has notes running.
        self.shown = None;
    }

    /// The meeting stopped (for any reason).
    pub fn meeting_stopped(&mut self) {
        self.meeting = None;
        if matches!(self.prompt(), Some(Prompt::StoppingSoon { .. })) {
            self.shown = None;
        }
    }

    /// A meeting that stopped on its own finished saving: say so briefly.
    pub fn saved(
        &mut self,
        now: Instant,
        title: String,
        path: PathBuf,
        length_secs: u64,
        reason: StopReason,
    ) {
        self.show(
            Prompt::Saved {
                title,
                path,
                length_secs,
                reason,
            },
            Some(now + SAVED_FOR),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn mins(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }

    fn wall(h: u32, m: u32, s: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 30, h, m, s).unwrap()
    }

    fn settings() -> Settings {
        Settings::default()
    }

    fn zoom() -> Vec<String> {
        vec!["Zoom".to_string()]
    }

    fn event(title: &str, start: DateTime<Local>) -> UpcomingEvent {
        UpcomingEvent {
            key: format!("{title}-{}", start.timestamp()),
            title: title.into(),
            start,
            end: start + chrono::Duration::minutes(30),
            all_day: false,
            other_attendees: true,
            video_link: true,
            declined: false,
            cancelled: false,
        }
    }

    /// Drive the machine second by second with the same mic sample.
    struct Clock {
        t0: Instant,
        w0: DateTime<Local>,
        at: u64,
    }

    impl Clock {
        fn new() -> Clock {
            Clock {
                t0: Instant::now(),
                w0: wall(14, 0, 0),
                at: 0,
            }
        }
        fn now(&self) -> Instant {
            self.t0 + secs(self.at)
        }
        /// Tick every second up to and including `to`; the first stop wins.
        fn run(
            &mut self,
            n: &mut Notetaker,
            to: u64,
            mic: &[String],
            events: &[UpcomingEvent],
        ) -> Option<StopReason> {
            let mut stop = None;
            while self.at < to {
                self.at += 1;
                let w = self.w0 + chrono::Duration::seconds(self.at as i64);
                let s = n.tick(self.now(), w, mic, events);
                if stop.is_none() {
                    stop = s;
                }
            }
            stop
        }
    }

    #[test]
    fn labels_read_like_the_menu() {
        let heads: Vec<_> = HEADS_UP_CHOICES
            .iter()
            .map(|&s| heads_up_label(s))
            .collect();
        assert_eq!(heads, ["Off", "At start", "15 s", "1 min", "5 min"]);
        let max: Vec<_> = MAX_LENGTH_CHOICES
            .iter()
            .map(|&m| max_length_label(m))
            .collect();
        assert_eq!(max, ["30 min", "1 h", "2 h", "3 h", "4 h"]);
        assert_eq!(heads_up_label(30), "30 s");
        assert_eq!(max_length_label(90), "1 h 30 min");
    }

    #[test]
    fn settings_come_from_the_config() {
        let s = Settings::default();
        assert_eq!(s.heads_up, Some(secs(15)));
        assert!(s.detect_calls);
        assert_eq!(s.max_length, mins(120));
        let mut m = MeetingConfig::default();
        Setting::HeadsUpSecs(-1).apply(&mut m);
        Setting::MaxMinutes(30).apply(&mut m);
        assert_eq!(Settings::from(&m).heads_up, None);
        assert_eq!(Settings::from(&m).max_length, mins(30));
        assert_eq!(Setting::MaxMinutes(30).key(), "max_minutes");
        assert_eq!(Setting::DetectCalls(false).value().as_bool(), Some(false));
        assert_eq!(Setting::HeadsUpSecs(-1).value().as_integer(), Some(-1));
    }

    #[test]
    fn a_call_is_offered_once_after_three_seconds() {
        let mut n = Notetaker::new(settings());
        let mut c = Clock::new();
        // The first sample is at 1 s; three seconds of mic later, the prompt.
        c.run(&mut n, 3, &zoom(), &[]);
        assert_eq!(n.prompt(), None);
        c.run(&mut n, 4, &zoom(), &[]);
        assert!(matches!(n.prompt(), Some(Prompt::Call { app, .. }) if app == "Zoom"));
    }

    #[test]
    fn not_now_holds_for_the_rest_of_that_call() {
        let mut n = Notetaker::new(settings());
        let mut c = Clock::new();
        c.run(&mut n, 4, &zoom(), &[]);
        assert!(n.prompt().is_some());
        assert_eq!(n.reply(Reply::Dismiss), Outcome::Nothing);
        assert_eq!(n.prompt(), None);
        c.run(&mut n, 60, &zoom(), &[]);
        assert_eq!(n.prompt(), None);
        // A short break (switching AirPods) is still that call.
        c.run(&mut n, 65, &[], &[]);
        c.run(&mut n, 120, &zoom(), &[]);
        assert_eq!(n.prompt(), None);
        // A new call after this one ended is offered again.
        c.run(&mut n, 140, &[], &[]);
        c.run(&mut n, 150, &zoom(), &[]);
        assert!(matches!(n.prompt(), Some(Prompt::Call { .. })));
    }

    #[test]
    fn an_unanswered_call_prompt_goes_away_and_counts_as_not_now() {
        let mut n = Notetaker::new(settings());
        let mut c = Clock::new();
        c.run(&mut n, 4, &zoom(), &[]);
        c.run(&mut n, 23, &zoom(), &[]);
        assert!(n.prompt().is_some());
        c.run(&mut n, 24, &zoom(), &[]);
        assert_eq!(n.prompt(), None);
        c.run(&mut n, 200, &zoom(), &[]);
        assert_eq!(n.prompt(), None);
    }

    #[test]
    fn detect_calls_off_offers_nothing() {
        let mut n = Notetaker::new(Settings {
            detect_calls: false,
            ..settings()
        });
        let mut c = Clock::new();
        c.run(&mut n, 30, &zoom(), &[]);
        assert_eq!(n.prompt(), None);
        // Nothing else needs the mic watched.
        assert!(!n.wants_mic());
    }

    #[test]
    fn a_call_that_ends_takes_its_prompt_with_it() {
        let mut n = Notetaker::new(settings());
        let mut c = Clock::new();
        c.run(&mut n, 4, &zoom(), &[]);
        assert!(n.prompt().is_some());
        c.run(&mut n, 14, &[], &[]);
        assert!(n.prompt().is_some(), "not over until 10 s after");
        c.run(&mut n, 15, &[], &[]);
        assert_eq!(n.prompt(), None);
    }

    #[test]
    fn a_meeting_started_from_the_call_prompt_outlasts_the_call() {
        let mut n = Notetaker::new(settings());
        let mut c = Clock::new();
        c.run(&mut n, 4, &zoom(), &[]);
        let Outcome::StartMeeting {
            title,
            fallback,
            call,
        } = n.reply(Reply::Accept)
        else {
            panic!("expected a start");
        };
        assert_eq!(title, None);
        assert_eq!(fallback, "Zoom call");
        n.meeting_started(c.now(), "Zoom call", call);

        assert_eq!(c.run(&mut n, 600, &zoom(), &[]), None);
        // Zoom lets go of the mic (muted, say): the meeting keeps recording.
        assert_eq!(c.run(&mut n, 900, &[], &[]), None);
        assert_eq!(n.prompt(), None);
        // Stopped by hand while Zoom still has the mic: not offered again.
        c.run(&mut n, 910, &zoom(), &[]);
        n.meeting_stopped();
        c.run(&mut n, 1000, &zoom(), &[]);
        assert_eq!(n.prompt(), None);
    }

    #[test]
    fn saved_goes_away_on_its_own() {
        let mut n = Notetaker::new(settings());
        let mut c = Clock::new();
        n.saved(
            c.now(),
            "M".into(),
            "/m.md".into(),
            1,
            StopReason::MaxLength,
        );
        assert!(matches!(n.prompt(), Some(Prompt::Saved { .. })));
        c.run(&mut n, 7, &[], &[]);
        assert!(n.prompt().is_some());
        c.run(&mut n, 8, &[], &[]);
        assert_eq!(n.prompt(), None);
    }

    #[test]
    fn a_manual_meeting_takes_the_call_prompt_down_and_ignores_the_mic() {
        let mut n = Notetaker::new(settings());
        let mut c = Clock::new();
        c.run(&mut n, 5, &zoom(), &[]);
        // ⌥M while the call prompt is up: the prompt goes, notes run.
        n.meeting_started(c.now(), "Weekly sync", None);
        assert_eq!(n.prompt(), None);
        c.run(&mut n, 100, &zoom(), &[]);
        assert_eq!(c.run(&mut n, 400, &[], &[]), None);
    }

    #[test]
    fn a_call_that_starts_during_a_meeting_is_not_offered() {
        let mut n = Notetaker::new(settings());
        let mut c = Clock::new();
        n.meeting_started(c.now(), "Meeting", None);
        c.run(&mut n, 60, &[], &[]);
        c.run(&mut n, 70, &zoom(), &[]);
        assert_eq!(n.prompt(), None);
    }

    #[test]
    fn a_meeting_with_no_call_never_stops_for_one() {
        let mut n = Notetaker::new(settings());
        let mut c = Clock::new();
        n.meeting_started(c.now(), "In person", None);
        assert_eq!(c.run(&mut n, 3000, &[], &[]), None);
    }

    #[test]
    fn warns_two_minutes_before_the_limit_then_stops() {
        let mut n = Notetaker::new(Settings {
            max_length: mins(30),
            ..settings()
        });
        let mut c = Clock::new();
        n.meeting_started(c.now(), "Planning", None);
        c.run(&mut n, 28 * 60 - 1, &[], &[]);
        assert_eq!(n.prompt(), None);
        c.run(&mut n, 28 * 60, &[], &[]);
        assert_eq!(
            n.prompt(),
            Some(&Prompt::StoppingSoon {
                title: "Planning".into(),
                limit: mins(30)
            })
        );
        assert_eq!(c.run(&mut n, 30 * 60 - 1, &[], &[]), None);
        assert!(n.prompt().is_some(), "stays up until the stop");
        assert_eq!(
            c.run(&mut n, 30 * 60, &[], &[]),
            Some(StopReason::MaxLength)
        );
        n.meeting_stopped();
        assert_eq!(n.prompt(), None);
    }

    #[test]
    fn keep_going_adds_thirty_minutes_and_warns_again() {
        let mut n = Notetaker::new(Settings {
            max_length: mins(60),
            ..settings()
        });
        let mut c = Clock::new();
        n.meeting_started(c.now(), "Offsite", None);
        c.run(&mut n, 58 * 60, &[], &[]);
        assert_eq!(n.reply(Reply::Accept), Outcome::Extended);
        assert_eq!(n.prompt(), None);
        assert_eq!(c.run(&mut n, 88 * 60 - 1, &[], &[]), None);
        assert_eq!(n.prompt(), None);
        c.run(&mut n, 88 * 60, &[], &[]);
        assert!(matches!(
            n.prompt(),
            Some(Prompt::StoppingSoon { limit, .. }) if *limit == mins(90)
        ));
        assert_eq!(
            c.run(&mut n, 90 * 60, &[], &[]),
            Some(StopReason::MaxLength)
        );
    }

    #[test]
    fn a_shorter_limit_set_mid_meeting_applies_to_it() {
        let mut n = Notetaker::new(settings());
        let mut c = Clock::new();
        n.meeting_started(c.now(), "M", None);
        c.run(&mut n, 40 * 60, &[], &[]);
        n.set_settings(Settings {
            max_length: mins(30),
            ..settings()
        });
        assert_eq!(
            c.run(&mut n, 40 * 60 + 1, &[], &[]),
            Some(StopReason::MaxLength)
        );
    }

    #[test]
    fn heads_up_fifteen_seconds_before_then_record() {
        let mut n = Notetaker::new(settings());
        let mut c = Clock::new();
        let events = [event("Design review", wall(14, 1, 0))];
        c.run(&mut n, 44, &[], &events);
        assert_eq!(n.prompt(), None);
        c.run(&mut n, 45, &[], &events);
        assert!(
            matches!(n.prompt(), Some(Prompt::HeadsUp { title, .. }) if title == "Design review")
        );
        assert_eq!(
            n.reply(Reply::Accept),
            Outcome::StartMeeting {
                title: Some("Design review".into()),
                fallback: "Design review".into(),
                call: None
            }
        );
        n.meeting_started(c.now(), "Design review", None);
        // Offered once only, and never during a meeting.
        c.run(&mut n, 90, &[], &events);
        assert_eq!(n.prompt(), None);
    }

    #[test]
    fn heads_up_not_now_is_final_and_it_expires_after_the_start() {
        let mut n = Notetaker::new(settings());
        let mut c = Clock::new();
        let events = [
            event("Standup", wall(14, 1, 0)),
            event("Retro", wall(14, 10, 0)),
        ];
        c.run(&mut n, 45, &[], &events);
        n.reply(Reply::Dismiss);
        c.run(&mut n, 100, &[], &events);
        assert_eq!(n.prompt(), None);
        // Unanswered: gone a minute after the start.
        c.run(&mut n, 9 * 60 + 45, &[], &events);
        assert!(matches!(n.prompt(), Some(Prompt::HeadsUp { title, .. }) if title == "Retro"));
        c.run(&mut n, 11 * 60 - 1, &[], &events);
        assert!(n.prompt().is_some());
        c.run(&mut n, 11 * 60, &[], &events);
        assert_eq!(n.prompt(), None);
    }

    #[test]
    fn heads_up_off_offers_nothing() {
        let mut n = Notetaker::new(Settings {
            heads_up: None,
            ..settings()
        });
        let mut c = Clock::new();
        c.run(&mut n, 120, &[], &[event("Standup", wall(14, 1, 0))]);
        assert_eq!(n.prompt(), None);
        assert!(!n.wants_calendar());
    }

    #[test]
    fn turning_a_setting_off_takes_its_prompt_down() {
        let mut n = Notetaker::new(settings());
        let mut c = Clock::new();
        c.run(&mut n, 4, &zoom(), &[]);
        assert!(n.prompt().is_some());
        n.set_settings(Settings {
            detect_calls: false,
            ..settings()
        });
        assert_eq!(n.prompt(), None);
    }

    #[test]
    fn a_reply_with_nothing_shown_does_nothing() {
        let mut n = Notetaker::new(settings());
        assert_eq!(n.reply(Reply::Accept), Outcome::Nothing);
    }
}
