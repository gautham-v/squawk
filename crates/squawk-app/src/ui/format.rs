//! The words the popover shows, as pure functions of a snapshot and a clock,
//! so every header state and row label is tested without a window.

use std::time::Instant;

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime};
use squawk_core::notetaker::heads_up::UpcomingEvent;
use squawk_core::notetaker::Settings;
use squawk_core::status::format_elapsed;
use squawk_core::store::MeetingSummary;
use squawk_core::ModelStatus;

use crate::controller::{DictationPhase, Snapshot};
use crate::permissions::Pane;

/// How the state line is inked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Primary,
    /// Copper: recording or a meeting.
    Accent,
    Secondary,
}

/// The header: one state line, maybe a fix button, maybe a progress bar,
/// maybe a muted note under it.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub text: String,
    pub tone: Tone,
    /// A button beside the state line that opens a System Settings pane.
    pub fix: Option<Pane>,
    /// 0..=1 while the model downloads.
    pub progress: Option<f32>,
    pub note: Option<String>,
}

pub const READY_LINE: &str = "Ready · fn to talk";
pub const FIX_LABEL: &str = "Open Settings";

pub fn header(snap: &Snapshot, now: Instant) -> Header {
    let note = snap.last_error.clone().or_else(|| snap.config_note.clone());
    let line = |text: String, tone: Tone| Header {
        text,
        tone,
        fix: None,
        progress: None,
        note: note.clone(),
    };

    match &snap.dictation {
        DictationPhase::Recording { since, hands_free } => {
            let elapsed = format_elapsed(now.saturating_duration_since(*since).as_secs());
            let suffix = if *hands_free { " · hands-free" } else { "" };
            return line(format!("Recording {elapsed}{suffix}"), Tone::Accent);
        }
        DictationPhase::Transcribing => return line("Transcribing…".into(), Tone::Secondary),
        DictationPhase::Idle => {}
    }

    if snap.permissions.accessibility == Some(false) {
        return Header {
            fix: Some(Pane::Accessibility),
            ..line("Needs Accessibility".into(), Tone::Primary)
        };
    }
    if snap.permissions.microphone == Some(false) {
        return Header {
            fix: Some(Pane::Microphone),
            ..line("Needs Microphone".into(), Tone::Primary)
        };
    }

    if let Some(meeting) = &snap.meeting {
        let elapsed = format_elapsed(now.saturating_duration_since(meeting.since).as_secs());
        let fix =
            (snap.permissions.screen_recording == Some(false)).then_some(Pane::ScreenRecording);
        let lost = if meeting.mic_lost { " · mic lost" } else { "" };
        return Header {
            fix,
            ..line(
                format!("Meeting · {} · {elapsed}{lost}", meeting.title),
                Tone::Accent,
            )
        };
    }
    if snap.meeting_saving {
        return line("Saving meeting…".into(), Tone::Secondary);
    }

    match &snap.model {
        ModelStatus::Ready => line(READY_LINE.into(), Tone::Primary),
        status @ ModelStatus::Downloading { .. } => Header {
            progress: Some(status.progress().unwrap_or(0.0)),
            ..line(status.label(), Tone::Primary)
        },
        status => line(status.label(), Tone::Primary),
    }
}

/// The time on a History row: "14:03" today, "Yesterday", "Sep 28" this
/// year, "2025-09-28" before that.
pub fn row_time(at: NaiveDateTime, today: NaiveDate) -> String {
    let day = at.date();
    if day == today {
        at.format("%H:%M").to_string()
    } else if today.pred_opt() == Some(day) {
        "Yesterday".into()
    } else if day.year() == today.year() {
        at.format("%b %-d").to_string()
    } else {
        at.format("%Y-%m-%d").to_string()
    }
}

