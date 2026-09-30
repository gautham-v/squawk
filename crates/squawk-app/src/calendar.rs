//! The meeting title: the calendar event happening now (EventKit, as in
//! daybar's `calendar/eventkit.rs`).
//!
//! Among non-all-day events whose span contains now, prefer the one that
//! started most recently; ties → the shortest. Requests full calendar access
//! the first time (needs NSCalendarsFullAccessUsageDescription in
//! Info.plist); denied or no event → `None`, and the caller uses "Meeting".

pub fn current_event_title() -> Option<String> {
    todo!("app agent")
}
