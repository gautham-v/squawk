//! Dev preview: renders the popover in a normal window with fixture data —
//! no mic, no model, no files read.
//!
//! ```text
//! cargo run -p squawk-app --example popover_preview -- \
//!     ready|meetings|meetings-nocal|dictionary|settings|settings-menu|settings-nocal|
//!     recording|meeting|downloading|permissions|empty|panels \
//!     [light|dark|system] [end]
//! ```
//!
//! The optional second argument forces the appearance (by default the
//! window follows the system); `end` scrolls the list to its bottom.
//! `panels` shows the notetaker's prompt panels instead of the popover.

use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate, TimeZone};
use gpui::{
    div, point, px, size, App, AppContext, Application, Bounds, Focusable, IntoElement,
    ParentElement, Render, Styled, TitlebarOptions, Window, WindowBounds, WindowOptions,
};
use squawk_app::controller::{DictationPhase, MeetingSnap, Snapshot};
use squawk_app::ui::panel::{self, PanelReply, PromptPanel};
use squawk_app::ui::popover::{self, Popover, PopoverData, PopoverEvent, Tab};
use squawk_app::ui::settings::Menu;
use squawk_app::ui::theme;
use squawk_core::dictionary::Entry;
use squawk_core::notetaker::calls::CallId;
use squawk_core::notetaker::heads_up::UpcomingEvent;
use squawk_core::notetaker::{Prompt, StopReason};
use squawk_core::status::Permissions;
use squawk_core::store::{DictationEntry, MeetingSummary};
use squawk_core::{ModelStatus, Paths};

struct Preview {
    popover: gpui::Entity<Popover>,
}

/// The prompt panels, stacked.
struct Panels {
    panels: Vec<gpui::Entity<PromptPanel>>,
}

impl Render for Panels {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(16.))
            .bg(gpui::rgb(0xd7d3cc))
            .p(px(24.))
            .children(self.panels.iter().map(|p| {
                div()
                    .w(panel::PANEL_WIDTH)
                    .h(px(panel::PANEL_HEIGHT_PX))
                    .child(p.clone())
            }))
    }
}

fn prompts() -> Vec<Prompt> {
    let start = Local::now()
        .date_naive()
        .and_hms_opt(15, 0, 0)
        .and_then(|t| Local.from_local_datetime(&t).earliest())
        .unwrap();
    vec![
        Prompt::HeadsUp {
            key: "design-review".into(),
            title: "Design review".into(),
            start,
            end: start + chrono::Duration::minutes(30),
            auto: false,
        },
        Prompt::HeadsUp {
            key: "design-review".into(),
            title: "Design review".into(),
            start,
            end: start + chrono::Duration::minutes(30),
            auto: true,
        },
        Prompt::Call {
            call: CallId(1),
            app: "Zoom".into(),
        },
        Prompt::Recording {
            title: "Design review".into(),
            app: "Google Meet".into(),
        },
        Prompt::StoppingSoon {
            title: "Weekly sync".into(),
            limit: Duration::from_secs(2 * 3600),
        },
        Prompt::Saved {
            title: "Weekly sync".into(),
            path: "/tmp/2026-09-29 1400 Weekly sync.md".into(),
            length_secs: 7200,
            reason: StopReason::MaxLength,
        },
    ]
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
    // Enough rows that the list has to scroll.
    let mut history = history;
    let older = [
        ("Mail", None, "Sounds good, see you Thursday."),
        ("Ghostty", Some("demo-app"), "Run the tests again with the verbose flag and paste me the first failure."),
        ("Notes", None, "Groceries: oat milk, lemons, rice, the good bread."),
        ("Slack", None, "I pushed the fix, can you take a look when you get a minute? It should unblock the release."),
        ("Ghostty", Some("squawk"), "Rename the helper to something that says what it does."),
        ("Safari", None, "How long does a cast iron pan take to season in the oven?"),
        ("Ghostty", Some("demo-app"), "Add a retry with exponential backoff around the upload, capped at five attempts."),
        ("Messages", None, "On my way."),
        ("Mail", None, "Attaching the notes from today's review. The open questions are at the bottom."),
        ("Ghostty", Some("squawk"), "Why does the popover clip the dictionary rows? Find the root cause first."),
    ];
    let two_days_ago = today - chrono::Days::new(2);
    for (i, (app, project, text)) in older.iter().enumerate() {
        history.push(DictationEntry {
            at: at(two_days_ago, 18 - i as u32, 30),
            app: (*app).into(),
            project: project.map(Into::into),
            text: (*text).into(),
        });
    }
    let mut meetings = vec![
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
    for (i, title) in [
        "Standup",
        "Roadmap planning for the next two quarters with the whole team",
        "1:1",
        "Customer call",
        "Retro",
        "Interview debrief",
        "Architecture review",
        "Standup",
        "Launch readiness",
    ]
    .iter()
    .enumerate()
    {
        meetings.push(MeetingSummary {
            path: format!("/tmp/meeting-{i}.md").into(),
            title: (*title).into(),
            started_at: Local
                .with_ymd_and_hms(2026, 9, 27 - i as u32, 10, 0, 0)
                .earliest(),
            length_secs: 900 + 300 * i as u64,
            in_progress: false,
            snippet: String::new(),
        });
    }
    let dictionary = [
        "Kubernetes",
        "ChatGPT",
        "cloud code -> Claude Code",
        "gee pee tee five -> GPT-5",
        "PostgreSQL",
        "post gress -> Postgres",
        "Tailscale",
        "WezTerm",
        "Ghostty",
        "someone dot example at example dot com -> someone.example@example.com",
        "type script -> TypeScript",
        "Grafana",
        "next js -> Next.js",
        "Anthropic",
        "the quick brown fox jumps over the lazy dog every single morning -> The quick brown fox jumps over the lazy dog",
        "Terraform",
        "rust up -> rustup",
        "Figma",
        "open telemetry -> OpenTelemetry",
        "Supabase",
        "git hub -> GitHub",
        "Hugging Face",
        "llama cpp -> llama.cpp",
        "Kafka",
        "a very long plain term that keeps going well past the edge of the popover",
        "web socket -> WebSocket",
    ]
    .iter()
    .filter_map(|line| Entry::parse(line))
    .collect();
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
                app: Some("Zoom".into()),
                mic_lost: false,
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
        "settings-nocal" | "meetings-nocal" => s.calendar_access = Some(false),
        _ => {}
    }
    if s.calendar_access.is_none() {
        s.calendar_access = Some(true);
    }
    s.permissions.screen_recording = Some(true);
    s.next_meeting = Some(next_meeting());
    s
}

