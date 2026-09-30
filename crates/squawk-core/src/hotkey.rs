//! The fn-key state machine: raw key events in, dictation actions out.
//!
//! Pure and clock-free — every input carries its own `Instant` — so the
//! Wispr-Flow feel can be pinned down in tests. The app's event tap turns
//! CGEvents into [`Input`]s, feeds them here, performs the [`Action`]s, and
//! arms a timer for [`Machine::deadline`] that feeds [`Input::Tick`].
//!
//! The gestures:
//! - **Hold** fn (longer than `tap_max`): push-to-talk. Release pastes.
//! - **Double tap** fn: hands-free. Recording keeps going after release; the
//!   next fn press-and-release stops it and pastes.
//! - **Single tap** with no second tap within `double_tap`: discarded.
//! - fn **with another key** (fn+arrow, fn+F5, fn+cmd…): fn is a modifier;
//!   cancel silently and let the key through.
//! - **esc** while recording cancels (and is swallowed, so a terminal behind
//!   squawk does not also get it).
//! - When idle: ctrl+cmd+V pastes the last dictation, ctrl+cmd+C copies it,
//!   option+M starts or stops a meeting. These are swallowed.
//!
//! Recording starts on the very first fn down, before the machine knows which
//! gesture it is, so no speech is lost to the decision. A tap that turns out
//! to be nothing throws that audio away.

use std::time::{Duration, Instant};

pub const DEFAULT_TAP_MAX_MS: u64 = 300;
pub const DEFAULT_DOUBLE_TAP_MS: u64 = 400;

/// macOS virtual keycodes the machine cares about (kVK_* in HIToolbox).
pub mod keycode {
    pub const FN: u16 = 63;
    /// Apple keyboards with a 🌐 key also send a keyDown with this code when
    /// it is pressed. It is fn itself, never "another key".
    pub const GLOBE: u16 = 179;
    pub const ESCAPE: u16 = 53;
    pub const ANSI_C: u16 = 8;
    pub const ANSI_V: u16 = 9;
    pub const ANSI_M: u16 = 46;
}

/// CGEventFlags bits (same values as NSEventModifierFlags).
pub mod flag {
    pub const SHIFT: u64 = 0x0002_0000;
    pub const CONTROL: u64 = 0x0004_0000;
    pub const OPTION: u64 = 0x0008_0000;
    pub const COMMAND: u64 = 0x0010_0000;
    /// NSEventModifierFlagFunction / kCGEventFlagMaskSecondaryFn. Note:
    /// arrow and F-key keyDowns carry this bit even when fn is not held, so
    /// fn state must come from flagsChanged on keycode 63, never from this bit
    /// on a keyDown.
    pub const FUNCTION: u64 = 0x0080_0000;
}

/// The modifiers other than fn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub control: bool,
    pub option: bool,
    pub command: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        shift: false,
        control: false,
        option: false,
        command: false,
    };

    /// From raw CGEventFlags.
    pub fn from_flags(flags: u64) -> Mods {
        Mods {
            shift: flags & flag::SHIFT != 0,
            control: flags & flag::CONTROL != 0,
            option: flags & flag::OPTION != 0,
            command: flags & flag::COMMAND != 0,
        }
    }

    pub fn any(self) -> bool {
        self.shift || self.control || self.option || self.command
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// flagsChanged, keycode 63, fn bit now set. `mods` are the other
    /// modifiers held at that moment.
    FnDown { mods: Mods },
    /// flagsChanged, keycode 63, fn bit now clear.
    FnUp,
    /// flagsChanged for any other modifier key.
    ModsChanged { mods: Mods },
    /// keyDown (autorepeats included, `repeat` set).
    KeyDown {
        keycode: u16,
        mods: Mods,
        repeat: bool,
    },
    /// The timer armed for [`Machine::deadline`] fired.
    Tick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    /// A single tap: no second tap came.
    Tap,
    /// fn was used as a modifier for another key.
    FnAsModifier,
    /// esc.
    Escape,
    /// The app asked ([`Machine::reset`]): lost events, sleep, a failure.
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Open the mic and start a dictation.
    StartRecording,
    /// The dictation is now hands-free (the menu bar shows the lock).
    EnterHandsFree,
    /// Stop, transcribe the tail, paste.
    StopAndPaste,
    /// Stop and throw the audio away.
    Cancel(CancelReason),
    /// Paste the last dictation again (ctrl+cmd+V).
    PasteLast,
    /// Copy the last dictation to the clipboard (ctrl+cmd+C).
    CopyLast,
    /// Start or stop a meeting (option+M).
    ToggleMeeting,
}

