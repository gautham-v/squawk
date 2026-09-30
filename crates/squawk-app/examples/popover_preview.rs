//! Dev preview: renders the popover in a normal window with fixture data —
//! no mic, no model, no files read.
//!
//! ```text
//! cargo run -p squawk-app --example popover_preview -- \
//!     ready|meetings|dictionary|recording|meeting|downloading|permissions|empty
//! ```

use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate, TimeZone};
use gpui::{
    div, point, px, size, App, AppContext, Application, Bounds, Focusable, IntoElement,
    ParentElement, Render, Styled, TitlebarOptions, Window, WindowBounds, WindowOptions,
};
use squawk_app::controller::{DictationPhase, MeetingSnap, Snapshot};
use squawk_app::ui::popover::{self, Popover, PopoverData, PopoverEvent, Tab};
use squawk_app::ui::theme;
use squawk_core::dictionary::Entry;
use squawk_core::status::Permissions;
use squawk_core::store::{DictationEntry, MeetingSummary};
use squawk_core::{ModelStatus, Paths};

struct Preview {
    popover: gpui::Entity<Popover>,
}

impl Render for Preview {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .justify_center()
            .items_start()
            .bg(gpui::rgb(0xd7d3cc))
            .p(px(24.))
            .child(
                div()
                    .h(px(theme::POPOVER_HEIGHT_PX))
                    .child(self.popover.clone()),
            )
    }
}

fn fixture_data() -> PopoverData {
    let today = Local::now().date_naive();
    let at = |d: NaiveDate, h, m| d.and_hms_opt(h, m, 0).unwrap();
    let history = vec![
        DictationEntry {
            at: at(today, 14, 5),
            app: "Ghostty".into(),
            project: Some("squawk".into()),
            text: "Fix the resampler so it handles 48 kHz input, and add a test that feeds \
                   it a sine at 1 kHz and checks the frequency survives."
                .into(),
        },
        DictationEntry {
            at: at(today, 13, 40),
            app: "Safari".into(),
            project: None,
            text: "Thanks, that works.".into(),
        },
        DictationEntry {
            at: at(today.pred_opt().unwrap(), 17, 12),
            app: "Ghostty".into(),
            project: Some("demo-app".into()),
            text: "Look at @src/audio.rs and tell me why the first segment is always empty.".into(),
        },
    ];
    let meetings = vec![
        MeetingSummary {
            path: "/tmp/2026-09-29 1400 Weekly sync.md".into(),
            title: "Weekly sync".into(),
            started_at: Local.with_ymd_and_hms(2026, 9, 29, 14, 0, 0).earliest(),
            length_secs: 2530,
            in_progress: false,
            snippet: "Morning. Can everyone hear me?".into(),
        },
        MeetingSummary {
            path: "/tmp/2026-09-28 0930 Design review.md".into(),
            title: "Design review".into(),
            started_at: Local.with_ymd_and_hms(2026, 9, 28, 9, 30, 0).earliest(),
            length_secs: 1805,
            in_progress: false,
            snippet: String::new(),
        },
    ];
    let dictionary = vec![
        Entry::Term("Kubernetes".into()),
        Entry::Term("ChatGPT".into()),
        Entry::Replace {
            from: "cloud code".into(),
            to: "Claude Code".into(),
        },
    ];
    PopoverData {
        history,
        meetings,
        dictionary,
        error: None,
    }
}

fn snapshot(mode: &str) -> Snapshot {
    let paths = Paths::under(&std::env::temp_dir().join("squawk-preview"));
    let mut s = Snapshot::initial(paths, ModelStatus::Ready, None);
    s.permissions = Permissions {
        accessibility: Some(true),
        microphone: Some(true),
        screen_recording: None,
    };
    let now = Instant::now();
    match mode {
        "recording" => {
            s.dictation = DictationPhase::Recording {
                since: now - Duration::from_secs(7),
                hands_free: true,
            }
        }
        "meeting" => {
            s.meeting = Some(MeetingSnap {
                title: "Weekly sync".into(),
                path: "/tmp/m.md".into(),
                since: now - Duration::from_secs(724),
                started_at: Local::now(),
            })
        }
        "downloading" => {
            s.model = ModelStatus::Downloading {
                downloaded: 201_000_000,
                total: Some(478_517_071),
            }
        }
        "permissions" => {
            s.permissions.accessibility = Some(false);
            s.config_note = Some("config.toml: expected a number for tap_max_ms".into());
        }
        _ => {}
    }
    s
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "ready".into());

    Application::new().run(move |cx: &mut App| {
        popover::bind_keys(cx);
        let bounds = Bounds {
            origin: point(px(120.), px(120.)),
            size: size(
                px(theme::POPOVER_WIDTH_PX + 48.),
                px(theme::POPOVER_HEIGHT_PX + 48.),
            ),
        };
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("squawk preview".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let tab = match mode.as_str() {
                    "meetings" | "meeting" => Tab::Meetings,
                    "dictionary" => Tab::Dictionary,
                    _ => Tab::History,
                };
                let data = if mode == "empty" {
                    PopoverData::default()
                } else {
                    fixture_data()
                };
                let fn_hint = mode == "ready";
                let snap = snapshot(&mode);
                let popover = cx.new(|cx| Popover::with_data(snap, data, tab, fn_hint, cx));
                cx.subscribe(&popover, |_, event, _| {
                    if *event == PopoverEvent::Close {
                        println!("preview: popover asked to close");
                    } else {
                        println!("preview: {event:?}");
                    }
                })
                .detach();
                window.focus(&popover.focus_handle(cx));
                cx.new(|_| Preview { popover })
            },
        )
        .expect("open preview window");
        cx.activate(true);
    });
}
