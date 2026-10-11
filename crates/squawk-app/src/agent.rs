//! The agent's side of the app: a worker thread that types dictations into
//! the agent's tmux session and shows or hides its terminal window.
//!
//! The tmux and terminal commands, and the order they run in, are
//! `squawk_core::agent`'s; this module only gives them a thread (a launch
//! waits seconds for Claude Code to boot, which must never hold up the
//! controller) and does the one AppKit thing core cannot: hiding the
//! terminal when ⌥+fn is tapped with it in front.
//!
//! Jobs run one at a time, in order, so a dictation sent while the session
//! is still launching lands after the launch, and two quick dictations
//! arrive in the order they were spoken.

use std::thread;
use std::time::Instant;

use crossbeam_channel::Sender;
use objc2::rc::autoreleasepool;
use objc2_app_kit::NSRunningApplication;
use squawk_core::agent::{self, Setup, System};

use crate::controller::Metrics;
use crate::frontmost;

/// Work for the agent thread.
pub enum Job {
    /// Type `messages` into the session, launching it first if needed, then
    /// log the dictation's line (`metrics`, timed from `released`).
    Deliver {
        setup: Setup,
        messages: Vec<String>,
        released: Instant,
        metrics: Box<Metrics>,
    },
    /// Bring the session's terminal window forward (launching the session
    /// if it is not there).
    Show(Setup),
    /// ⌥+fn tap: hide the terminal if it is in front showing the session,
    /// else show it.
    Toggle(Setup),
}

/// Handle to the agent thread.
#[derive(Clone)]
pub struct AgentWorker {
    tx: Sender<Job>,
}

impl AgentWorker {
    /// Start the thread. `failed` is called on it with what went wrong when
    /// a job fails, and whether it was a delivery (the controller shows it,
    /// and leaves the working state only when a request did not get there).
    pub fn spawn(failed: impl Fn(String, bool) + Send + 'static) -> AgentWorker {
        let (tx, rx) = crossbeam_channel::unbounded::<Job>();
        thread::Builder::new()
            .name("squawk-agent".into())
            .spawn(move || {
                for job in rx {
                    let delivery = matches!(job, Job::Deliver { .. });
                    if let Err(e) = run(job) {
                        log::warn!("agent: {e}");
                        failed(e, delivery);
                    }
                }
            })
            .expect("spawn agent thread");
        AgentWorker { tx }
    }

    /// Never blocks.
    pub fn send(&self, job: Job) {
        let _ = self.tx.send(job);
    }
}

fn run(job: Job) -> Result<(), String> {
    match job {
        Job::Deliver {
            setup,
            messages,
            released,
            mut metrics,
        } => {
            let start = Instant::now();
            let delivered = agent::deliver(&mut System, &setup, &messages);
            metrics.paste_ms = start.elapsed().as_millis();
            metrics.release_to_paste_ms = released.elapsed().as_millis();
            // One line per dictation, delivered or not, as for a paste.
            log::info!(target: "dictation", "{}", metrics.line());
            if delivered?.launched {
                log::info!(target: "agent", "delivered after launching {}", setup.session);
            }
            Ok(())
        }
        Job::Show(setup) => agent::show(&mut System, &setup),
        Job::Toggle(setup) => toggle(&setup),
    }
}

fn toggle(setup: &Setup) -> Result<(), String> {
    let front = frontmost::front_app().filter(|f| setup.is_terminal(&f.name));
    if let Some(front) = front {
        if agent::attached(&mut System, setup) && hide(front.pid) {
            return Ok(());
        }
    }
    agent::show(&mut System, setup)
}

/// Hide the app with `pid` (cmd+H, as if the user pressed it). Whatever
/// was behind it comes forward.
fn hide(pid: i32) -> bool {
    autoreleasepool(|_| {
        NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .is_some_and(|app| app.hide())
    })
}