/// What the tap does with the event, and what the app should do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub actions: Vec<Action>,
    /// Drop the event instead of passing it on. Only ever set for keyDowns;
    /// flagsChanged events always pass through.
    pub swallow: bool,
}

impl Outcome {
    fn none() -> Outcome {
        Outcome::default()
    }

    fn act(action: Action) -> Outcome {
        Outcome {
            actions: vec![action],
            swallow: false,
        }
    }

    fn swallowed(mut self) -> Outcome {
        self.swallow = true;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    /// fn is down, recording; not yet known whether this is a hold or a tap.
    Pressed {
        down_at: Instant,
    },
    /// A tap was released; recording continues while we wait for a second.
    TapReleased {
        up_at: Instant,
    },
    /// The second tap is down; hands-free once it comes up.
    SecondPress,
    /// Hands-free recording, fn up.
    HandsFree,
    /// Hands-free, fn down: stops on release unless another key joins it.
    HandsFreeFnDown,
}

impl State {
    /// The mic is open in this state.
    pub fn is_recording(self) -> bool {
        !matches!(self, State::Idle)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timings {
    pub tap_max: Duration,
    pub double_tap: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Timings {
            tap_max: Duration::from_millis(DEFAULT_TAP_MAX_MS),
            double_tap: Duration::from_millis(DEFAULT_DOUBLE_TAP_MS),
        }
    }
}

impl From<&crate::config::HotkeyConfig> for Timings {
    fn from(c: &crate::config::HotkeyConfig) -> Self {
        Timings {
            tap_max: Duration::from_millis(c.tap_max_ms),
            double_tap: Duration::from_millis(c.double_tap_ms),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Machine {
    state: State,
    timings: Timings,
}

impl Machine {
    pub fn new(timings: Timings) -> Machine {
        Machine {
            state: State::Idle,
            timings,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn set_timings(&mut self, timings: Timings) {
        self.timings = timings;
    }

    /// When the app must feed [`Input::Tick`]: the end of the double-tap
    /// window. `None` when nothing is pending.
    pub fn deadline(&self) -> Option<Instant> {
        match self.state {
            State::TapReleased { up_at } => Some(up_at + self.timings.double_tap),
            _ => None,
        }
    }

    /// Back to idle from wherever we are, e.g. after the tap was disabled and
    /// fn events may have been missed, or the engine failed. Returns the
    /// cancel to perform if a recording was running.
    pub fn reset(&mut self) -> Option<Action> {
        let was = self.state;
        self.state = State::Idle;
        was.is_recording()
            .then_some(Action::Cancel(CancelReason::Reset))
    }

    /// The app finished a dictation some other way (max length reached, the
    /// user clicked stop): forget it without emitting anything.
    pub fn force_idle(&mut self) {
        self.state = State::Idle;
    }

    pub fn handle(&mut self, input: Input, now: Instant) -> Outcome {
        // A pending double-tap window that has run out resolves first, so a
        // late Tick and a late press behave the same.
        let mut pre = Vec::new();
        if let State::TapReleased { up_at } = self.state {
            if now.duration_since(up_at) >= self.timings.double_tap {
                self.state = State::Idle;
                pre.push(Action::Cancel(CancelReason::Tap));
            }
        }
        let mut out = self.step(input, now);
        if !pre.is_empty() {
            pre.append(&mut out.actions);
            out.actions = pre;
        }
        out
    }

    fn step(&mut self, input: Input, now: Instant) -> Outcome {
        use State::*;
        let is_escape = |keycode: u16| keycode == keycode::ESCAPE;
        match (self.state, input) {
            (_, Input::Tick) => Outcome::none(),

            // ---- idle
            (Idle, Input::FnDown { mods }) if !mods.any() => {
                self.state = Pressed { down_at: now };
                Outcome::act(Action::StartRecording)
            }
            (
                Idle,
                Input::KeyDown {
                    keycode,
                    mods,
                    repeat,
                },
            ) => match global_combo(keycode, mods) {
                Some(action) if !repeat => Outcome::act(action).swallowed(),
                // Swallow held-down repeats of a combo too, or the letter
                // would start typing after the first press.
                Some(_) => Outcome::none().swallowed(),
                None => Outcome::none(),
            },
            (Idle, _) => Outcome::none(),

            // ---- first press, undecided
            (Pressed { down_at }, Input::FnUp) => {
                if now.duration_since(down_at) < self.timings.tap_max {
                    self.state = TapReleased { up_at: now };
                    Outcome::none()
                } else {
                    self.state = Idle;
                    Outcome::act(Action::StopAndPaste)
                }
            }
            (Pressed { .. }, Input::KeyDown { keycode, .. }) if is_escape(keycode) => {
                self.state = Idle;
                Outcome::act(Action::Cancel(CancelReason::Escape)).swallowed()
            }
            (Pressed { .. }, Input::KeyDown { .. }) => {
                self.state = Idle;
                Outcome::act(Action::Cancel(CancelReason::FnAsModifier))
            }
            (Pressed { .. }, Input::ModsChanged { mods }) if mods.any() => {
                self.state = Idle;
                Outcome::act(Action::Cancel(CancelReason::FnAsModifier))
            }
            (Pressed { .. }, _) => Outcome::none(),

            // ---- between the taps
            (TapReleased { .. }, Input::FnDown { mods }) if !mods.any() => {
                self.state = SecondPress;
                Outcome::act(Action::EnterHandsFree)
            }
            (TapReleased { .. }, Input::KeyDown { keycode, .. }) if is_escape(keycode) => {
                self.state = Idle;
                Outcome::act(Action::Cancel(CancelReason::Escape)).swallowed()
            }
            (TapReleased { .. }, Input::KeyDown { .. } | Input::FnDown { .. }) => {
                // Typing (or fn+modifier) right after a tap: the tap was nothing.
                self.state = Idle;
                Outcome::act(Action::Cancel(CancelReason::Tap))
            }
            (TapReleased { .. }, _) => Outcome::none(),

            // ---- second press held
            (SecondPress, Input::FnUp) => {
                self.state = HandsFree;
                Outcome::none()
            }
            (SecondPress, Input::KeyDown { keycode, .. }) if is_escape(keycode) => {
                self.state = Idle;
                Outcome::act(Action::Cancel(CancelReason::Escape)).swallowed()
            }
            (SecondPress, Input::KeyDown { .. }) => {
                self.state = Idle;
                Outcome::act(Action::Cancel(CancelReason::FnAsModifier))
            }
            (SecondPress, Input::ModsChanged { mods }) if mods.any() => {
                self.state = Idle;
                Outcome::act(Action::Cancel(CancelReason::FnAsModifier))
            }
            (SecondPress, _) => Outcome::none(),

            // ---- hands-free
            (HandsFree, Input::FnDown { mods }) if !mods.any() => {
                self.state = HandsFreeFnDown;
                Outcome::none()
            }
            (HandsFree | HandsFreeFnDown, Input::KeyDown { keycode, .. }) if is_escape(keycode) => {
                self.state = Idle;
                Outcome::act(Action::Cancel(CancelReason::Escape)).swallowed()
            }
            (HandsFree, _) => Outcome::none(),
            (HandsFreeFnDown, Input::FnUp) => {
                self.state = Idle;
                Outcome::act(Action::StopAndPaste)
            }
            // fn+key during hands-free is the user doing something else
            // (fn+arrow to scroll); keep recording.
            (HandsFreeFnDown, Input::KeyDown { .. }) => {
                self.state = HandsFree;
                Outcome::none()
            }
            (HandsFreeFnDown, Input::ModsChanged { mods }) if mods.any() => {
                self.state = HandsFree;
                Outcome::none()
            }
            (HandsFreeFnDown, _) => Outcome::none(),
        }
    }
}

/// The idle-only shortcuts. Matched on keycode (layout-independent, the way
/// Wispr does it) and on the exact modifier set.
fn global_combo(keycode: u16, mods: Mods) -> Option<Action> {
    let ctrl_cmd = Mods {
        control: true,
        command: true,
        ..Mods::NONE
    };
    let opt = Mods {
        option: true,
        ..Mods::NONE
    };
    match (keycode, mods) {
        (keycode::ANSI_V, m) if m == ctrl_cmd => Some(Action::PasteLast),
        (keycode::ANSI_C, m) if m == ctrl_cmd => Some(Action::CopyLast),
        (keycode::ANSI_M, m) if m == opt => Some(Action::ToggleMeeting),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Clock {
        t0: Instant,
        m: Machine,
    }

    impl Clock {
        fn new() -> Clock {
            Clock {
                t0: Instant::now(),
                m: Machine::new(Timings::default()),
            }
        }
        fn at(&mut self, ms: u64, input: Input) -> Outcome {
            self.m.handle(input, self.t0 + Duration::from_millis(ms))
        }
        fn acts(&mut self, ms: u64, input: Input) -> Vec<Action> {
            self.at(ms, input).actions
        }
    }

    const DOWN: Input = Input::FnDown { mods: Mods::NONE };
    const UP: Input = Input::FnUp;

    fn key(keycode: u16) -> Input {
        Input::KeyDown {
            keycode,
            mods: Mods::NONE,
            repeat: false,
        }
    }

    fn key_with(keycode: u16, mods: Mods) -> Input {
        Input::KeyDown {
            keycode,
            mods,
            repeat: false,
        }
    }

    const ARROW_LEFT: u16 = 123;

    #[test]
    fn hold_is_push_to_talk() {
        let mut c = Clock::new();
        assert_eq!(c.acts(0, DOWN), [Action::StartRecording]);
        assert_eq!(c.acts(1200, UP), [Action::StopAndPaste]);
        assert_eq!(c.m.state(), State::Idle);
    }

    #[test]
    fn hold_exactly_tap_max_is_push_to_talk() {
        let mut c = Clock::new();
        c.acts(0, DOWN);
        assert_eq!(c.acts(DEFAULT_TAP_MAX_MS, UP), [Action::StopAndPaste]);
    }

    #[test]
    fn single_tap_is_discarded_after_window() {
        let mut c = Clock::new();
        assert_eq!(c.acts(0, DOWN), [Action::StartRecording]);
        assert!(c.acts(120, UP).is_empty());
        let deadline = c.m.deadline().unwrap();
        assert_eq!(
            deadline,
            c.t0 + Duration::from_millis(120 + DEFAULT_DOUBLE_TAP_MS)
        );
        assert!(c
            .acts(120 + DEFAULT_DOUBLE_TAP_MS - 1, Input::Tick)
            .is_empty());
        assert_eq!(
            c.acts(120 + DEFAULT_DOUBLE_TAP_MS, Input::Tick),
            [Action::Cancel(CancelReason::Tap)]
        );
        assert_eq!(c.m.state(), State::Idle);
        assert!(c.m.deadline().is_none());
    }

    #[test]
    fn double_tap_is_hands_free_and_a_press_stops_it() {
        let mut c = Clock::new();
        c.acts(0, DOWN);
        c.acts(100, UP);
        assert_eq!(c.acts(250, DOWN), [Action::EnterHandsFree]);
        assert!(c.acts(330, UP).is_empty());
        assert_eq!(c.m.state(), State::HandsFree);
        // Talking for a while, typing even.
        let out = c.at(5000, key(0));
        assert!(out.actions.is_empty() && !out.swallow);
        assert!(c.acts(20_000, DOWN).is_empty());
        assert_eq!(c.acts(20_100, UP), [Action::StopAndPaste]);
        assert_eq!(c.m.state(), State::Idle);
    }

    #[test]
    fn slow_second_tap_discards_first_and_starts_fresh() {
        let mut c = Clock::new();
        c.acts(0, DOWN);
        c.acts(100, UP);
        // No Tick arrived (timer late); the press after the window resolves it.
        assert_eq!(
            c.acts(100 + DEFAULT_DOUBLE_TAP_MS + 50, DOWN),
            [Action::Cancel(CancelReason::Tap), Action::StartRecording]
        );
        assert!(matches!(c.m.state(), State::Pressed { .. }));
    }

    #[test]
    fn fn_with_another_key_cancels_and_passes_the_key() {
        let mut c = Clock::new();
        c.acts(0, DOWN);
        let out = c.at(80, key(ARROW_LEFT));
        assert_eq!(out.actions, [Action::Cancel(CancelReason::FnAsModifier)]);
        assert!(!out.swallow);
        // The release afterwards does nothing.
        assert!(c.acts(300, UP).is_empty());
        assert_eq!(c.m.state(), State::Idle);
    }

    #[test]
    fn fn_then_modifier_cancels() {
        let mut c = Clock::new();
        c.acts(0, DOWN);
        let cmd = Mods {
            command: true,
            ..Mods::NONE
        };
        assert_eq!(
            c.acts(50, Input::ModsChanged { mods: cmd }),
            [Action::Cancel(CancelReason::FnAsModifier)]
        );
    }

    #[test]
    fn modifier_then_fn_is_ignored() {
        let mut c = Clock::new();
        let ctrl = Mods {
            control: true,
            ..Mods::NONE
        };
        assert!(c.acts(0, Input::FnDown { mods: ctrl }).is_empty());
        assert!(c.acts(500, UP).is_empty());
        assert_eq!(c.m.state(), State::Idle);
    }

    #[test]
    fn escape_cancels_in_every_recording_state_and_is_swallowed() {
        // While holding.
        let mut c = Clock::new();
        c.acts(0, DOWN);
        let out = c.at(500, key(keycode::ESCAPE));
        assert_eq!(out.actions, [Action::Cancel(CancelReason::Escape)]);
        assert!(out.swallow);
        // Between taps.
        let mut c = Clock::new();
        c.acts(0, DOWN);
        c.acts(100, UP);
        let out = c.at(150, key(keycode::ESCAPE));
        assert_eq!(out.actions, [Action::Cancel(CancelReason::Escape)]);
        assert!(out.swallow);
        // Hands-free.
        let mut c = Clock::new();
        c.acts(0, DOWN);
        c.acts(100, UP);
        c.acts(200, DOWN);
        c.acts(280, UP);
        let out = c.at(9000, key(keycode::ESCAPE));
        assert_eq!(out.actions, [Action::Cancel(CancelReason::Escape)]);
        assert!(out.swallow);
        assert_eq!(c.m.state(), State::Idle);
    }

    #[test]
    fn escape_when_idle_passes_through() {
        let mut c = Clock::new();
        let out = c.at(0, key(keycode::ESCAPE));
        assert!(out.actions.is_empty());
        assert!(!out.swallow);
    }

    #[test]
    fn typing_right_after_a_tap_discards_it() {
        let mut c = Clock::new();
        c.acts(0, DOWN);
        c.acts(90, UP);
        let out = c.at(150, key(0));
        assert_eq!(out.actions, [Action::Cancel(CancelReason::Tap)]);
        assert!(!out.swallow);
    }

    #[test]
    fn fn_arrow_during_hands_free_keeps_recording() {
        let mut c = Clock::new();
        c.acts(0, DOWN);
        c.acts(100, UP);
        c.acts(200, DOWN);
        c.acts(280, UP);
        assert!(c.acts(3000, DOWN).is_empty());
        assert!(c.acts(3050, key(ARROW_LEFT)).is_empty());
        assert!(c.acts(3200, UP).is_empty());
        assert_eq!(c.m.state(), State::HandsFree);
        c.acts(6000, DOWN);
        assert_eq!(c.acts(6090, UP), [Action::StopAndPaste]);
    }

    #[test]
    fn fn_combo_on_second_press_cancels() {
        let mut c = Clock::new();
        c.acts(0, DOWN);
        c.acts(100, UP);
        c.acts(200, DOWN);
        assert_eq!(
            c.acts(250, key(ARROW_LEFT)),
            [Action::Cancel(CancelReason::FnAsModifier)]
        );
    }

    #[test]
    fn global_combos_only_when_idle() {
        let ctrl_cmd = Mods {
            control: true,
            command: true,
            ..Mods::NONE
        };
        let opt = Mods {
            option: true,
            ..Mods::NONE
        };
        let mut c = Clock::new();
        let out = c.at(0, key_with(keycode::ANSI_V, ctrl_cmd));
        assert_eq!(out.actions, [Action::PasteLast]);
        assert!(out.swallow);
        assert_eq!(
            c.acts(10, key_with(keycode::ANSI_C, ctrl_cmd)),
            [Action::CopyLast]
        );
        assert_eq!(
            c.acts(20, key_with(keycode::ANSI_M, opt)),
            [Action::ToggleMeeting]
        );
        // Plain cmd+V is not ours.
        let cmd = Mods {
            command: true,
            ..Mods::NONE
        };
        let out = c.at(30, key_with(keycode::ANSI_V, cmd));
        assert!(out.actions.is_empty() && !out.swallow);
        // Option+shift+M is not ours either.
        let opt_shift = Mods {
            option: true,
            shift: true,
            ..Mods::NONE
        };
        assert!(c.acts(40, key_with(keycode::ANSI_M, opt_shift)).is_empty());
        // Repeats are swallowed without acting again.
        let out = c.at(
            50,
            Input::KeyDown {
                keycode: keycode::ANSI_M,
                mods: opt,
                repeat: true,
            },
        );
        assert!(out.actions.is_empty() && out.swallow);
    }

    #[test]
    fn reset_cancels_a_running_recording() {
        let mut c = Clock::new();
        assert_eq!(c.m.reset(), None);
        c.acts(0, DOWN);
        assert_eq!(c.m.reset(), Some(Action::Cancel(CancelReason::Reset)));
        assert_eq!(c.m.state(), State::Idle);
    }

    #[test]
    fn duplicate_fn_down_while_pressed_is_ignored() {
        let mut c = Clock::new();
        c.acts(0, DOWN);
        assert!(c.acts(50, DOWN).is_empty());
        assert_eq!(c.acts(900, UP), [Action::StopAndPaste]);
    }

    #[test]
    fn mods_from_flags() {
        let m = Mods::from_flags(flag::CONTROL | flag::COMMAND | flag::FUNCTION);
        assert!(m.control && m.command && !m.option && !m.shift);
        assert!(!Mods::from_flags(flag::FUNCTION).any());
    }
}
