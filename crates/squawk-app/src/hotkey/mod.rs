//! The global fn-key tap.
//!
//! A `CGEventTap` (session tap, head insert, active — not listen-only — so it
//! can swallow) for flagsChanged and keyDown, on its own thread with its own
//! CFRunLoop. [`driver`] turns each event into a
//! `squawk_core::hotkey::Input` and decides the outcome; the callback only
//! returns NULL when `Outcome.swallow` says so.
//!
//! Actions go to `on_action` (the controller) without blocking. The
//! double-tap window's end is fed as `Input::Tick` by a small ticker thread
//! that sleeps until `Machine::deadline()`: the machine also resolves an
//! expired window on the next event, so the ticker only makes a discarded
//! tap close the mic on time rather than at the next key press.
//!
//! When macOS disables the tap (kCGEventTapDisabledByTimeout /
//! ByUserInput) the callback re-enables it and resets the machine, since fn
//! events may have been missed. The callback stays fast: one short mutex
//! hold, no allocation beyond the outcome, no I/O.

pub mod driver;

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Instant;

use objc2_core_foundation::{kCFRunLoopCommonModes, CFMachPort, CFRetained, CFRunLoop};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use squawk_core::hotkey::{flag, keycode, Action, CancelReason, Input, Machine, Outcome, Timings};

use driver::{drive, RawEvent, RawKind};

/// The machine, shared by the tap thread (which must decide swallowing inside
/// the callback), the ticker, and the controller (which forces it idle when a
/// dictation ends some other way). Outlives any one tap, so a tap re-created
/// after Accessibility is granted keeps the same state.
#[derive(Clone)]
pub struct SharedMachine(Arc<Mutex<Machine>>);

impl SharedMachine {
    pub fn new(timings: Timings) -> SharedMachine {
        SharedMachine(Arc::new(Mutex::new(Machine::new(timings))))
    }

    fn lock(&self) -> MutexGuard<'_, Machine> {
        // A panic while holding the lock leaves a machine that is still a
        // valid state; keep using it.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Apply new timings (config reload).
    pub fn set_timings(&self, timings: Timings) {
        self.lock().set_timings(timings);
    }

    /// The controller ended a dictation itself (max length, an engine error):
    /// put the machine back to idle silently.
    pub fn force_idle(&self) {
        self.lock().force_idle();
    }
}

type ActionSink = Arc<dyn Fn(Action) + Send + Sync>;

#[derive(Debug, PartialEq, Eq)]
pub enum TapError {
    /// CGEventTapCreate returned NULL: Accessibility (or Input Monitoring) is
    /// not granted.
    NotTrusted,
}

/// A running tap. Dropping it stops the run loop and removes the tap.
pub struct HotkeyTap {
    run_loop: RunLoopHandle,
    ticker: Arc<Ticker>,
}

impl HotkeyTap {
    /// Create the tap on a new thread and return once it is installed (or
    /// refused).
    pub fn start(
        machine: SharedMachine,
        on_action: impl Fn(Action) + Send + Sync + 'static,
    ) -> Result<HotkeyTap, TapError> {
        let on_action: ActionSink = Arc::new(on_action);
        let ticker = Ticker::spawn(machine.clone(), on_action.clone());
        let (tx, rx) = mpsc::sync_channel(1);
        let tap_ticker = ticker.clone();
        thread::Builder::new()
            .name("squawk-hotkey".into())
            .spawn(move || run_tap(machine, on_action, tap_ticker, tx))
            .expect("spawn hotkey thread");
        match rx.recv() {
            Ok(Ok(run_loop)) => Ok(HotkeyTap { run_loop, ticker }),
            Ok(Err(e)) => {
                ticker.stop();
                Err(e)
            }
            Err(_) => {
                ticker.stop();
                Err(TapError::NotTrusted)
            }
        }
    }
}

