//! The pure half of the event tap: a raw CGEvent (its type, keycode, flags
//! and autorepeat bit) in, the machine's [`Outcome`] out.
//!
//! `squawk_core::hotkey::Machine` decides what a gesture means; this module
//! decides what a CGEvent *is*, which is where the macOS quirks live:
//! - fn state comes only from flagsChanged on keycode 63. Arrow and F-key
//!   keyDowns carry the fn bit without fn being held, so the bit on a keyDown
//!   says nothing.
//! - flagsChanged is never swallowed: dropping a modifier change leaves the
//!   system believing the key is still down.
//! - When macOS disables the tap (a slow callback, or secure input) events
//!   were lost, so the machine is reset rather than trusted.
//!
//! Kept free of CoreGraphics so every gesture can be replayed in tests with
//! synthetic timestamps and the exact flag values the tap sees.

use std::time::Instant;

use squawk_core::hotkey::{flag, keycode, Input, Machine, Mods, Outcome};

/// The event types the tap subscribes to, plus the tap's own "I was switched
/// off" notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawKind {
    FlagsChanged,
    KeyDown,
    /// kCGEventTapDisabledByTimeout / kCGEventTapDisabledByUserInput.
    TapDisabled,
}

/// One event as the tap callback reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawEvent {
    pub kind: RawKind,
    pub keycode: u16,
    /// Raw CGEventFlags.
    pub flags: u64,
    /// kCGKeyboardEventAutorepeat was non-zero.
    pub autorepeat: bool,
}

impl RawEvent {
    pub fn flags_changed(keycode: u16, flags: u64) -> RawEvent {
        RawEvent {
            kind: RawKind::FlagsChanged,
            keycode,
            flags,
            autorepeat: false,
        }
    }

    pub fn key_down(keycode: u16, flags: u64, autorepeat: bool) -> RawEvent {
        RawEvent {
            kind: RawKind::KeyDown,
            keycode,
            flags,
            autorepeat,
        }
    }

    pub fn tap_disabled() -> RawEvent {
        RawEvent {
            kind: RawKind::TapDisabled,
            keycode: 0,
            flags: 0,
            autorepeat: false,
        }
    }
}

/// What a raw event means to the machine. `None` for the tap-disabled
/// notification, which is not an input but a reason to reset.
pub fn translate(event: &RawEvent) -> Option<Input> {
    let mods = Mods::from_flags(event.flags);
    match event.kind {
        RawKind::FlagsChanged if event.keycode == keycode::FN => {
            if event.flags & flag::FUNCTION != 0 {
                Some(Input::FnDown { mods })
            } else {
                Some(Input::FnUp)
            }
        }
        RawKind::FlagsChanged => Some(Input::ModsChanged { mods }),
        RawKind::KeyDown => Some(Input::KeyDown {
            keycode: event.keycode,
            mods,
            repeat: event.autorepeat,
        }),
        RawKind::TapDisabled => None,
    }
}

