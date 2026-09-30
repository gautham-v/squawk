//! The `NSStatusItem` and the bridge from its clicks into gpui. Port of
//! claudebar's `status_item.rs` (same target/action class, same
//! global-monitor `ClickedOutside`, same `Anchor` maths), with squawk's
//! [`MenuBarState`].

use std::time::Instant;

use futures::channel::mpsc::UnboundedReceiver;
use objc2::MainThreadMarker;

use crate::controller::{DictationPhase, Snapshot};

/// What the menu bar item shows. Precedence, highest first: a dictation
/// (recording, then transcribing), a meeting, needs-attention, idle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuBarState {
    /// Monochrome template glyph (five vertical bars).
    Idle,
    /// Model missing/downloading/failed, or a permission missing: the glyph
    /// at AppKit's disabled (dimmed) rendering. The popover says why.
    NeedsAttention,
    /// Copper glyph + "0:07". `hands_free` adds a small lock mark.
    Recording { elapsed_secs: u64, hands_free: bool },
    /// Dimmed glyph, briefly, between release and paste.
    Transcribing,
    /// "● 12:04" in copper.
    Meeting { elapsed_secs: u64 },
}

impl MenuBarState {
    /// Pure mapping from a snapshot; `now` is passed in so the 1 s timer that
    /// redraws the elapsed time and tests agree.
    pub fn from_snapshot(snap: &Snapshot, now: Instant) -> MenuBarState {
        match &snap.dictation {
            DictationPhase::Recording { since, hands_free } => MenuBarState::Recording {
                elapsed_secs: now.saturating_duration_since(*since).as_secs(),
                hands_free: *hands_free,
            },
            DictationPhase::Transcribing => MenuBarState::Transcribing,
            DictationPhase::Idle => match &snap.meeting {
                Some(m) => MenuBarState::Meeting {
                    elapsed_secs: now.saturating_duration_since(m.since).as_secs(),
                },
                None if !snap.model.is_ready() || !snap.permissions.can_dictate() => {
                    MenuBarState::NeedsAttention
                }
                None => MenuBarState::Idle,
            },
        }
    }

    /// Whether the menu bar needs a redraw every second.
    pub fn ticks(&self) -> bool {
        matches!(
            self,
            MenuBarState::Recording { .. } | MenuBarState::Meeting { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusItemEvent {
    Clicked,
    ClickedOutside,
}

/// Where the item is, in gpui screen coordinates (top-left origin).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub item: ScreenRect,
    pub screen: ScreenRect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

pub struct StatusItem {
    _private: (),
}

impl StatusItem {
    pub fn new(
        mtm: MainThreadMarker,
        state: MenuBarState,
    ) -> (StatusItem, UnboundedReceiver<StatusItemEvent>) {
        let _ = (mtm, state);
        todo!("app agent: port claudebar status_item.rs")
    }

    pub fn set_state(&self, mtm: MainThreadMarker, state: MenuBarState) {
        let _ = (mtm, state);
        todo!("app agent")
    }

    pub fn anchor(&self, mtm: MainThreadMarker) -> Option<Anchor> {
        let _ = mtm;
        todo!("app agent")
    }
}

pub fn set_accessory_activation_policy(mtm: MainThreadMarker) {
    let _ = mtm;
    todo!("app agent")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::MeetingSnap;
    use squawk_core::status::Permissions;
    use squawk_core::ModelStatus;
    use std::time::Duration;

    fn snap() -> Snapshot {
        Snapshot {
            dictation: DictationPhase::Idle,
            meeting: None,
            model: ModelStatus::Ready,
            permissions: Permissions::default(),
            config_note: None,
            last_error: None,
            dictations_this_run: 0,
        }
    }

    #[test]
    fn precedence() {
        let now = Instant::now();
        let t0 = now - Duration::from_secs(7);
        assert_eq!(
            MenuBarState::from_snapshot(&snap(), now),
            MenuBarState::Idle
        );

        let mut s = snap();
        s.model = ModelStatus::Missing;
        assert_eq!(
            MenuBarState::from_snapshot(&s, now),
            MenuBarState::NeedsAttention
        );

        s.meeting = Some(MeetingSnap {
            title: "M".into(),
            path: "/m.md".into(),
            since: now - Duration::from_secs(724),
        });
        assert_eq!(
            MenuBarState::from_snapshot(&s, now),
            MenuBarState::Meeting { elapsed_secs: 724 }
        );

        s.dictation = DictationPhase::Recording {
            since: t0,
            hands_free: true,
        };
        assert_eq!(
            MenuBarState::from_snapshot(&s, now),
            MenuBarState::Recording {
                elapsed_secs: 7,
                hands_free: true
            }
        );

        s.dictation = DictationPhase::Transcribing;
        assert_eq!(
            MenuBarState::from_snapshot(&s, now),
            MenuBarState::Transcribing
        );
    }
}
