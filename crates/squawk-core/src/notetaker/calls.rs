//! What counts as a call: which processes using the mic are call apps, and
//! when a run of mic use becomes a call (and when it ends).
//!
//! The macOS side (`squawk-app`'s `mic_watch`) lists the processes with
//! input running; [`classify`] names the known call apps among them, a
//! browser only through [`call_site`] on one of its window titles; and
//! [`CallTracker`] turns the named apps over time into [`CallEvent`]s:
//! - a call starts once an app has used the mic for [`MIN_USE`] without a
//!   break (dictation apps and Siri come and go quicker than that, and are
//!   not call apps anyway);
//! - it ends once the app has let go of the mic for [`END_AFTER`]; picking
//!   the mic back up within that is the same call (switching AirPods on).

use std::time::{Duration, Instant};

/// How long an app must hold the mic before it counts as a call.
pub const MIN_USE: Duration = Duration::from_secs(3);
/// How long an app must have let go of the mic before its call has ended.
pub const END_AFTER: Duration = Duration::from_secs(10);

/// A process with input running, as Core Audio reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicUser {
    pub pid: i32,
    /// Empty when the process has none (daemons).
    pub bundle_id: String,
    /// The executable's name (`proc_name`).
    pub name: String,
}

/// What a mic user is, when it is something call-shaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A call app: its display name.
    App(&'static str),
    /// A browser (or its helper): whether it is on a call depends on its
    /// window titles. `bundle_id` is the browser's own app.
    Browser {
        bundle_id: &'static str,
        name: &'static str,
    },
}

/// Call apps by bundle id (or bundle id prefix, for their helpers).
pub const CALL_APPS: &[(&str, &str)] = &[
    ("us.zoom", "Zoom"),
    ("com.microsoft.teams2", "Microsoft Teams"),
    ("com.microsoft.teams", "Microsoft Teams"),
    ("com.apple.FaceTime", "FaceTime"),
    ("com.tinyspeck.slackmacgap", "Slack"),
    ("Cisco-Systems.Spark", "Webex"),
    ("com.webex.meetingmanager", "Webex"),
    ("com.cisco.webexmeetingsapp", "Webex"),
    ("com.hnc.Discord", "Discord"),
];

/// Daemons that do a call app's audio for it, by process name.
pub const CALL_DAEMONS: &[(&str, &str)] = &[("avconferenced", "FaceTime")];

/// Browsers: (bundle id or prefix of the process using the mic, the
/// browser's app bundle id, its name).
pub const BROWSERS: &[(&str, &str, &str)] = &[
    ("com.google.Chrome", "com.google.Chrome", "Chrome"),
    (
        "company.thebrowser.Browser",
        "company.thebrowser.Browser",
        "Arc",
    ),
    (
        "org.mozilla.firefoxdeveloperedition",
        "org.mozilla.firefoxdeveloperedition",
        "Firefox",
    ),
    ("org.mozilla.firefox", "org.mozilla.firefox", "Firefox"),
    ("org.mozilla.nightly", "org.mozilla.nightly", "Firefox"),
    (
        "org.mozilla.plugincontainer",
        "org.mozilla.firefox",
        "Firefox",
    ),
    ("com.apple.Safari", "com.apple.Safari", "Safari"),
    ("com.apple.WebKit.GPU", "com.apple.Safari", "Safari"),
    ("com.microsoft.edgemac", "com.microsoft.edgemac", "Edge"),
    ("com.brave.Browser", "com.brave.Browser", "Brave"),
];

/// Call sites, by a word or phrase their tab titles carry. Checked in order.
pub const CALL_SITES: &[(&str, &str)] = &[
    ("Google Meet", "Google Meet"),
    ("Meet", "Google Meet"),
    ("Zoom", "Zoom"),
    ("Microsoft Teams", "Microsoft Teams"),
    ("Webex", "Webex"),
    ("Whereby", "Whereby"),
    ("Jitsi Meet", "Jitsi Meet"),
    ("Discord", "Discord"),
    ("Slack", "Slack"),
];

