//! The global fn-key tap.
//!
//! A `CGEventTap` (kCGSessionEventTap, head insert, default — not listen-only —
//! so it can swallow) for flagsChanged and keyDown, on its own thread with
//! its own CFRunLoop. Each event becomes a `squawk_core::hotkey::Input`:
//! - flagsChanged with keycode 63 → `FnDown`/`FnUp` from the 0x800000 bit;
//! - other flagsChanged → `ModsChanged`;
//! - keyDown → `KeyDown` (autorepeat from kCGKeyboardEventAutorepeat).
//!
//! The machine's `Outcome.swallow` decides whether the callback returns NULL.
//! Actions go to `on_action` (the controller) without blocking. A
//! CFRunLoopTimer on the same run loop is re-armed to `Machine::deadline()`
//! after every event and feeds `Input::Tick`.
//!
//! When macOS disables the tap (kCGEventTapDisabledByTimeout /
//! ByUserInput) the callback re-enables it with CGEventTapEnable and calls
//! `Machine::reset()` (fn events may have been missed). The callback must
//! stay fast — no allocation-heavy work, no locks held across anything slow.

use std::sync::{Arc, Mutex};

use squawk_core::hotkey::{Action, Machine, Timings};

/// A running tap. Dropping it stops the run loop and removes the tap.
pub struct HotkeyTap {
    machine: Arc<Mutex<Machine>>,
}

#[derive(Debug)]
pub enum TapError {
    /// CGEventTapCreate returned NULL: Accessibility / Input Monitoring is
    /// not granted.
    NotTrusted,
}

impl HotkeyTap {
    pub fn start(
        timings: Timings,
        on_action: impl Fn(Action) + Send + 'static,
    ) -> Result<HotkeyTap, TapError> {
        let _ = (timings, on_action);
        todo!("app agent")
    }

    /// Apply new timings (config reload).
    pub fn set_timings(&self, timings: Timings) {
        self.machine.lock().expect("machine").set_timings(timings);
    }

    /// The controller ended a dictation itself (max length, stop from the
    /// popover, engine error): put the machine back to idle silently.
    pub fn force_idle(&self) {
        self.machine.lock().expect("machine").force_idle();
    }
}