/// A meeting row's second line: "Sep 29 14:00 · 42:10", plus " · recording"
/// for the live one.
pub fn meeting_meta(meeting: &MeetingSummary) -> String {
    let mut parts = Vec::new();
    if let Some(at) = meeting.started_at {
        parts.push(at.format("%b %-d %H:%M").to_string());
    }
    parts.push(format_elapsed(meeting.length_secs));
    if meeting.in_progress {
        parts.push("recording".into());
    }
    parts.join(" · ")
}

/// The footer's meeting row.
pub fn meeting_action(recording: bool) -> &'static str {
    if recording {
        "Stop meeting"
    } else {
        "Record meeting"
    }
}

/// The Meetings tab's pinned row: what is on now or next, or why it cannot
/// say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextRow {
    /// A meeting is recording: "Now", its title, "Recording · 4:12 · Zoom".
    Now { title: String, meta: String },
    /// The next calendar meeting: "Next", "in 25 min", its title,
    /// "14:30–15:00 · Google Meet · will record".
    Next {
        when: String,
        title: String,
        meta: String,
    },
    /// No meeting to show: a line, maybe a muted note and a fix.
    Line {
        text: &'static str,
        note: Option<&'static str>,
        fix: Option<Pane>,
    },
}

pub const NEXT_LABEL: &str = "Next";
pub const NOW_LABEL: &str = "Now";
pub const NO_CALENDAR: &str = "Can\u{2019}t see your calendar";
pub const NO_CALENDAR_NOTE: &str = "Calendar access is off";
pub const ASKING_CALENDAR: &str = "Asking for Calendar access\u{2026}";
pub const NO_MEETINGS: &str = "No meetings today or tomorrow";

/// The pinned row for `snap` at `now` (`wall` is the same moment on the
/// calendar).
pub fn next_row(snap: &Snapshot, now: Instant, wall: DateTime<Local>) -> NextRow {
    if let Some(meeting) = &snap.meeting {
        let elapsed = format_elapsed(now.saturating_duration_since(meeting.since).as_secs());
        let mut meta = format!("Recording · {elapsed}");
        if let Some(app) = &meeting.app {
            meta.push_str(&format!(" · {app}"));
        }
        return NextRow::Now {
            title: meeting.title.clone(),
            meta,
        };
    }
    match (snap.calendar_access, &snap.next_meeting) {
        (Some(false), _) => NextRow::Line {
            text: NO_CALENDAR,
            note: Some(NO_CALENDAR_NOTE),
            fix: Some(Pane::Calendars),
        },
        (None, _) => NextRow::Line {
            text: ASKING_CALENDAR,
            note: None,
            fix: None,
        },
        (Some(true), None) => NextRow::Line {
            text: NO_MEETINGS,
            note: None,
            fix: None,
        },
        (Some(true), Some(event)) => NextRow::Next {
            when: next_when(event.start, event.end, wall),
            title: event.title.trim().to_string(),
            meta: next_meta(event, &Settings::from(&snap.meeting_config)),
        },
    }
}

/// "now", "in 1 min", "in 25 min", "in 2 h", "tomorrow", "Mon".
pub fn next_when(start: DateTime<Local>, end: DateTime<Local>, now: DateTime<Local>) -> String {
    if start <= now {
        return if now < end {
            "now".into()
        } else {
            String::new()
        };
    }
    let today = now.date_naive();
    if start.date_naive() == today {
        let secs = (start - now).num_seconds();
        let mins = (secs + 59) / 60;
        return if mins < 60 {
            format!("in {mins} min")
        } else {
            format!("in {} h", mins / 60)
        };
    }
    if today.succ_opt() == Some(start.date_naive()) {
        return "tomorrow".into();
    }
    start.format("%a").to_string()
}

/// "14:30–15:00 · Google Meet · will record": the time, the call app when
/// the event has a link, and what squawk will do (record on its own, or
/// offer a heads-up), when anything.
pub fn next_meta(event: &UpcomingEvent, settings: &Settings) -> String {
    let mut parts = vec![format!(
        "{}–{}",
        event.start.format("%H:%M"),
        event.end.format("%H:%M")
    )];
    if let Some(app) = event.call_app {
        parts.push(app.to_string());
    }
    if settings.will_record(event) {
        parts.push("will record".into());
    } else if settings.heads_up.is_some() {
        parts.push("heads-up".into());
    }
    parts.join(" · ")
}

