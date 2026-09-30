//! The meeting title: the calendar event happening now (EventKit, as in
//! daybar's `calendar/eventkit.rs`).
//!
//! Among non-all-day events whose span contains now, prefer the one that
//! started most recently; ties → the shortest. Needs full calendar access
//! (and NSCalendarsFullAccessUsageDescription in Info.plist). The first time,
//! access is requested in the background and this meeting is simply called
//! "Meeting": blocking a meeting start on a permission prompt would lose the
//! first minutes of it. Denied or no event → `None`.

use block2::RcBlock;
use chrono::{DateTime, Duration, Local, TimeZone};
use objc2::rc::autoreleasepool;
use objc2::runtime::{Bool, NSObjectProtocol};
use objc2::sel;
use objc2_event_kit::{EKAuthorizationStatus, EKEntityType, EKEventStore};
use objc2_foundation::{NSDate, NSError};

/// One event, reduced to what the pick needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub title: String,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub all_day: bool,
}

pub fn current_event_title() -> Option<String> {
    // SAFETY: a class method with no preconditions.
    let status = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
    match status {
        EKAuthorizationStatus::FullAccess => {}
        EKAuthorizationStatus::NotDetermined => {
            request_access_in_background();
            return None;
        }
        _ => return None,
    }
    let now = Local::now();
    let candidates = autoreleasepool(|_| {
        // SAFETY: a fresh store used on this thread only.
        let store = unsafe { EKEventStore::new() };
        let start = ns_date(now - Duration::hours(12));
        let end = ns_date(now + Duration::hours(12));
        // SAFETY: valid dates; `None` = all calendars.
        let events = unsafe {
            let predicate =
                store.predicateForEventsWithStartDate_endDate_calendars(&start, &end, None);
            store.eventsMatchingPredicate(&predicate)
        };
        events
            .iter()
            .filter_map(|ev| {
                // SAFETY: the store is alive for this scope.
                unsafe {
                    Some(Candidate {
                        title: ev.title().to_string(),
                        start: from_ns_date(&ev.startDate())?,
                        end: from_ns_date(&ev.endDate())?,
                        all_day: ev.isAllDay(),
                    })
                }
            })
            .collect::<Vec<_>>()
    });
    pick_current(&candidates, now)
}

/// The event the user is in at `now`.
pub fn pick_current(events: &[Candidate], now: DateTime<Local>) -> Option<String> {
    events
        .iter()
        .filter(|e| !e.all_day && e.start <= now && now < e.end)
        .filter(|e| !e.title.trim().is_empty())
        .max_by(|a, b| {
            a.start
                .cmp(&b.start)
                .then_with(|| (b.end - b.start).cmp(&(a.end - a.start)))
        })
        .map(|e| e.title.trim().to_string())
}

fn request_access_in_background() {
    std::thread::spawn(|| {
        autoreleasepool(|_| {
            // SAFETY: a fresh store; the completion only logs.
            let store = unsafe { EKEventStore::new() };
            let handler = RcBlock::new(|granted: Bool, _err: *mut NSError| {
                log::info!("calendar access granted: {}", granted.as_bool());
            });
            let modern = store.respondsToSelector(sel!(requestFullAccessToEventsWithCompletion:));
            unsafe {
                if modern {
                    store.requestFullAccessToEventsWithCompletion(RcBlock::as_ptr(&handler));
                } else {
                    #[allow(deprecated)]
                    store.requestAccessToEntityType_completion(
                        EKEntityType::Event,
                        RcBlock::as_ptr(&handler),
                    );
                }
            }
            // Keep the store alive while the prompt is up.
            std::thread::sleep(std::time::Duration::from_secs(120));
        });
    });
}

fn ns_date(at: DateTime<Local>) -> objc2::rc::Retained<NSDate> {
    let secs = at.timestamp() as f64 + f64::from(at.timestamp_subsec_nanos()) / 1e9;
    NSDate::dateWithTimeIntervalSince1970(secs)
}

fn from_ns_date(date: &NSDate) -> Option<DateTime<Local>> {
    let secs = date.timeIntervalSince1970();
    Local.timestamp_opt(secs.floor() as i64, 0).earliest()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 29, h, m, 0).unwrap()
    }

    fn event(title: &str, start: DateTime<Local>, end: DateTime<Local>) -> Candidate {
        Candidate {
            title: title.into(),
            start,
            end,
            all_day: false,
        }
    }

    #[test]
    fn the_event_in_progress_wins() {
        let events = [
            event("Earlier", at(9, 0), at(10, 0)),
            event("Weekly sync", at(14, 0), at(15, 0)),
            event("Later", at(16, 0), at(17, 0)),
        ];
        assert_eq!(
            pick_current(&events, at(14, 20)).as_deref(),
            Some("Weekly sync")
        );
    }

    #[test]
    fn overlapping_events_prefer_the_latest_start_then_the_shortest() {
        let events = [
            event("Focus block", at(13, 0), at(17, 0)),
            event("Standup", at(14, 0), at(14, 15)),
            event("Offsite prep", at(14, 0), at(16, 0)),
        ];
        assert_eq!(pick_current(&events, at(14, 5)).as_deref(), Some("Standup"));
    }

    #[test]
    fn all_day_and_untitled_events_are_skipped() {
        let mut holiday = event("Holiday", at(0, 0), at(23, 59));
        holiday.all_day = true;
        let untitled = event("  ", at(14, 0), at(15, 0));
        assert_eq!(pick_current(&[holiday, untitled], at(14, 30)), None);
    }

    #[test]
    fn an_event_that_just_ended_is_not_current() {
        let events = [event("Done", at(13, 0), at(14, 0))];
        assert_eq!(pick_current(&events, at(14, 0)), None);
    }
}
