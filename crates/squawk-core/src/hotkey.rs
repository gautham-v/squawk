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
//! - **Triple tap** fn (the third press inside `double_tap` of the second
//!   release): hands-free, and the dictation now goes to the agent. The next
//!   fn press-and-release stops it and sends it. A later third press is the
//!   ordinary stop.
//! - **Single tap** with no second tap within `double_tap`: discarded.
//! - All of the above with **⌥ held as fn goes down** (and no other
//!   modifier): the same gestures, but the dictation goes to the agent (a
//!   Claude Code session in tmux) instead of being pasted, and a single ⌥+fn
//!   tap shows or hides the agent's terminal window. ⌥ may be let go once
//!   fn is down; the target is fixed at the first press.
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

    /// ⌥ alone.
    pub const OPTION: Mods = Mods {
        option: true,
        ..Mods::NONE
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

/// Where a dictation goes when it is finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Target {
    /// Pasted into the focused app (plain fn).
    #[default]
    Paste,
    /// Sent to the agent's tmux session (⌥ held with fn).
    Agent,
}

impl Target {
    /// The target for a first fn press with `mods` held, or `None` when the
    /// press is fn used as a modifier (ctrl+fn, cmd+fn, ⌥⇧+fn…).
    pub fn from_mods(mods: Mods) -> Option<Target> {
        if !mods.any() {
            Some(Target::Paste)
        } else if mods == Mods::OPTION {
            Some(Target::Agent)
        } else {
            None
        }
    }

