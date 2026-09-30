//! Which calendar event deserves a heads-up, and when.
//!
//! An event qualifies when it is a real meeting: timed (not all-day), not
//! cancelled, not declined, and with other people on it or a call link in
//! its URL, location or notes. It is due from `lead` before its start until
//! [`LATE`] after (so a Mac that wakes a little late still offers), once.
//!
//! The same events answer two more questions: which meeting is on now, for
//! recording a call automatically ([`happening`]), and which one is next,
//! for the popover's Meetings tab ([`next`]).

use chrono::{DateTime, Duration, Local, TimeZone};

/// How long after an event's start its heads-up may still appear.
pub const LATE: Duration = Duration::seconds(60);
/// How early a call may start and still count as its calendar meeting
/// (people join a few minutes before).
pub const JOIN_EARLY: Duration = Duration::minutes(5);

/// One calendar event, reduced to what the heads-up needs.
#[derive(Debug, Clone, PartialEq)]
pub struct UpcomingEvent {
    /// Unique per occurrence (a repeating event's id plus its start).
    pub key: String,
    pub title: String,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub all_day: bool,
    /// Someone besides the user is invited.
    pub other_attendees: bool,
    /// The app of the Zoom/Meet/Teams/… link in the URL, location or
    /// notes ("Google Meet"), if it has one.
    pub call_app: Option<&'static str>,
    /// The user declined it.
    pub declined: bool,
    pub cancelled: bool,
}

impl UpcomingEvent {
    /// Whether this is the kind of event that gets a heads-up.
    pub fn is_meeting(&self) -> bool {
        !self.all_day
            && !self.declined
            && !self.cancelled
            && !self.title.trim().is_empty()
            && (self.other_attendees || self.call_app.is_some())
    }

    /// Whether its heads-up is due at `now` with `lead` before the start.
    pub fn is_due(&self, now: DateTime<Local>, lead: Duration) -> bool {
        self.is_meeting() && now >= self.start - lead && now < self.start + LATE
    }
}

/// The call links a calendar event carries: (host and path, the app).
pub const CALL_LINKS: &[(&str, &str)] = &[
    ("zoom.us/j/", "Zoom"),
    ("zoom.us/my/", "Zoom"),
    ("zoom.us/w/", "Zoom"),
    ("meet.google.com/", "Google Meet"),
    ("teams.microsoft.com/l/meetup-join", "Microsoft Teams"),
    ("teams.live.com/meet", "Microsoft Teams"),
    ("webex.com/", "Webex"),
    ("whereby.com/", "Whereby"),
    ("meet.jit.si/", "Jitsi Meet"),
    ("facetime.apple.com/join", "FaceTime"),
    ("discord.gg/", "Discord"),
    ("app.slack.com/huddle", "Slack"),
];

/// The app of the first call link in these texts, if any.
pub fn call_app<'a>(texts: impl IntoIterator<Item = &'a str>) -> Option<&'static str> {
    texts.into_iter().find_map(|text| {
        let lower = text.to_ascii_lowercase();
        CALL_LINKS
            .iter()
            .find(|(link, _)| lower.contains(link))
            .map(|(_, app)| *app)
    })
}

/// The meeting on at `now` (from [`JOIN_EARLY`] before its start until its
/// end). Overlapping ones: the latest start wins, as in the calendar's
/// "current event".
pub fn happening(events: &[UpcomingEvent], now: DateTime<Local>) -> Option<&UpcomingEvent> {
    events
        .iter()
        .filter(|e| e.is_meeting() && e.start - JOIN_EARLY <= now && now < e.end)
        .max_by_key(|e| e.start)
}

/// The next meeting as of `now`: the soonest-starting one that has not
/// ended, through the end of tomorrow. One already under way counts.
pub fn next(events: &[UpcomingEvent], now: DateTime<Local>) -> Option<&UpcomingEvent> {
    let horizon = end_of_tomorrow(now);
    events
        .iter()
        .filter(|e| e.is_meeting() && now < e.end && horizon.is_none_or(|h| e.start < h))
        .min_by_key(|e| e.start)
}

/// Midnight at the end of tomorrow. `None` only on a calendar edge that
/// has no such local time.
pub fn end_of_tomorrow(now: DateTime<Local>) -> Option<DateTime<Local>> {
    let day = now.date_naive().checked_add_days(chrono::Days::new(2))?;
    Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
        .earliest()
}