/// `bundle_id` is `id` or one of its helpers (`id.helper`, `id.helper.Renderer`).
fn is_or_under(bundle_id: &str, id: &str) -> bool {
    bundle_id == id
        || bundle_id
            .strip_prefix(id)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// What `user` is, leaving out squawk itself (`own_pid`).
pub fn classify(user: &MicUser, own_pid: i32) -> Option<Source> {
    if user.pid == own_pid {
        return None;
    }
    if let Some((_, name)) = CALL_APPS
        .iter()
        .find(|(id, _)| is_or_under(&user.bundle_id, id))
    {
        return Some(Source::App(name));
    }
    if let Some((_, name)) = CALL_DAEMONS.iter().find(|(proc, _)| user.name == *proc) {
        return Some(Source::App(name));
    }
    BROWSERS
        .iter()
        .find(|(id, _, _)| is_or_under(&user.bundle_id, id))
        .map(|(_, app, name)| Source::Browser {
            bundle_id: app,
            name,
        })
}

/// The call site a browser window title is on, if any: "Meet – abc-defg-hij"
/// → Google Meet, "Weekly | Microsoft Teams" → Microsoft Teams. Whole words
/// only, and case-sensitive ("zoom in on photos" is not Zoom).
pub fn call_site(title: &str) -> Option<&'static str> {
    let words: Vec<&str> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    CALL_SITES.iter().find_map(|(phrase, name)| {
        let wanted: Vec<&str> = phrase.split(' ').collect();
        words
            .windows(wanted.len())
            .any(|w| w == wanted.as_slice())
            .then_some(*name)
    })
}

/// One call, for as long as it lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub id: CallId,
    /// The app's display name ("Zoom", "Google Meet").
    pub app: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallEvent {
    Started(Call),
    Ended(Call),
}

#[derive(Debug, Clone)]
struct Tracked {
    call: Call,
    since: Instant,
    /// When it let go of the mic, while it has.
    released: Option<Instant>,
    confirmed: bool,
}

/// Turns "these call apps are using the mic now" samples into calls.
#[derive(Debug, Clone)]
pub struct CallTracker {
    tracked: Vec<Tracked>,
    next_id: u64,
    min_use: Duration,
    end_after: Duration,
}

impl Default for CallTracker {
    fn default() -> Self {
        CallTracker::new(MIN_USE, END_AFTER)
    }
}

impl CallTracker {
    pub fn new(min_use: Duration, end_after: Duration) -> CallTracker {
        CallTracker {
            tracked: Vec::new(),
            next_id: 1,
            min_use,
            end_after,
        }
    }

    /// Feed the call apps using the mic at `now` (call at least once a
    /// second while anything is tracked, so starts and ends land on time).
    pub fn update(&mut self, now: Instant, active: &[String]) -> Vec<CallEvent> {
        for app in active {
            match self.tracked.iter_mut().find(|t| &t.call.app == app) {
                Some(t) => t.released = None,
                None => {
                    let id = CallId(self.next_id);
                    self.next_id += 1;
                    self.tracked.push(Tracked {
                        call: Call {
                            id,
                            app: app.clone(),
                        },
                        since: now,
                        released: None,
                        confirmed: false,
                    });
                }
            }
        }
        let mut events = Vec::new();
        let (min_use, end_after) = (self.min_use, self.end_after);
        self.tracked.retain_mut(|t| {
            let using = active.contains(&t.call.app);
            if !using && t.released.is_none() {
                t.released = Some(now);
            }
            if !t.confirmed {
                // A short use: gone before it counted.
                if !using {
                    return false;
                }
                if now.saturating_duration_since(t.since) >= min_use {
                    t.confirmed = true;
                    events.push(CallEvent::Started(t.call.clone()));
                }
                return true;
            }
            match t.released {
                Some(at) if now.saturating_duration_since(at) >= end_after => {
                    events.push(CallEvent::Ended(t.call.clone()));
                    false
                }
                _ => true,
            }
        });
        events
    }

    /// Calls that have started and not ended, oldest first.
    pub fn calls(&self) -> impl Iterator<Item = &Call> {
        self.tracked.iter().filter(|t| t.confirmed).map(|t| &t.call)
    }

    /// The call with this id, if it is still going.
    pub fn get(&self, id: CallId) -> Option<&Call> {
        self.calls().find(|c| c.id == id)
    }

    /// The newest call whose app has the mic right now.
    pub fn current(&self) -> Option<&Call> {
        self.tracked
            .iter()
            .filter(|t| t.confirmed && t.released.is_none())
            .max_by_key(|t| t.since)
            .map(|t| &t.call)
    }