    /// Modifiers that, joining fn, mean fn is being used as a modifier: any
    /// for a plain dictation; any but ⌥ (which is part of the gesture) for
    /// the agent.
    fn foreign(self, mods: Mods) -> bool {
        match self {
            Target::Paste => mods.any(),
            Target::Agent => mods.shift || mods.control || mods.command,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Target::Paste => "paste",
            Target::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Open the mic and start a dictation bound for `target`.
    StartRecording { target: Target },
    /// The dictation is now hands-free (the menu bar shows the lock).
    EnterHandsFree,
    /// A third quick tap: the hands-free dictation under way now goes to the
    /// agent. The recording carries on; only its target changes.
    RetargetAgent,
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
    /// A single quick ⌥+fn tap: show the agent's terminal window, or hide it
    /// if it is in front. Comes right after the tap's `Cancel(Tap)`.
    ToggleAgentWindow,
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
    /// The third tap of a triple tap is down: hands-free for the agent, and
    /// back to `HandsFree` on release.
    HandsFreeThirdPress,
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
    /// Where the dictation under way goes, fixed at its first fn press.
    target: Target,
    /// ⌥+fn is the agent's. Off (`[agent] enabled = false`), ⌥+fn is fn
    /// used as a modifier, as it was before the agent existed, and a triple
    /// tap is a double tap and a stop.
    agent_enabled: bool,
    /// In `HandsFree` right after a plain double tap: until this instant a
    /// third press sends the dictation to the agent instead of stopping it.
    third_tap_until: Option<Instant>,
}

impl Machine {
    pub fn new(timings: Timings) -> Machine {
        Machine {
            state: State::Idle,
            timings,
            target: Target::Paste,
            agent_enabled: true,
            third_tap_until: None,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// The target of the dictation under way (`Paste` when idle).
    pub fn target(&self) -> Target {
        self.target
    }

    pub fn set_timings(&mut self, timings: Timings) {
        self.timings = timings;
    }

    pub fn set_agent_enabled(&mut self, enabled: bool) {
        self.agent_enabled = enabled;
    }

    /// The target a first fn press with `mods` starts, if it starts one.
    fn start_target(&self, mods: Mods) -> Option<Target> {
        Target::from_mods(mods).filter(|t| *t == Target::Paste || self.agent_enabled)
    }

    /// When the app must feed [`Input::Tick`]: the end of the double-tap
    /// window, or of the triple-tap one. `None` when nothing is pending.
    pub fn deadline(&self) -> Option<Instant> {
        match self.state {
            State::TapReleased { up_at } => Some(up_at + self.timings.double_tap),
            State::HandsFree => self.third_tap_until,
            _ => None,
        }
    }

    /// Back to idle from wherever we are, e.g. after the tap was disabled and
    /// fn events may have been missed, or the engine failed. Returns the
    /// cancel to perform if a recording was running.
    pub fn reset(&mut self) -> Option<Action> {
        let was = self.state;
        self.state = State::Idle;
        self.target = Target::Paste;
        self.third_tap_until = None;
        was.is_recording()
            .then_some(Action::Cancel(CancelReason::Reset))
    }

    /// The app finished a dictation some other way (max length reached, the
    /// user clicked stop): forget it without emitting anything.
    pub fn force_idle(&mut self) {
        self.state = State::Idle;
        self.target = Target::Paste;
        self.third_tap_until = None;
    }

    pub fn handle(&mut self, input: Input, now: Instant) -> Outcome {
        // A pending double-tap window that has run out resolves first, so a
        // late Tick and a late press behave the same.
        let mut pre = Vec::new();
        if let State::TapReleased { up_at } = self.state {
            if now.duration_since(up_at) >= self.timings.double_tap {
                self.state = State::Idle;
                pre.push(Action::Cancel(CancelReason::Tap));
                if self.target == Target::Agent {
                    pre.push(Action::ToggleAgentWindow);
                }
            }
        }
        // The triple-tap window closes by the clock too, silently: the
        // dictation simply stays hands-free for wherever it was going.
        if self.state != State::HandsFree || self.third_tap_until.is_some_and(|t| now >= t) {
            self.third_tap_until = None;
        }
        if self.state == State::Idle {
            self.target = Target::Paste;
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
        let target = self.target;
        let foreign = |mods: Mods| target.foreign(mods);
        match (self.state, input) {
            (_, Input::Tick) => Outcome::none(),

            // ---- idle
            (Idle, Input::FnDown { mods }) => match self.start_target(mods) {
                Some(target) => {
                    self.target = target;
                    self.state = Pressed { down_at: now };
                    Outcome::act(Action::StartRecording { target })
                }
                None => Outcome::none(),
            },
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
            (Pressed { .. }, Input::ModsChanged { mods }) if foreign(mods) => {
                self.state = Idle;
                Outcome::act(Action::Cancel(CancelReason::FnAsModifier))
            }
            (Pressed { .. }, _) => Outcome::none(),

            // ---- between the taps
            (TapReleased { .. }, Input::FnDown { mods }) if !foreign(mods) => {
                self.state = SecondPress;
                Outcome::act(Action::EnterHandsFree)
            }
            (TapReleased { .. }, Input::KeyDown { keycode, .. }) if is_escape(keycode) => {
                self.state = Idle;
                Outcome::act(Action::Cancel(CancelReason::Escape)).swallowed()
            }
            (TapReleased { .. }, Input::FnDown { mods }) => {
                // fn with other modifiers right after a tap: the tap was
                // nothing. A press that starts a dictation of its own (⌥+fn
                // after a plain tap) starts it.
                self.state = Idle;
                let mut out = Outcome::act(Action::Cancel(CancelReason::Tap));
                if let Some(target) = self.start_target(mods) {
                    self.target = target;
                    self.state = Pressed { down_at: now };
                    out.actions.push(Action::StartRecording { target });
                }
                out
            }
            (TapReleased { .. }, Input::KeyDown { .. }) => {
                // Typing right after a tap: the tap was nothing.
                self.state = Idle;
                Outcome::act(Action::Cancel(CancelReason::Tap))
            }
            (TapReleased { .. }, _) => Outcome::none(),

            // ---- second press held
            (SecondPress, Input::FnUp) => {
                self.state = HandsFree;
                // A plain double tap may still become a triple tap.
                if self.target == Target::Paste && self.agent_enabled {
                    self.third_tap_until = Some(now + self.timings.double_tap);
                }
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
            (SecondPress, Input::ModsChanged { mods }) if foreign(mods) => {
                self.state = Idle;
                Outcome::act(Action::Cancel(CancelReason::FnAsModifier))
            }
            (SecondPress, _) => Outcome::none(),

            // ---- hands-free
            (HandsFree, Input::FnDown { mods }) if !foreign(mods) => {
                if self.third_tap_until.take().is_some() {
                    // The third tap of a triple tap: keep going, for the agent.
                    self.target = Target::Agent;
                    self.state = HandsFreeThirdPress;
                    Outcome::act(Action::RetargetAgent)
                } else {
                    self.state = HandsFreeFnDown;
                    Outcome::none()
                }
            }
            (HandsFree | HandsFreeFnDown | HandsFreeThirdPress, Input::KeyDown { keycode, .. })
                if is_escape(keycode) =>
            {
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
            (HandsFreeFnDown, Input::ModsChanged { mods }) if foreign(mods) => {
                self.state = HandsFree;
                Outcome::none()
            }
            (HandsFreeFnDown, _) => Outcome::none(),

            // ---- the third tap of a triple tap held
            (HandsFreeThirdPress, Input::FnUp | Input::KeyDown { .. }) => {
                self.state = HandsFree;
                Outcome::none()
            }
            (HandsFreeThirdPress, Input::ModsChanged { mods }) if foreign(mods) => {
                self.state = HandsFree;
                Outcome::none()
            }
            (HandsFreeThirdPress, _) => Outcome::none(),
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
    /// fn going down with ⌥ already held.
    const OPT_DOWN: Input = Input::FnDown { mods: Mods::OPTION };
    const START: Action = Action::StartRecording {
        target: Target::Paste,
    };
    const START_AGENT: Action = Action::StartRecording {
        target: Target::Agent,
    };

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
        assert_eq!(c.acts(0, DOWN), [START]);
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
        assert_eq!(c.acts(0, DOWN), [START]);
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
            [Action::Cancel(CancelReason::Tap), START]
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
    fn option_fn_hold_records_for_the_agent() {
        let mut c = Clock::new();
        assert_eq!(c.acts(0, OPT_DOWN), [START_AGENT]);
        assert_eq!(c.m.target(), Target::Agent);
        // Letting go of ⌥ first changes nothing.
        assert!(c
            .acts(400, Input::ModsChanged { mods: Mods::NONE })
            .is_empty());
        assert_eq!(c.acts(1500, UP), [Action::StopAndPaste]);
        assert_eq!(c.m.state(), State::Idle);
        // The next plain hold pastes again.
        assert_eq!(c.acts(3000, DOWN), [START]);
        assert_eq!(c.m.target(), Target::Paste);
        assert_eq!(c.acts(4000, UP), [Action::StopAndPaste]);
    }

    #[test]
    fn option_fn_tap_toggles_the_agent_window() {
        let mut c = Clock::new();
        assert_eq!(c.acts(0, OPT_DOWN), [START_AGENT]);
        assert!(c.acts(110, UP).is_empty());
        assert!(c
            .acts(110 + DEFAULT_DOUBLE_TAP_MS - 1, Input::Tick)
            .is_empty());
        assert_eq!(
            c.acts(110 + DEFAULT_DOUBLE_TAP_MS, Input::Tick),
            [Action::Cancel(CancelReason::Tap), Action::ToggleAgentWindow]
        );
        assert_eq!(c.m.state(), State::Idle);
        // A plain tap afterwards is still just discarded.
        c.acts(2000, DOWN);
        c.acts(2100, UP);
        assert_eq!(
            c.acts(2100 + DEFAULT_DOUBLE_TAP_MS, Input::Tick),
            [Action::Cancel(CancelReason::Tap)]
        );
    }

    #[test]
    fn option_fn_tap_then_typing_is_nothing() {
        let mut c = Clock::new();
        c.acts(0, OPT_DOWN);
        c.acts(90, UP);
        assert_eq!(c.acts(150, key(0)), [Action::Cancel(CancelReason::Tap)]);
        assert!(c.acts(2000, Input::Tick).is_empty());
    }

    #[test]
    fn option_fn_double_tap_is_hands_free_for_the_agent() {
        let mut c = Clock::new();
        assert_eq!(c.acts(0, OPT_DOWN), [START_AGENT]);
        c.acts(100, UP);
        // The second press with or without ⌥.
        assert_eq!(c.acts(250, DOWN), [Action::EnterHandsFree]);
        assert!(c.acts(330, UP).is_empty());
        assert_eq!(c.m.state(), State::HandsFree);
        assert_eq!(c.m.target(), Target::Agent);
        // Stopping with ⌥ still held works too.
        assert!(c.acts(9000, OPT_DOWN).is_empty());
        assert_eq!(c.acts(9100, UP), [Action::StopAndPaste]);
        assert_eq!(c.m.state(), State::Idle);

        let mut c = Clock::new();
        c.acts(0, OPT_DOWN);
        c.acts(100, UP);
        assert_eq!(c.acts(250, OPT_DOWN), [Action::EnterHandsFree]);
        assert!(c.acts(330, UP).is_empty());
        assert_eq!(c.m.state(), State::HandsFree);
    }

    #[test]
    fn option_with_another_modifier_is_not_the_agent() {
        let mut c = Clock::new();
        let opt_shift = Mods {
            option: true,
            shift: true,
            ..Mods::NONE
        };
        assert!(c.acts(0, Input::FnDown { mods: opt_shift }).is_empty());
        assert!(c.acts(600, UP).is_empty());
        assert_eq!(c.m.state(), State::Idle);
        // cmd joining an agent hold is fn as a modifier: cancel.
        c.acts(1000, OPT_DOWN);
        let opt_cmd = Mods {
            option: true,
            command: true,
            ..Mods::NONE
        };
        assert_eq!(
            c.acts(1100, Input::ModsChanged { mods: opt_cmd }),
            [Action::Cancel(CancelReason::FnAsModifier)]
        );
        // And fn+⌥+arrow too.
        c.acts(2000, OPT_DOWN);
        assert_eq!(
            c.acts(2050, key_with(ARROW_LEFT, Mods::OPTION)),
            [Action::Cancel(CancelReason::FnAsModifier)]
        );
    }

    #[test]
    fn option_pressed_during_a_plain_hold_does_not_switch_to_the_agent() {
        let mut c = Clock::new();
        assert_eq!(c.acts(0, DOWN), [START]);
        assert_eq!(
            c.acts(200, Input::ModsChanged { mods: Mods::OPTION }),
            [Action::Cancel(CancelReason::FnAsModifier)]
        );
        // Hands-free: ⌥ and the stop press do not change the target.
        let mut c = Clock::new();
        c.acts(0, DOWN);
        c.acts(100, UP);
        c.acts(200, DOWN);
        c.acts(280, UP);
        assert!(c
            .acts(3000, Input::ModsChanged { mods: Mods::OPTION })
            .is_empty());
        assert_eq!(c.m.target(), Target::Paste);
    }

    #[test]
    fn option_fn_right_after_a_plain_tap_starts_an_agent_dictation() {
        let mut c = Clock::new();
        c.acts(0, DOWN);
        c.acts(90, UP);
        assert_eq!(
            c.acts(200, OPT_DOWN),
            [Action::Cancel(CancelReason::Tap), START_AGENT]
        );
        assert_eq!(c.m.target(), Target::Agent);
        assert_eq!(c.acts(1500, UP), [Action::StopAndPaste]);
    }

    #[test]
    fn option_fn_is_nothing_with_the_agent_off() {
        let mut c = Clock::new();
        c.m.set_agent_enabled(false);
        assert!(c.acts(0, OPT_DOWN).is_empty());
        assert!(c.acts(90, UP).is_empty());
        assert!(c.acts(2000, Input::Tick).is_empty());
        assert_eq!(c.m.state(), State::Idle);
        // A plain tap then ⌥+fn: just the tap's cancel.
        c.acts(3000, DOWN);
        c.acts(3090, UP);
        assert_eq!(c.acts(3200, OPT_DOWN), [Action::Cancel(CancelReason::Tap)]);
        assert_eq!(c.m.state(), State::Idle);
        // Plain fn is untouched.
        assert_eq!(c.acts(5000, DOWN), [START]);
    }

    #[test]
    fn triple_tap_is_hands_free_for_the_agent() {
        let mut c = Clock::new();
        assert_eq!(c.acts(0, DOWN), [START]);
        c.acts(100, UP);
        assert_eq!(c.acts(220, DOWN), [Action::EnterHandsFree]);
        assert!(c.acts(300, UP).is_empty());
        assert_eq!(c.m.state(), State::HandsFree);
        assert_eq!(
            c.m.deadline(),
            Some(c.t0 + Duration::from_millis(300 + DEFAULT_DOUBLE_TAP_MS))
        );
        // The third press, just inside the window.
        assert_eq!(
            c.acts(300 + DEFAULT_DOUBLE_TAP_MS - 1, DOWN),
            [Action::RetargetAgent]
        );
        assert_eq!(c.m.state(), State::HandsFreeThirdPress);
        assert_eq!(c.m.target(), Target::Agent);
        assert!(c.acts(800, UP).is_empty());
        assert_eq!(c.m.state(), State::HandsFree);
        assert!(c.m.deadline().is_none());
        // Talking, a tick, then one press-and-release sends it.
        assert!(c.acts(5000, Input::Tick).is_empty());
        assert!(c.acts(20_000, DOWN).is_empty());
        assert_eq!(c.m.target(), Target::Agent);
        assert_eq!(c.acts(20_100, UP), [Action::StopAndPaste]);
        assert_eq!(c.m.state(), State::Idle);
        // The next plain hold pastes again.
        assert_eq!(c.acts(30_000, DOWN), [START]);
        assert_eq!(c.m.target(), Target::Paste);
    }

    #[test]
    fn a_late_third_tap_stops_and_pastes() {
        // At the end of the window, with the tick on time.
        let mut c = Clock::new();
        c.acts(0, DOWN);
        c.acts(100, UP);
        c.acts(220, DOWN);
        c.acts(300, UP);
        assert!(c.acts(300 + DEFAULT_DOUBLE_TAP_MS, Input::Tick).is_empty());
        assert!(c.m.deadline().is_none());
        assert!(c.acts(300 + DEFAULT_DOUBLE_TAP_MS, DOWN).is_empty());
        assert_eq!(c.m.state(), State::HandsFreeFnDown);
        assert_eq!(c.acts(800, UP), [Action::StopAndPaste]);
        assert_eq!(c.m.target(), Target::Paste);
        // No tick at all (timer late): the clock decides, not the order.
        let mut c = Clock::new();
        c.acts(0, DOWN);
        c.acts(100, UP);
        c.acts(220, DOWN);
        c.acts(300, UP);
        assert!(c.acts(300 + DEFAULT_DOUBLE_TAP_MS + 50, DOWN).is_empty());
        assert_eq!(c.acts(900, UP), [Action::StopAndPaste]);
        assert_eq!(c.m.target(), Target::Paste);
    }

    #[test]
    fn triple_tap_with_the_agent_off_stops_and_pastes() {
        let mut c = Clock::new();
        c.m.set_agent_enabled(false);
        c.acts(0, DOWN);
        c.acts(100, UP);
        assert_eq!(c.acts(220, DOWN), [Action::EnterHandsFree]);
        assert!(c.acts(300, UP).is_empty());
        assert!(c.m.deadline().is_none());
        assert!(c.acts(400, DOWN).is_empty());
        assert_eq!(c.acts(480, UP), [Action::StopAndPaste]);
        assert_eq!(c.m.target(), Target::Paste);
    }

    #[test]
    fn option_fn_triple_tap_is_still_a_stop() {
        // Already the agent's: the third tap stops it, as before.
        let mut c = Clock::new();
        c.acts(0, OPT_DOWN);
        c.acts(100, UP);
        assert_eq!(c.acts(220, DOWN), [Action::EnterHandsFree]);
        c.acts(300, UP);
        assert!(c.m.deadline().is_none());
        assert!(c.acts(400, DOWN).is_empty());
        assert_eq!(c.acts(480, UP), [Action::StopAndPaste]);
    }

    #[test]
    fn escape_cancels_a_triple_tap() {
        let mut c = Clock::new();
        c.acts(0, DOWN);
        c.acts(100, UP);
        c.acts(220, DOWN);
        c.acts(300, UP);
        c.acts(400, DOWN);
        let out = c.at(450, key(keycode::ESCAPE));
        assert_eq!(out.actions, [Action::Cancel(CancelReason::Escape)]);
        assert!(out.swallow);
        assert_eq!(c.m.state(), State::Idle);
        // And once it is hands-free for the agent.
        let mut c = Clock::new();
        c.acts(0, DOWN);
        c.acts(100, UP);
        c.acts(220, DOWN);
        c.acts(300, UP);
        c.acts(400, DOWN);
        c.acts(480, UP);
        let out = c.at(9000, key(keycode::ESCAPE));
        assert_eq!(out.actions, [Action::Cancel(CancelReason::Escape)]);
        assert_eq!(c.m.state(), State::Idle);
    }

    #[test]
    fn typing_inside_the_triple_tap_window_keeps_it_open() {
        let mut c = Clock::new();
        c.acts(0, DOWN);
        c.acts(100, UP);
        c.acts(220, DOWN);
        c.acts(300, UP);
        assert!(c.acts(350, key(0)).is_empty());
        assert_eq!(c.acts(500, DOWN), [Action::RetargetAgent]);
        // fn+arrow on the third press: still the agent's, still recording.
        assert!(c.acts(550, key(ARROW_LEFT)).is_empty());
        assert_eq!(c.m.state(), State::HandsFree);
        assert_eq!(c.m.target(), Target::Agent);
        c.acts(3000, UP);
        c.acts(5000, DOWN);
        assert_eq!(c.acts(5100, UP), [Action::StopAndPaste]);
    }

    #[test]
    fn targets_from_modifiers() {
        assert_eq!(Target::from_mods(Mods::NONE), Some(Target::Paste));
        assert_eq!(Target::from_mods(Mods::OPTION), Some(Target::Agent));
        let ctrl = Mods {
            control: true,
            ..Mods::NONE
        };
        assert_eq!(Target::from_mods(ctrl), None);
    }

    #[test]
    fn mods_from_flags() {
        let m = Mods::from_flags(flag::CONTROL | flag::COMMAND | flag::FUNCTION);
        assert!(m.control && m.command && !m.option && !m.shift);
        assert!(!Mods::from_flags(flag::FUNCTION).any());
    }
}
