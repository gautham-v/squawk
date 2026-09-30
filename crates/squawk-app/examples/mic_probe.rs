//! Lists the processes using a microphone, twice a second, the way the call
//! detector sees them: pid, bundle id, name, and what `classify` makes of it.
//! Start and stop a recording in another app to watch it come and go.
//!
//! `cargo run -p squawk-app --example mic_probe [seconds]`
//!
//! `mic_probe watch [seconds]` runs the real pipeline instead: the
//! listener-driven `MicWatcher` feeding a `Notetaker` ticked once a second,
//! printing each change, answering Start to a call prompt and reporting when
//! the meeting would stop (only at the maximum length: a call ending never
//! stops one).

use std::time::{Duration, Instant};

use squawk_app::mic_watch;
use squawk_core::notetaker::{calls, Auto, Notetaker, Outcome, Reply, Settings};

fn main() {
    if std::env::args().nth(1).as_deref() == Some("watch") {
        let secs = std::env::args()
            .nth(2)
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);
        watch(secs);
        return;
    }
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let own = std::process::id() as i32;
    let start = Instant::now();
    let mut last = String::new();
    while start.elapsed() < Duration::from_secs(secs) {
        let users = mic_watch::input_processes();
        let mut line = String::new();
        for u in &users {
            let what = match calls::classify(u, own) {
                Some(calls::Source::App(name)) => format!("call app {name}"),
                Some(calls::Source::Browser { bundle_id, name }) => {
                    let titles = mic_watch::window_titles(bundle_id);
                    let site = titles.iter().find_map(|t| calls::call_site(t));
                    format!("browser {name}, titles {titles:?} -> {site:?}")
                }
                None => "not a call app".into(),
            };
            line.push_str(&format!(
                "\n    pid {} bundle {:?} name {:?}: {what}",
                u.pid, u.bundle_id, u.name
            ));
        }
        let calls_now = mic_watch::call_apps(own);
        let line = format!(
            "{} using input{line}\n  call apps: {calls_now:?}",
            users.len()
        );
        if line != last {
            println!("[{:5.1}s] {line}", start.elapsed().as_secs_f32());
            last = line;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn watch(secs: u64) {
    let own = std::process::id() as i32;
    let start = Instant::now();
    let t = move || start.elapsed().as_secs_f32();
    let (tx, rx) = std::sync::mpsc::channel::<Vec<String>>();
    mic_watch::MicWatcher::start(own, move |apps| {
        println!("[{:5.1}s] watcher: call apps using the mic {apps:?}", t());
        let _ = tx.send(apps);
    });
    let mut notetaker = Notetaker::new(Settings::default());
    let mut apps: Vec<String> = Vec::new();
    let mut shown = None;
    let mut next_tick = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        let wait = next_tick.saturating_duration_since(Instant::now());
        if let Ok(latest) = rx.recv_timeout(wait) {
            apps = latest;
        }
        if Instant::now() < next_tick {
            continue;
        }
        next_tick += Duration::from_secs(1);
        let now = Instant::now();
        if let Some(Auto::Stop(reason)) = notetaker.tick(now, chrono::Local::now(), &apps, &[]) {
            println!("[{:5.1}s] notetaker: stop the meeting ({reason:?})", t());
            notetaker.meeting_stopped();
        }
        let prompt = notetaker.prompt().cloned();
        if prompt != shown {
            println!("[{:5.1}s] notetaker: prompt {prompt:?}", t());
            shown = prompt.clone();
            if let Some(squawk_core::notetaker::Prompt::Call { .. }) = prompt {
                if let Outcome::StartMeeting { fallback, call, .. } = notetaker.reply(Reply::Accept)
                {
                    println!(
                        "[{:5.1}s] probe: pressed Start, meeting \"{fallback}\"",
                        t()
                    );
                    notetaker.meeting_started(now, &fallback, call);
                    shown = None;
                }
            }
        }
    }
}