    /// Whether anything is being tracked (a tick is needed to end it).
    pub fn is_idle(&self) -> bool {
        self.tracked.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(pid: i32, bundle_id: &str, name: &str) -> MicUser {
        MicUser {
            pid,
            bundle_id: bundle_id.into(),
            name: name.into(),
        }
    }

    fn apps(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn call_apps_and_their_helpers_are_named() {
        assert_eq!(
            classify(&user(10, "us.zoom.xos", "zoom.us"), 1),
            Some(Source::App("Zoom"))
        );
        assert_eq!(
            classify(&user(10, "com.microsoft.teams2", "MSTeams"), 1),
            Some(Source::App("Microsoft Teams"))
        );
        assert_eq!(
            classify(&user(10, "com.hnc.Discord.helper", "Discord Helper"), 1),
            Some(Source::App("Discord"))
        );
        assert_eq!(
            classify(&user(10, "", "avconferenced"), 1),
            Some(Source::App("FaceTime"))
        );
        assert_eq!(
            classify(&user(10, "Cisco-Systems.Spark", "Webex"), 1),
            Some(Source::App("Webex"))
        );
    }

    #[test]
    fn browsers_need_a_title_and_map_helpers_to_the_browser() {
        assert_eq!(
            classify(
                &user(10, "com.google.Chrome.helper", "Google Chrome Helper"),
                1
            ),
            Some(Source::Browser {
                bundle_id: "com.google.Chrome",
                name: "Chrome"
            })
        );
        assert_eq!(
            classify(&user(10, "com.apple.WebKit.GPU", "com.apple.WebKit.GPU"), 1),
            Some(Source::Browser {
                bundle_id: "com.apple.Safari",
                name: "Safari"
            })
        );
        assert_eq!(
            classify(
                &user(10, "org.mozilla.firefoxdeveloperedition", "firefox"),
                1
            ),
            Some(Source::Browser {
                bundle_id: "org.mozilla.firefoxdeveloperedition",
                name: "Firefox"
            })
        );
    }

    #[test]
    fn squawk_dictation_apps_and_siri_are_not_calls() {
        assert_eq!(classify(&user(42, "us.zoom.xos", "zoom.us"), 42), None);
        assert_eq!(
            classify(&user(10, "com.example.dictate", "Dictate"), 1),
            None
        );
        assert_eq!(
            classify(&user(10, "com.apple.assistantd", "assistantd"), 1),
            None
        );
        assert_eq!(
            classify(
                &user(
                    10,
                    "com.apple.SpeechRecognitionCore.speechrecognitiond",
                    "speechrecognitiond"
                ),
                1
            ),
            None
        );
        // A prefix has to end at a dot.
        assert_eq!(classify(&user(10, "us.zoomer.app", "zoomer"), 1), None);
    }

    #[test]
    fn call_sites_come_from_whole_words_in_the_title() {
        assert_eq!(call_site("Meet – abc-defg-hij"), Some("Google Meet"));
        assert_eq!(
            call_site("Meet - abc-defg-hij - Google Chrome"),
            Some("Google Meet")
        );
        assert_eq!(call_site("Google Meet"), Some("Google Meet"));
        assert_eq!(
            call_site("Weekly sync | Microsoft Teams"),
            Some("Microsoft Teams")
        );
        assert_eq!(call_site("Zoom Meeting"), Some("Zoom"));
        assert_eq!(call_site("general - Example Co - Slack"), Some("Slack"));
        assert_eq!(call_site("How to zoom in on photos"), None);
        assert_eq!(call_site("Meeting notes - Google Docs"), None);
        assert_eq!(call_site("Online mic test"), None);
        assert_eq!(call_site(""), None);
    }

    #[test]
    fn a_call_starts_after_three_seconds_of_mic() {
        let t0 = Instant::now();
        let mut tracker = CallTracker::default();
        let zoom = apps(&["Zoom"]);
        assert!(tracker.update(t0, &zoom).is_empty());
        assert!(tracker.update(t0 + secs(1), &zoom).is_empty());
        assert!(tracker.update(t0 + secs(2), &zoom).is_empty());
        let events = tracker.update(t0 + secs(3), &zoom);
        assert_eq!(
            events,
            [CallEvent::Started(Call {
                id: CallId(1),
                app: "Zoom".into()
            })]
        );
        // Only once.
        assert!(tracker.update(t0 + secs(4), &zoom).is_empty());
        assert_eq!(tracker.current().map(|c| c.app.as_str()), Some("Zoom"));
    }

    #[test]
    fn a_short_mic_use_is_never_a_call() {
        let t0 = Instant::now();
        let mut tracker = CallTracker::default();
        tracker.update(t0, &apps(&["Slack"]));
        tracker.update(t0 + secs(2), &apps(&["Slack"]));
        assert!(tracker
            .update(t0 + secs(2) + Duration::from_millis(500), &[])
            .is_empty());
        assert!(tracker.is_idle());
        // Starting again later counts from zero.
        tracker.update(t0 + secs(10), &apps(&["Slack"]));
        assert!(tracker.update(t0 + secs(12), &apps(&["Slack"])).is_empty());
        assert_eq!(tracker.update(t0 + secs(13), &apps(&["Slack"])).len(), 1);
    }

    #[test]
    fn a_call_ends_ten_seconds_after_the_app_lets_go() {
        let t0 = Instant::now();
        let mut tracker = CallTracker::default();
        let zoom = apps(&["Zoom"]);
        for s in 0..=60 {
            tracker.update(t0 + secs(s), &zoom);
        }
        assert!(tracker.update(t0 + secs(61), &[]).is_empty());
        assert!(tracker.update(t0 + secs(70), &[]).is_empty());
        assert!(tracker.current().is_none(), "no longer holding the mic");
        assert!(tracker.calls().next().is_some(), "but not over yet");
        let events = tracker.update(t0 + secs(71), &[]);
        assert_eq!(
            events,
            [CallEvent::Ended(Call {
                id: CallId(1),
                app: "Zoom".into()
            })]
        );
        assert!(tracker.is_idle());
    }

    #[test]
    fn picking_the_mic_back_up_within_ten_seconds_is_the_same_call() {
        let t0 = Instant::now();
        let mut tracker = CallTracker::default();
        let meet = apps(&["Google Meet"]);
        for s in 0..=5 {
            tracker.update(t0 + secs(s), &meet);
        }
        tracker.update(t0 + secs(6), &[]);
        tracker.update(t0 + secs(14), &[]);
        assert!(tracker.update(t0 + secs(15), &meet).is_empty());
        // The ten seconds start over at the next release.
        tracker.update(t0 + secs(30), &[]);
        assert!(tracker.update(t0 + secs(39), &[]).is_empty());
        let ended = tracker.update(t0 + secs(40), &[]);
        assert!(matches!(&ended[..], [CallEvent::Ended(c)] if c.id == CallId(1)));
    }

    #[test]
    fn two_apps_are_two_calls() {
        let t0 = Instant::now();
        let mut tracker = CallTracker::default();
        tracker.update(t0, &apps(&["Zoom"]));
        tracker.update(t0 + secs(2), &apps(&["Zoom", "Slack"]));
        let started = tracker.update(t0 + secs(3), &apps(&["Zoom", "Slack"]));
        assert_eq!(started.len(), 1);
        let started = tracker.update(t0 + secs(5), &apps(&["Zoom", "Slack"]));
        assert!(
            matches!(&started[..], [CallEvent::Started(c)] if c.app == "Slack" && c.id == CallId(2))
        );
        assert_eq!(tracker.current().map(|c| c.app.as_str()), Some("Slack"));
        tracker.update(t0 + secs(6), &apps(&["Zoom"]));
        let ended = tracker.update(t0 + secs(16), &apps(&["Zoom"]));
        assert!(matches!(&ended[..], [CallEvent::Ended(c)] if c.app == "Slack"));
        assert_eq!(tracker.current().map(|c| c.app.as_str()), Some("Zoom"));
    }

    #[test]
    fn a_new_call_after_the_end_gets_a_new_id() {
        let t0 = Instant::now();
        let mut tracker = CallTracker::default();
        let zoom = apps(&["Zoom"]);
        tracker.update(t0, &zoom);
        tracker.update(t0 + secs(3), &zoom);
        tracker.update(t0 + secs(4), &[]);
        tracker.update(t0 + secs(14), &[]);
        tracker.update(t0 + secs(20), &zoom);
        let started = tracker.update(t0 + secs(23), &zoom);
        assert!(matches!(&started[..], [CallEvent::Started(c)] if c.id == CallId(2)));
    }
}