/// A meeting 25 minutes from now with a Meet link.
fn next_meeting() -> UpcomingEvent {
    let start = Local::now() + chrono::Duration::minutes(25);
    UpcomingEvent {
        key: "design-review".into(),
        title: "Design review".into(),
        start,
        end: start + chrono::Duration::minutes(30),
        all_day: false,
        other_attendees: true,
        call_app: Some("Google Meet"),
        declined: false,
        cancelled: false,
    }
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "ready".into());
    let appearance = std::env::args().nth(2);
    let scroll_to_end = std::env::args().nth(3).as_deref() == Some("end");

    Application::new().run(move |cx: &mut App| {
        match appearance.as_deref() {
            None | Some("system") => {}
            Some(name) => force_appearance(name),
        }
        popover::bind_keys(cx);
        if mode == "panels" {
            open_panels(cx);
            return;
        }
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
                    "meetings" | "meeting" | "meetings-nocal" => Tab::Meetings,
                    "dictionary" => Tab::Dictionary,
                    "settings" | "settings-menu" | "settings-nocal" => Tab::Settings,
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
                if scroll_to_end {
                    popover.update(cx, |popover, cx| popover.scroll_to_end(cx));
                }
                if mode == "settings-menu" {
                    popover.update(cx, |popover, cx| popover.toggle_menu(Menu::HeadsUp, cx));
                }
                cx.new(|_| Preview { popover })
            },
        )
        .expect("open preview window");
        cx.activate(true);
    });
}

fn open_panels(cx: &mut App) {
    let prompts = prompts();
    let height = prompts.len() as f32 * (panel::PANEL_HEIGHT_PX + 16.0) + 32.0;
    let bounds = Bounds {
        origin: point(px(120.), px(120.)),
        size: size(px(panel::PANEL_WIDTH_PX + 48.), px(height)),
    };
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("squawk panels".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        |_, cx| {
            let panels = prompts
                .into_iter()
                .map(|prompt| {
                    let p = cx.new(|_| PromptPanel::new(Some(prompt)));
                    cx.subscribe(&p, |_, PanelReply(reply), _| {
                        println!("preview: panel {reply:?}");
                    })
                    .detach();
                    p
                })
                .collect();
            cx.new(|_| Panels { panels })
        },
    )
    .expect("open panels window");
    cx.activate(true);
}

/// Set the whole app's appearance, so a light or dark screenshot does not
/// need the system setting flipped.
fn force_appearance(name: &str) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication,
    };
    let mtm = MainThreadMarker::new().expect("main thread");
    let named = match name {
        "light" => unsafe { NSAppearanceNameAqua },
        "dark" => unsafe { NSAppearanceNameDarkAqua },
        other => panic!("appearance is light or dark, not {other}"),
    };
    let appearance = NSAppearance::appearanceNamed(named);
    NSApplication::sharedApplication(mtm).setAppearance(appearance.as_deref());
}