/// The ready line under the pinned row: whether squawk can see the
/// calendar, notice calls and record the other side (left out when
/// system audio is off).
pub fn ready_checks(snap: &Snapshot) -> Vec<(&'static str, bool)> {
    let mut checks = vec![
        ("Calendar", snap.calendar_access == Some(true)),
        ("Call detection", snap.meeting_config.detect_calls),
    ];
    if snap.meeting_config.system_audio {
        checks.push((
            "System audio",
            snap.permissions.screen_recording == Some(true),
        ));
    }
    checks
}

/// "✓ Calendar", "✕ System audio".
pub fn ready_check(name: &str, ok: bool) -> String {
    format!("{} {name}", if ok { "\u{2713}" } else { "\u{2715}" })
}

/// The meeting shortcut, as the footer shows it.
pub const MEETING_SHORTCUT: &str = "⌥M";

/// Between the spoken and the written side of a dictionary replacement.
pub const REPLACE_ARROW: &str = "→";

/// The count beside "Edit dictionary.txt".
pub fn dictionary_count(entries: usize) -> String {
    match entries {
        1 => "1 entry".to_string(),
        n => format!("{n} entries"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::MeetingSnap;
    use chrono::{Local, TimeZone};
    use squawk_core::status::Permissions;
    use std::time::Duration;

    fn ready() -> Snapshot {
        let mut s = Snapshot::initial(
            crate::controller::tests::test_paths(),
            ModelStatus::Ready,
            None,
        );
        s.permissions = Permissions {
            accessibility: Some(true),
            microphone: Some(true),
            screen_recording: None,
        };
        s
    }

    #[test]
    fn dictionary_count_is_singular_for_one() {
        assert_eq!(dictionary_count(1), "1 entry");
        assert_eq!(dictionary_count(26), "26 entries");
    }

    #[test]
    fn ready_says_how_to_talk() {
        let h = header(&ready(), Instant::now());
        assert_eq!(h.text, READY_LINE);
        assert_eq!(h.tone, Tone::Primary);
        assert_eq!(h.fix, None);
        assert_eq!(h.progress, None);
        assert_eq!(h.note, None);
    }

    #[test]
    fn recording_shows_the_timer_in_copper() {
        let now = Instant::now();
        let mut s = ready();
        s.dictation = DictationPhase::Recording {
            since: now - Duration::from_secs(7),
            hands_free: false,
        };
        let h = header(&s, now);
        assert_eq!(h.text, "Recording 0:07");
        assert_eq!(h.tone, Tone::Accent);

        s.dictation = DictationPhase::Recording {
            since: now - Duration::from_secs(62),
            hands_free: true,
        };
        assert_eq!(header(&s, now).text, "Recording 1:02 · hands-free");
    }

    #[test]
    fn transcribing_is_muted() {
        let mut s = ready();
        s.dictation = DictationPhase::Transcribing;
        let h = header(&s, Instant::now());
        assert_eq!(h.text, "Transcribing…");
        assert_eq!(h.tone, Tone::Secondary);
    }

    #[test]
    fn missing_permissions_come_with_their_settings_pane() {
        let mut s = ready();
        s.permissions.accessibility = Some(false);
        let h = header(&s, Instant::now());
        assert_eq!(h.text, "Needs Accessibility");
        assert_eq!(h.fix, Some(Pane::Accessibility));

        let mut s = ready();
        s.permissions.microphone = Some(false);
        let h = header(&s, Instant::now());
        assert_eq!(h.text, "Needs Microphone");
        assert_eq!(h.fix, Some(Pane::Microphone));

        // Not asked yet is not missing.
        let mut s = ready();
        s.permissions.microphone = None;
        assert_eq!(header(&s, Instant::now()).text, READY_LINE);
    }

    #[test]
    fn a_downloading_model_shows_its_progress() {
        let mut s = ready();
        s.model = ModelStatus::Downloading {
            downloaded: 42,
            total: Some(100),
        };
        let h = header(&s, Instant::now());
        assert_eq!(h.text, "Downloading model 42%");
        assert_eq!(h.progress, Some(0.42));

        s.model = ModelStatus::Loading;
        let h = header(&s, Instant::now());
        assert_eq!(h.text, "Loading model");
        assert_eq!(h.progress, None);

        s.model = ModelStatus::Failed {
            message: "offline".into(),
        };
        assert_eq!(header(&s, Instant::now()).text, "Model failed: offline");
    }

    #[test]
    fn a_meeting_shows_title_and_time() {
        let now = Instant::now();
        let mut s = ready();
        s.meeting = Some(MeetingSnap {
            title: "Weekly sync".into(),
            path: "/m.md".into(),
            since: now - Duration::from_secs(724),
            started_at: Local::now(),
            app: None,
            mic_lost: false,
        });
        let h = header(&s, now);
        assert_eq!(h.text, "Meeting · Weekly sync · 12:04");
        s.meeting.as_mut().unwrap().mic_lost = true;
        assert_eq!(
            header(&s, now).text,
            "Meeting · Weekly sync · 12:04 · mic lost"
        );
        assert_eq!(h.tone, Tone::Accent);
        assert_eq!(h.fix, None);

        s.permissions.screen_recording = Some(false);
        assert_eq!(header(&s, now).fix, Some(Pane::ScreenRecording));

        // A dictation during the meeting takes the line.
        s.dictation = DictationPhase::Transcribing;
        assert_eq!(header(&s, now).text, "Transcribing…");
    }

    #[test]
    fn saving_a_meeting_is_muted() {
        let mut s = ready();
        s.meeting_saving = true;
        assert_eq!(header(&s, Instant::now()).text, "Saving meeting…");
    }

    #[test]
    fn the_last_error_wins_over_the_config_note() {
        let mut s = ready();
        s.config_note = Some("config.toml line 3: expected a number".into());
        assert_eq!(
            header(&s, Instant::now()).note.as_deref(),
            Some("config.toml line 3: expected a number")
        );
        s.last_error = Some("microphone: no input device".into());
        assert_eq!(
            header(&s, Instant::now()).note.as_deref(),
            Some("microphone: no input device")
        );
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn row_times_get_coarser_with_age() {
        let today = day(2026, 9, 29);
        let at = |d: NaiveDate| d.and_hms_opt(14, 3, 12).unwrap();
        assert_eq!(row_time(at(today), today), "14:03");
        assert_eq!(row_time(at(day(2026, 9, 28)), today), "Yesterday");
        assert_eq!(row_time(at(day(2026, 9, 2)), today), "Sep 2");
        assert_eq!(row_time(at(day(2025, 12, 31)), today), "2025-12-31");
    }

    #[test]
    fn meeting_rows_say_when_and_how_long() {
        let started = Local.with_ymd_and_hms(2026, 9, 29, 14, 0, 0).unwrap();
        let mut m = MeetingSummary {
            path: "/m.md".into(),
            title: "Weekly sync".into(),
            started_at: Some(started),
            length_secs: 2530,
            in_progress: false,
            snippet: String::new(),
        };
        assert_eq!(meeting_meta(&m), "Sep 29 14:00 · 42:10");
        m.in_progress = true;
        assert_eq!(meeting_meta(&m), "Sep 29 14:00 · 42:10 · recording");
        m.started_at = None;
        m.in_progress = false;
        assert_eq!(meeting_meta(&m), "42:10");
    }

    fn upcoming(title: &str, start: DateTime<Local>) -> UpcomingEvent {
        UpcomingEvent {
            key: title.into(),
            title: title.into(),
            start,
            end: start + chrono::Duration::minutes(30),
            all_day: false,
            other_attendees: true,
            call_app: Some("Google Meet"),
            declined: false,
            cancelled: false,
        }
    }

    fn clock(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 30, h, m, 0).unwrap()
    }

    #[test]
    fn the_next_meeting_says_how_soon() {
        let start = clock(14, 30);
        let end = clock(15, 0);
        assert_eq!(next_when(start, end, clock(14, 5)), "in 25 min");
        let just_before = start - chrono::Duration::seconds(30);
        assert_eq!(next_when(start, end, just_before), "in 1 min");
        assert_eq!(next_when(start, end, clock(12, 0)), "in 2 h");
        assert_eq!(next_when(start, end, clock(14, 40)), "now");
        let tomorrow = start + chrono::Duration::days(1);
        assert_eq!(next_when(tomorrow, tomorrow, clock(18, 0)), "tomorrow");
    }

    #[test]
    fn the_next_meeting_says_what_squawk_will_do() {
        let e = upcoming("Design review", clock(14, 30));
        let mut settings = Settings::default();
        assert_eq!(
            next_meta(&e, &settings),
            "14:30–15:00 · Google Meet · will record"
        );
        settings.auto_record = squawk_core::notetaker::AutoRecord::Off;
        assert_eq!(
            next_meta(&e, &settings),
            "14:30–15:00 · Google Meet · heads-up"
        );
        settings.heads_up = None;
        let mut e = e;
        e.call_app = None;
        assert_eq!(next_meta(&e, &settings), "14:30–15:00");
    }

    #[test]
    fn the_pinned_row_follows_the_calendar_and_the_meeting() {
        let now = Instant::now();
        let mut s = ready();
        assert!(matches!(
            next_row(&s, now, clock(14, 5)),
            NextRow::Line {
                text: ASKING_CALENDAR,
                ..
            }
        ));
        s.calendar_access = Some(false);
        assert_eq!(
            next_row(&s, now, clock(14, 5)),
            NextRow::Line {
                text: NO_CALENDAR,
                note: Some(NO_CALENDAR_NOTE),
                fix: Some(Pane::Calendars)
            }
        );
        s.calendar_access = Some(true);
        assert!(matches!(
            next_row(&s, now, clock(14, 5)),
            NextRow::Line {
                text: NO_MEETINGS,
                ..
            }
        ));
        s.next_meeting = Some(upcoming("Design review", clock(14, 30)));
        assert_eq!(
            next_row(&s, now, clock(14, 5)),
            NextRow::Next {
                when: "in 25 min".into(),
                title: "Design review".into(),
                meta: "14:30–15:00 · Google Meet · will record".into()
            }
        );
        s.meeting = Some(MeetingSnap {
            title: "Design review".into(),
            path: "/m.md".into(),
            since: now - Duration::from_secs(252),
            started_at: Local::now(),
            app: Some("Google Meet".into()),
            mic_lost: false,
        });
        assert_eq!(
            next_row(&s, now, clock(14, 34)),
            NextRow::Now {
                title: "Design review".into(),
                meta: "Recording · 4:12 · Google Meet".into()
            }
        );
    }

    #[test]
    fn the_ready_line_checks_calendar_calls_and_system_audio() {
        let mut s = ready();
        s.calendar_access = Some(true);
        s.permissions.screen_recording = Some(false);
        assert_eq!(
            ready_checks(&s),
            [
                ("Calendar", true),
                ("Call detection", true),
                ("System audio", false)
            ]
        );
        s.meeting_config.system_audio = false;
        s.meeting_config.detect_calls = false;
        assert_eq!(
            ready_checks(&s),
            [("Calendar", true), ("Call detection", false)]
        );
        assert_eq!(ready_check("Calendar", true), "\u{2713} Calendar");
        assert_eq!(ready_check("System audio", false), "\u{2715} System audio");
    }

    #[test]
    fn the_footer_row_flips_with_the_meeting() {
        assert_eq!(meeting_action(false), "Record meeting");
        assert_eq!(meeting_action(true), "Stop meeting");
    }
}