/// Feed one raw event to the machine.
pub fn drive(machine: &mut Machine, event: RawEvent, now: Instant) -> Outcome {
    match translate(&event) {
        Some(input) => {
            let mut outcome = machine.handle(input, now);
            if event.kind == RawKind::FlagsChanged {
                outcome.swallow = false;
            }
            outcome
        }
        None => Outcome {
            actions: machine.reset().into_iter().collect(),
            swallow: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use squawk_core::hotkey::{Action, CancelReason, State, Timings};
    use std::time::Duration;

    const FN_BIT: u64 = flag::FUNCTION;
    const ESC: u16 = keycode::ESCAPE;
    const ARROW_DOWN: u16 = 125;
    const F5: u16 = 96;
    const LETTER_A: u16 = 0;
    const LEFT_SHIFT: u16 = 56;
    const LEFT_CMD: u16 = 55;
    /// The non-coalesced "device independent" bit macOS sets on every
    /// flagsChanged; present so the tests see the flags the tap really sees.
    const NON_COALESCED: u64 = 0x100;

    /// A tap replay: every event at a millisecond offset from `t0`.
    struct Replay {
        t0: Instant,
        m: Machine,
    }

    impl Replay {
        fn new() -> Replay {
            Replay {
                t0: Instant::now(),
                m: Machine::new(Timings::default()),
            }
        }

        fn at(&mut self, ms: u64, event: RawEvent) -> Outcome {
            drive(&mut self.m, event, self.t0 + Duration::from_millis(ms))
        }

        fn acts(&mut self, ms: u64, event: RawEvent) -> Vec<Action> {
            self.at(ms, event).actions
        }

        fn tick(&mut self, ms: u64) -> Vec<Action> {
            self.m
                .handle(Input::Tick, self.t0 + Duration::from_millis(ms))
                .actions
        }
    }

    fn fn_down() -> RawEvent {
        RawEvent::flags_changed(keycode::FN, FN_BIT | NON_COALESCED)
    }

    fn fn_up() -> RawEvent {
        RawEvent::flags_changed(keycode::FN, NON_COALESCED)
    }

    fn key(code: u16) -> RawEvent {
        RawEvent::key_down(code, 0, false)
    }

    #[test]
    fn fn_flags_changed_becomes_fn_down_and_up() {
        assert_eq!(
            translate(&fn_down()),
            Some(Input::FnDown { mods: Mods::NONE })
        );
        assert_eq!(translate(&fn_up()), Some(Input::FnUp));
        let with_shift = RawEvent::flags_changed(keycode::FN, FN_BIT | flag::SHIFT);
        assert_eq!(
            translate(&with_shift),
            Some(Input::FnDown {
                mods: Mods {
                    shift: true,
                    ..Mods::NONE
                }
            })
        );
    }

    #[test]
    fn other_modifiers_are_mods_changed_even_with_the_fn_bit() {
        let shift = RawEvent::flags_changed(LEFT_SHIFT, FN_BIT | flag::SHIFT);
        assert_eq!(
            translate(&shift),
            Some(Input::ModsChanged {
                mods: Mods {
                    shift: true,
                    ..Mods::NONE
                }
            })
        );
    }

    /// An arrow keyDown carries the fn bit whether or not fn is held; that
    /// must not read as an fn press.
    #[test]
    fn the_fn_bit_on_a_key_down_is_ignored() {
        let arrow = RawEvent::key_down(ARROW_DOWN, FN_BIT, false);
        assert_eq!(
            translate(&arrow),
            Some(Input::KeyDown {
                keycode: ARROW_DOWN,
                mods: Mods::NONE,
                repeat: false
            })
        );
        let mut r = Replay::new();
        assert!(r.acts(0, arrow).is_empty());
        assert_eq!(r.m.state(), State::Idle);
    }

    #[test]
    fn hold_is_push_to_talk() {
        let mut r = Replay::new();
        assert_eq!(r.acts(0, fn_down()), [Action::StartRecording]);
        assert!(r.acts(900, fn_up()).contains(&Action::StopAndPaste));
        assert_eq!(r.m.state(), State::Idle);
    }

    #[test]
    fn a_quick_tap_is_discarded_when_the_window_runs_out() {
        let mut r = Replay::new();
        r.acts(0, fn_down());
        assert!(r.acts(90, fn_up()).is_empty());
        let deadline = r.m.deadline().expect("a double-tap window is pending");
        assert_eq!(deadline, r.t0 + Duration::from_millis(90 + 350));
        assert!(r.tick(439).is_empty());
        assert_eq!(r.tick(440), [Action::Cancel(CancelReason::Tap)]);
        assert_eq!(r.m.state(), State::Idle);
    }

    #[test]
    fn double_tap_is_hands_free_until_the_next_press() {
        let mut r = Replay::new();
        r.acts(0, fn_down());
        r.acts(80, fn_up());
        assert_eq!(r.acts(200, fn_down()), [Action::EnterHandsFree]);
        assert!(r.acts(260, fn_up()).is_empty());
        assert_eq!(r.m.state(), State::HandsFree);
        // Typing and ticks while hands-free change nothing.
        assert!(r.acts(3_000, key(LETTER_A)).is_empty());
        assert!(r.tick(4_000).is_empty());
        // One press-and-release stops and pastes.
        assert!(r.acts(30_000, fn_down()).is_empty());
        assert_eq!(r.acts(30_120, fn_up()), [Action::StopAndPaste]);
        assert_eq!(r.m.state(), State::Idle);
    }

    #[test]
    fn fn_with_an_arrow_cancels_and_lets_the_arrow_through() {
        let mut r = Replay::new();
        r.acts(0, fn_down());
        let out = r.at(60, RawEvent::key_down(ARROW_DOWN, FN_BIT, false));
        assert_eq!(out.actions, [Action::Cancel(CancelReason::FnAsModifier)]);
        assert!(!out.swallow);
        assert!(r.acts(200, fn_up()).is_empty());
    }

    #[test]
    fn fn_with_an_f_key_cancels() {
        let mut r = Replay::new();
        r.acts(0, fn_down());
        assert_eq!(
            r.acts(500, RawEvent::key_down(F5, FN_BIT, false)),
            [Action::Cancel(CancelReason::FnAsModifier)]
        );
    }

    #[test]
    fn fn_then_cmd_cancels() {
        let mut r = Replay::new();
        r.acts(0, fn_down());
        let out = r.at(
            150,
            RawEvent::flags_changed(LEFT_CMD, FN_BIT | flag::COMMAND),
        );
        assert_eq!(out.actions, [Action::Cancel(CancelReason::FnAsModifier)]);
        assert!(!out.swallow, "flagsChanged is never swallowed");
    }

    #[test]
    fn cmd_then_fn_does_not_record() {
        let mut r = Replay::new();
        r.acts(0, RawEvent::flags_changed(LEFT_CMD, flag::COMMAND));
        assert!(r
            .acts(
                50,
                RawEvent::flags_changed(keycode::FN, FN_BIT | flag::COMMAND)
            )
            .is_empty());
        assert_eq!(r.m.state(), State::Idle);
    }

    #[test]
    fn escape_cancels_every_recording_state_and_is_swallowed() {
        // While held.
        let mut r = Replay::new();
        r.acts(0, fn_down());
        let out = r.at(400, key(ESC));
        assert_eq!(out.actions, [Action::Cancel(CancelReason::Escape)]);
        assert!(out.swallow);

        // Hands-free.
        let mut r = Replay::new();
        r.acts(0, fn_down());
        r.acts(80, fn_up());
        r.acts(200, fn_down());
        r.acts(260, fn_up());
        let out = r.at(5_000, key(ESC));
        assert_eq!(out.actions, [Action::Cancel(CancelReason::Escape)]);
        assert!(out.swallow);
        assert_eq!(r.m.state(), State::Idle);

        // Idle: esc belongs to the app behind.
        let out = r.at(6_000, key(ESC));
        assert!(out.actions.is_empty());
        assert!(!out.swallow);
    }

    #[test]
    fn global_shortcuts_fire_once_and_swallow_their_repeats() {
        let mut r = Replay::new();
        let ctrl_cmd = flag::CONTROL | flag::COMMAND;
        let out = r.at(0, RawEvent::key_down(keycode::ANSI_V, ctrl_cmd, false));
        assert_eq!(out.actions, [Action::PasteLast]);
        assert!(out.swallow);
        let out = r.at(40, RawEvent::key_down(keycode::ANSI_V, ctrl_cmd, true));
        assert!(out.actions.is_empty());
        assert!(out.swallow);
        assert_eq!(
            r.acts(100, RawEvent::key_down(keycode::ANSI_C, ctrl_cmd, false)),
            [Action::CopyLast]
        );
        assert_eq!(
            r.acts(
                200,
                RawEvent::key_down(keycode::ANSI_M, flag::OPTION, false)
            ),
            [Action::ToggleMeeting]
        );
        // Plain cmd+V (our own synthetic paste among them) passes.
        let out = r.at(
            300,
            RawEvent::key_down(keycode::ANSI_V, flag::COMMAND, false),
        );
        assert!(out.actions.is_empty());
        assert!(!out.swallow);
    }

    #[test]
    fn a_disabled_tap_cancels_a_running_recording() {
        let mut r = Replay::new();
        r.acts(0, fn_down());
        assert_eq!(
            r.acts(700, RawEvent::tap_disabled()),
            [Action::Cancel(CancelReason::Reset)]
        );
        assert_eq!(r.m.state(), State::Idle);
        // The fn-up that arrives after re-enabling is harmless...
        assert!(r.acts(900, fn_up()).is_empty());
        // ...and the next hold works normally.
        assert_eq!(r.acts(2_000, fn_down()), [Action::StartRecording]);
        assert_eq!(r.acts(2_800, fn_up()), [Action::StopAndPaste]);
    }

    #[test]
    fn a_disabled_tap_while_idle_does_nothing() {
        let mut r = Replay::new();
        assert!(r.acts(0, RawEvent::tap_disabled()).is_empty());
        assert_eq!(r.m.state(), State::Idle);
    }

    #[test]
    fn a_disabled_tap_mid_hands_free_cancels_it() {
        let mut r = Replay::new();
        r.acts(0, fn_down());
        r.acts(80, fn_up());
        r.acts(200, fn_down());
        r.acts(260, fn_up());
        assert_eq!(
            r.acts(10_000, RawEvent::tap_disabled()),
            [Action::Cancel(CancelReason::Reset)]
        );
    }
}