impl Drop for HotkeyTap {
    fn drop(&mut self) {
        self.run_loop.0.stop();
        self.ticker.stop();
    }
}

/// The tap thread's run loop, so a drop elsewhere can stop it.
struct RunLoopHandle(CFRetained<CFRunLoop>);

// SAFETY: CFRunLoopStop is documented as callable from any thread; stopping
// is the only thing done with this handle off its own thread.
unsafe impl Send for RunLoopHandle {}
unsafe impl Sync for RunLoopHandle {}

/// What the C callback reaches through `user_info`.
struct TapState {
    machine: SharedMachine,
    on_action: ActionSink,
    ticker: Arc<Ticker>,
    /// The tap's own port, to re-enable it from inside the callback.
    port: AtomicPtr<CFMachPort>,
    /// When fn last came up, to log the gap when a double tap misses.
    last_fn_up: Mutex<Option<Instant>>,
}

fn run_tap(
    machine: SharedMachine,
    on_action: ActionSink,
    ticker: Arc<Ticker>,
    ready: mpsc::SyncSender<Result<RunLoopHandle, TapError>>,
) {
    let state = Box::into_raw(Box::new(TapState {
        machine,
        on_action,
        ticker,
        port: AtomicPtr::new(ptr::null_mut()),
        last_fn_up: Mutex::new(None),
    }));
    let mask: u64 = (1 << CGEventType::KeyDown.0) | (1 << CGEventType::FlagsChanged.0);
    // SAFETY: `callback` matches CGEventTapCallBack, and `state` stays alive
    // until after the run loop returns and the tap is invalidated below.
    let port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(callback),
            state.cast(),
        )
    };
    let Some(port) = port else {
        // SAFETY: never handed to a live tap.
        drop(unsafe { Box::from_raw(state) });
        let _ = ready.send(Err(TapError::NotTrusted));
        return;
    };
    // SAFETY: `state` is a live Box.
    unsafe { &*state }
        .port
        .store(CFRetained::as_ptr(&port).as_ptr(), Ordering::Release);

    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&port), 0) else {
        drop(unsafe { Box::from_raw(state) });
        let _ = ready.send(Err(TapError::NotTrusted));
        return;
    };
    let run_loop = CFRunLoop::current().expect("every thread has a run loop");
    // SAFETY: reading an immutable CF constant.
    run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
    CGEvent::tap_enable(&port, true);
    let _ = ready.send(Ok(RunLoopHandle(run_loop.clone())));
    log::info!("hotkey tap installed");

    CFRunLoop::run();

    CGEvent::tap_enable(&port, false);
    port.invalidate();
    // SAFETY: the port is invalidated, so the callback can no longer run.
    drop(unsafe { Box::from_raw(state) });
}

unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    let pass = event.as_ptr();
    if user_info.is_null() {
        return pass;
    }
    // SAFETY: `user_info` is the TapState box, alive for the tap's lifetime.
    let state = unsafe { &*(user_info as *const TapState) };
    // SAFETY: CoreGraphics hands us a valid event for the callback's duration.
    let event_ref = unsafe { event.as_ref() };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let raw = match event_type {
            CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
                let port = state.port.load(Ordering::Acquire);
                if !port.is_null() {
                    // SAFETY: the port outlives the callback (see run_tap).
                    CGEvent::tap_enable(unsafe { &*port }, true);
                }
                log::warn!("hotkey tap was disabled by macOS; re-enabled");
                RawEvent::tap_disabled()
            }
            CGEventType::FlagsChanged => {
                RawEvent::flags_changed(keycode_of(event_ref), CGEvent::flags(Some(event_ref)).0)
            }
            CGEventType::KeyDown => RawEvent::key_down(
                keycode_of(event_ref),
                CGEvent::flags(Some(event_ref)).0,
                CGEvent::integer_value_field(
                    Some(event_ref),
                    CGEventField::KeyboardEventAutorepeat,
                ) != 0,
            ),
            _ => return false,
        };
        let now = Instant::now();
        let outcome = {
            let mut machine = state.machine.lock();
            let outcome = drive(&mut machine, raw, now);
            state.ticker.arm(machine.deadline());
            outcome
        };
        log_gesture(state, &raw, &outcome, now);
        for action in outcome.actions {
            (state.on_action)(action);
        }
        outcome.swallow
    }));
    match result {
        Ok(true) => ptr::null_mut(),
        _ => pass,
    }
}

