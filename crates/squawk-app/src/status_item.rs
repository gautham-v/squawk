//! The `NSStatusItem` and the bridge from its clicks into gpui. Port of
//! claudebar's `status_item.rs` (same target/action class, same
//! global-monitor `ClickedOutside`, same `Anchor` maths), with squawk's
//! [`MenuBarState`].
//!
//! The bridge is deliberately dumb: the button's target pushes a
//! [`StatusItemEvent`] into an unbounded channel that `main.rs` drains from a
//! gpui task, so nothing AppKit-shaped leaks into the views.

use std::cell::RefCell;
use std::time::Instant;

use block2::RcBlock;
use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker};
use objc2_app_kit::{
    NSAppearanceCustomization, NSApplication, NSApplicationActivationPolicy, NSCellImagePosition,
    NSControl, NSEvent, NSEventMask, NSScreen, NSStatusBar, NSStatusBarButton, NSStatusItem,
    NSVariableStatusItemLength, NSView,
};
use objc2_foundation::{NSPoint, NSRect};

use crate::controller::{DictationPhase, Snapshot};
use crate::menu_bar_icon;

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

    /// Drawn at AppKit's disabled (dimmed) rendering.
    pub fn dimmed(&self) -> bool {
        matches!(
            self,
            MenuBarState::NeedsAttention | MenuBarState::Transcribing
        )
    }

    /// Whether the menu bar needs a redraw every second.
    pub fn ticks(&self) -> bool {
        matches!(
            self,
            MenuBarState::Recording { .. } | MenuBarState::Meeting { .. }
        )
    }
}

/// What the status item tells the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusItemEvent {
    /// The user clicked the menu bar item.
    Clicked,
    /// The user clicked somewhere outside this app; the popover should
    /// dismiss. A non-activating panel never makes squawk the active app, so
    /// resign-key is not a reliable signal — a global monitor is, and it
    /// never sees our own clicks.
    ClickedOutside,
}

/// Where the item is, in gpui screen coordinates (top-left origin).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub item: ScreenRect,
    pub screen: ScreenRect,
}

/// A rectangle in gpui's screen coordinate space (top-left origin, y down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

struct TargetIvars {
    tx: UnboundedSender<StatusItemEvent>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SquawkStatusItemTarget"]
    #[ivars = TargetIvars]
    struct StatusItemTarget;

    unsafe impl NSObjectProtocol for StatusItemTarget {}

    impl StatusItemTarget {
        #[unsafe(method(squawkStatusItemClicked:))]
        fn clicked(&self, _sender: *mut AnyObject) {
            // Fails only once the receiver is gone (shutting down).
            let _ = self.ivars().tx.unbounded_send(StatusItemEvent::Clicked);
        }
    }
);

impl StatusItemTarget {
    fn new(tx: UnboundedSender<StatusItemEvent>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(TargetIvars { tx });
        unsafe { msg_send![super(this), init] }
    }
}

/// Owns the menu bar item for the lifetime of the app. Dropping it removes
/// the item, so keep it alive.
pub struct StatusItem {
    item: Retained<NSStatusItem>,
    // Held so the target outlives the button's unretained `target` pointer.
    _target: Retained<StatusItemTarget>,
    outside_monitor: Option<Retained<AnyObject>>,
    /// What is drawn now, so the 1 s redraw only rebuilds on a change.
    shown: RefCell<Option<(MenuBarState, bool)>>,
}

impl StatusItem {
    pub fn new(
        mtm: MainThreadMarker,
        state: MenuBarState,
    ) -> (StatusItem, UnboundedReceiver<StatusItemEvent>) {
        let (tx, rx) = mpsc::unbounded();
        let tx_outside = tx.clone();
        let target = StatusItemTarget::new(tx);

        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = item.button(mtm) {
            unsafe {
                button.setImagePosition(NSCellImagePosition::ImageOnly);
                let control: &NSControl = &button;
                control.setTarget(Some(&*target));
                control.setAction(Some(sel!(squawkStatusItemClicked:)));
            }
        }
        let this = StatusItem {
            item,
            _target: target,
            outside_monitor: install_outside_click_monitor(tx_outside),
            shown: RefCell::new(None),
        };
        this.set_state(mtm, state);
        (this, rx)
    }

