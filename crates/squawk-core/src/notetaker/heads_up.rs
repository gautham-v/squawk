//! Which calendar event deserves a heads-up, and when.
//!
//! An event qualifies when it is a real meeting: timed (not all-day), not
//! cancelled, not declined, and with other people on it or a call link in
//! its URL, location or notes. It is due from `lead` before its start until
//! [`LATE`] after (so a Mac that wakes a little late still offers), once.

use chrono::{DateTime, Duration, Local};

/// How long after an event's start its heads-up may still appear.
pub const LATE: Duration = Duration::seconds(60);

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
    /// A Zoom/Meet/Teams/… link in the URL, location or notes.
    pub video_link: bool,
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
            && (self.other_attendees || self.video_link)
    }

    /// Whether its heads-up is due at `now` with `lead` before the start.
    pub fn is_due(&self, now: DateTime<Local>, lead: Duration) -> bool {
        self.is_meeting() && now >= self.start - lead && now < self.start + LATE
    }
}

/// Hosts of the call links a calendar event carries.
pub const VIDEO_HOSTS: &[&str] = &[
    "zoom.us/j/",
    "zoom.us/my/",
    "zoom.us/w/",
    "meet.google.com/",
    "teams.microsoft.com/l/meetup-join",
    "teams.live.com/meet",
    "webex.com/",
    "whereby.com/",
    "meet.jit.si/",
    "facetime.apple.com/join",
    "discord.gg/",
    "app.slack.com/huddle",
];

/// Whether any of these texts has a call link.
pub fn has_video_link<'a>(texts: impl IntoIterator<Item = &'a str>) -> bool {
    texts.into_iter().any(|text| {
        let lower = text.to_ascii_lowercase();
        VIDEO_HOSTS.iter().any(|host| lower.contains(host))
    })
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
            video_link: false,
            declined: false,
            cancelled: false,
        }
    }

    #[test]
    fn solo_blocks_all_day_declined_and_cancelled_events_get_nothing() {
        let mut focus = meeting("Focus time", at(15, 0, 0));
        focus.other_attendees = false;
        assert!(!focus.is_meeting());
        focus.video_link = true;
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
    fn call_links_are_found_in_any_text() {
        assert!(has_video_link([
            "https://example.zoom.us/j/123456789?pwd=x"
        ]));
        assert!(has_video_link([
            "",
            "Join: https://meet.google.com/abc-defg-hij"
        ]));
        assert!(has_video_link([
            "https://teams.microsoft.com/l/meetup-join/19%3ameeting"
        ]));
        assert!(has_video_link(["HTTPS://MEET.GOOGLE.COM/ABC-DEFG-HIJ"]));
        assert!(!has_video_link(["Conference room 4", "Bring the slides"]));
        assert!(!has_video_link(["https://zoom.us/pricing"]));
    }
}