/// One line per missed or caught double tap, so a gesture that feels wrong
/// can be diagnosed from squawk.log.
fn log_gesture(state: &TapState, raw: &RawEvent, outcome: &Outcome, now: Instant) {
    let fn_edge = raw.kind == RawKind::FlagsChanged && raw.keycode == keycode::FN;
    let mut last_up = state.last_fn_up.lock().unwrap_or_else(|e| e.into_inner());
    if fn_edge && raw.flags & flag::FUNCTION == 0 {
        *last_up = Some(now);
        return;
    }
    let gap = last_up.map(|t| now.duration_since(t).as_millis());
    if outcome.actions.contains(&Action::EnterHandsFree) {
        log::info!(
            "hands-free: second fn press {} ms after release",
            gap.unwrap_or(0)
        );
    } else if fn_edge {
        if let Some(ms) = gap.filter(|ms| *ms < 1500) {
            log::info!("fn pressed {ms} ms after the last release: too late for a double tap");
        }
    } else if outcome.actions.contains(&Action::Cancel(CancelReason::Tap)) {
        log::info!("fn tap discarded by key {} ({:?})", raw.keycode, raw.kind);
    }
}

fn keycode_of(event: &CGEvent) -> u16 {
    CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode) as u16
}

/// Sleeps until the machine's deadline and feeds it `Input::Tick`.
struct Ticker {
    state: Mutex<TickerState>,
    wake: Condvar,
}

struct TickerState {
    deadline: Option<Instant>,
    stopped: bool,
}

impl Ticker {
    fn spawn(machine: SharedMachine, on_action: ActionSink) -> Arc<Ticker> {
        let ticker = Arc::new(Ticker {
            state: Mutex::new(TickerState {
                deadline: None,
                stopped: false,
            }),
            wake: Condvar::new(),
        });
        let this = ticker.clone();
        thread::Builder::new()
            .name("squawk-hotkey-ticker".into())
            .spawn(move || this.run(machine, on_action))
            .expect("spawn ticker thread");
        ticker
    }

    fn lock(&self) -> MutexGuard<'_, TickerState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn arm(&self, deadline: Option<Instant>) {
        let mut state = self.lock();
        if state.deadline != deadline {
            state.deadline = deadline;
            self.wake.notify_one();
        }
    }

    fn stop(&self) {
        self.lock().stopped = true;
        self.wake.notify_one();
    }

    fn run(&self, machine: SharedMachine, on_action: ActionSink) {
        let mut state = self.lock();
        loop {
            if state.stopped {
                return;
            }
            match state.deadline {
                None => {
                    state = self.wake.wait(state).unwrap_or_else(|e| e.into_inner());
                }
                Some(deadline) => {
                    let now = Instant::now();
                    if now < deadline {
                        state = self
                            .wake
                            .wait_timeout(state, deadline - now)
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                        continue;
                    }
                    state.deadline = None;
                    drop(state);
                    let actions = {
                        let mut m = machine.lock();
                        let now = Instant::now();
                        let due = m.deadline().is_some_and(|d| d <= now);
                        let actions = if due {
                            m.handle(Input::Tick, now).actions
                        } else {
                            Vec::new()
                        };
                        let next = m.deadline();
                        drop(m);
                        self.arm(next);
                        actions
                    };
                    for action in actions {
                        on_action(action);
                    }
                    state = self.lock();
                }
            }
        }
    }
}