    /// Redraw for a new state. Cheap to call often: nothing is rebuilt
    /// unless the state (or the menu bar's appearance) changed.
    pub fn set_state(&self, mtm: MainThreadMarker, state: MenuBarState) {
        let Some(button) = self.item.button(mtm) else {
            return;
        };
        let dark = is_dark(&button);
        let key = (state, dark);
        if self.shown.borrow().as_ref() == Some(&key) {
            return;
        }
        let (image, _) = menu_bar_icon::item_image(&key.0, dark);
        button.setImage(Some(&image));
        button.setAppearsDisabled(key.0.dimmed());
        *self.shown.borrow_mut() = Some(key);
    }

    /// Where the item is and which display it is on, in gpui screen
    /// coordinates. AppKit's rects are bottom-left-origin, so both are
    /// flipped through the primary screen's height; the display comes along
    /// because the menu bar moves to whichever display has attention.
    pub fn anchor(&self, mtm: MainThreadMarker) -> Option<Anchor> {
        let button = self.item.button(mtm)?;
        let window = button.window()?;
        let frame: NSRect = window.frame();
        let flip_height = primary_screen_height(mtm)?;
        let screen = screen_containing(mtm, frame)?;
        Some(Anchor {
            item: flipped(frame, flip_height),
            screen: flipped(screen, flip_height),
        })
    }
}

impl Drop for StatusItem {
    fn drop(&mut self) {
        if let Some(monitor) = self.outside_monitor.take() {
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
    }
}

/// Whether the menu bar is drawing on a dark background, from the button's
/// own appearance: the menu bar over a pale wallpaper is light even in dark
/// mode.
fn is_dark(button: &NSStatusBarButton) -> bool {
    let view: &NSView = button;
    view.effectiveAppearance()
        .name()
        .to_string()
        .contains("Dark")
}

fn flipped(frame: NSRect, flip_height: f64) -> ScreenRect {
    ScreenRect {
        x: frame.origin.x as f32,
        y: (flip_height - (frame.origin.y + frame.size.height)) as f32,
        width: frame.size.width as f32,
        height: frame.size.height as f32,
    }
}

/// The frame of the screen `rect` sits on, by its midpoint; falls back to
/// the primary screen.
fn screen_containing(mtm: MainThreadMarker, rect: NSRect) -> Option<NSRect> {
    let mid = NSPoint::new(
        rect.origin.x + rect.size.width / 2.0,
        rect.origin.y + rect.size.height / 2.0,
    );
    let mut primary: Option<NSRect> = None;
    for screen in NSScreen::screens(mtm).iter() {
        let frame = screen.frame();
        if primary.is_none() || frame.origin == NSPoint::new(0.0, 0.0) {
            primary = Some(frame);
        }
        let inside = mid.x >= frame.origin.x
            && mid.x <= frame.origin.x + frame.size.width
            && mid.y >= frame.origin.y
            && mid.y <= frame.origin.y + frame.size.height;
        if inside {
            return Some(frame);
        }
    }
    primary
}

fn primary_screen_height(mtm: MainThreadMarker) -> Option<f64> {
    let mut fallback: Option<f64> = None;
    for screen in NSScreen::screens(mtm).iter() {
        let frame = screen.frame();
        if fallback.is_none() {
            fallback = Some(frame.size.height);
        }
        if frame.origin == NSPoint::new(0.0, 0.0) {
            return Some(frame.size.height);
        }
    }
    fallback
}

/// Run as a menu bar accessory: no Dock icon, no menus, never the active
/// app.
pub fn set_accessory_activation_policy(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
}

/// Watch for mouse-downs in other applications so the popover can dismiss.
fn install_outside_click_monitor(
    tx: UnboundedSender<StatusItemEvent>,
) -> Option<Retained<AnyObject>> {
    let mask =
        NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown;
    let handler = RcBlock::new(move |_event: core::ptr::NonNull<NSEvent>| {
        let _ = tx.unbounded_send(StatusItemEvent::ClickedOutside);
    });
    let monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &handler);
    if monitor.is_none() {
        log::warn!("global mouse monitor unavailable; outside clicks will not dismiss");
    }
    monitor
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::MeetingSnap;
    use squawk_core::status::Permissions;
    use squawk_core::ModelStatus;
    use std::time::Duration;

    fn snap() -> Snapshot {
        let mut s = Snapshot::initial(
            crate::controller::tests::test_paths(),
            ModelStatus::Ready,
            None,
        );
        s.permissions = Permissions::default();
        s
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
            started_at: chrono::Local::now(),
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