/// The event whose heads-up is due at `now`, skipping those in `done`.
/// The soonest start wins.
pub fn due(
    events: &[UpcomingEvent],
    now: DateTime<Local>,
    lead: Duration,
    done: impl Fn(&str) -> bool,
) -> Option<&UpcomingEvent> {
    events
        .iter()
        .filter(|e| e.is_due(now, lead) && !done(&e.key))
        .min_by_key(|e| e.start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32, s: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 30, h, m, s).unwrap()
    }

    fn meeting(title: &str, start: DateTime<Local>) -> UpcomingEvent {
        UpcomingEvent {
            key: format!("{title}@{}", start.timestamp()),
            title: title.into(),
            start,
            end: start + Duration::minutes(30),
            all_day: false,
            other_attendees: true,
            call_app: None,
            declined: false,
            cancelled: false,
        }
    }

    #[test]
    fn solo_blocks_all_day_declined_and_cancelled_events_get_nothing() {
        let mut focus = meeting("Focus time", at(15, 0, 0));
        focus.other_attendees = false;
        assert!(!focus.is_meeting());
        focus.call_app = Some("Zoom");
        assert!(focus.is_meeting(), "a solo event with a call link counts");

        let mut holiday = meeting("Holiday", at(0, 0, 0));
        holiday.all_day = true;
        let mut declined = meeting("Declined", at(15, 0, 0));
        declined.declined = true;
        let mut cancelled = meeting("Cancelled", at(15, 0, 0));
        cancelled.cancelled = true;
        let untitled = meeting(" ", at(15, 0, 0));
        for e in [holiday, declined, cancelled, untitled] {
            assert!(!e.is_meeting(), "{}", e.title);
        }
    }

    #[test]
    fn due_from_the_lead_until_a_minute_after_the_start() {
        let e = meeting("Design review", at(15, 0, 0));
        let lead = Duration::seconds(15);
        assert!(!e.is_due(at(14, 59, 44), lead));
        assert!(e.is_due(at(14, 59, 45), lead));
        assert!(e.is_due(at(15, 0, 0), lead));
        assert!(e.is_due(at(15, 0, 59), lead));
        assert!(!e.is_due(at(15, 1, 0), lead));
    }

    #[test]
    fn at_start_means_a_lead_of_zero() {
        let e = meeting("Standup", at(9, 30, 0));
        assert!(!e.is_due(at(9, 29, 59), Duration::zero()));
        assert!(e.is_due(at(9, 30, 0), Duration::zero()));
    }

    #[test]
    fn five_minutes_ahead() {
        let e = meeting("1:1", at(11, 0, 0));
        let lead = Duration::minutes(5);
        assert!(!e.is_due(at(10, 54, 59), lead));
        assert!(e.is_due(at(10, 55, 0), lead));
    }

    #[test]
    fn the_soonest_due_event_not_yet_offered_wins() {
        let events = [
            meeting("Later", at(15, 30, 0)),
            meeting("Retro", at(15, 0, 0)),
            meeting("Also now", at(15, 0, 30)),
        ];
        let now = at(14, 59, 50);
        let lead = Duration::minutes(1);
        assert_eq!(due(&events, now, lead, |_| false).unwrap().title, "Retro");
        let retro = events[1].key.clone();
        assert_eq!(
            due(&events, now, lead, |k| k == retro).unwrap().title,
            "Also now"
        );
        assert!(due(&events, now, lead, |_| true).is_none());
        assert!(due(&events, at(14, 0, 0), lead, |_| false).is_none());
    }

    #[test]
    fn call_links_are_found_in_any_text_and_name_their_app() {
        assert_eq!(
            call_app(["https://example.zoom.us/j/123456789?pwd=x"]),
            Some("Zoom")
        );
        assert_eq!(
            call_app(["", "Join: https://meet.google.com/abc-defg-hij"]),
            Some("Google Meet")
        );
        assert_eq!(
            call_app(["https://teams.microsoft.com/l/meetup-join/19%3ameeting"]),
            Some("Microsoft Teams")
        );
        assert_eq!(
            call_app(["HTTPS://MEET.GOOGLE.COM/ABC-DEFG-HIJ"]),
            Some("Google Meet")
        );
        assert_eq!(call_app(["Conference room 4", "Bring the slides"]), None);
        assert_eq!(call_app(["https://zoom.us/pricing"]), None);
    }

    #[test]
    fn a_call_joined_a_few_minutes_early_counts_as_its_meeting() {
        let e = meeting("Design review", at(15, 0, 0));
        let events = [e];
        assert!(happening(&events, at(14, 54, 59)).is_none());
        assert_eq!(
            happening(&events, at(14, 55, 0)).unwrap().title,
            "Design review"
        );
        assert!(happening(&events, at(15, 29, 59)).is_some());
        assert!(happening(&events, at(15, 30, 0)).is_none(), "it ended");
    }

    #[test]
    fn back_to_back_the_later_meeting_is_the_one_on() {
        let events = [
            meeting("First", at(15, 0, 0)),
            meeting("Second", at(15, 30, 0)),
        ];
        assert_eq!(happening(&events, at(15, 27, 0)).unwrap().title, "Second");
        let mut solo = meeting("Focus", at(15, 0, 0));
        solo.other_attendees = false;
        assert!(happening(&[solo], at(15, 10, 0)).is_none());
    }

    #[test]
    fn next_is_the_soonest_meeting_not_yet_over_through_tomorrow() {
        let now = at(14, 10, 0);
        let mut declined = meeting("Declined", at(14, 30, 0));
        declined.declined = true;
        let events = [
            meeting("Earlier", at(13, 0, 0)),
            declined,
            meeting("Later", at(16, 0, 0)),
            meeting("Under way", at(14, 0, 0)),
        ];
        assert_eq!(next(&events, now).unwrap().title, "Under way");
        assert_eq!(next(&events, at(14, 31, 0)).unwrap().title, "Later");

        let tomorrow = now + Duration::days(1);
        let day_after = now + Duration::days(2);
        let events = [
            meeting("Day after", day_after),
            meeting("Tomorrow", tomorrow),
        ];
        assert_eq!(next(&events, now).unwrap().title, "Tomorrow");
        assert!(next(&events[..1], now).is_none(), "past tomorrow");
    }
}
