//! The `NSStatusItem` and the bridge from its clicks into gpui. Port of
//! claudebar's `status_item.rs` (same target/action class, same
//! global-monitor `ClickedOutside`, same `Anchor` maths), with squawk's
//! [`MenuBarState`] and the animated glyph from `menu_bar_icon.rs`.
//!
//! The bridge is deliberately dumb: the button's target pushes a
//! [`StatusItemEvent`] into an unbounded channel that `main.rs` drains from a
//! gpui task, so nothing AppKit-shaped leaks into the views.
//!
//! The glyph animates on a main-run-loop `NSTimer` that exists only while
//! the state moves (recording, the settle after it, a meeting) and is
//! invalidated the moment it stops, so idle costs no wakeups at all.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use block2::RcBlock;
use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSCellImagePosition, NSControl, NSEvent,
    NSEventMask, NSScreen, NSStatusBar, NSStatusItem, NSVariableStatusItemLength, NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSTimer};

use crate::controller::{DictationPhase, Snapshot};
use crate::menu_bar_icon::{self, Animator, Glyph};

/// What the menu bar item shows. Precedence, highest first: a dictation
/// (recording, then transcribing), a meeting, needs-attention, idle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuBarState {
    /// The five bars at rest. No timer.
    Idle,
    /// Model missing/downloading/failed, or a permission missing: the
    /// resting bars, faint. The popover says why.
    NeedsAttention,
    /// Push-to-talk or hands-free (drawn the same): the bars follow the mic.
    Recording,
    /// Between release and paste: the bars ease back to rest.
    Transcribing,
    /// A meeting is recording: a slow, small pulse.
    Meeting,
}

impl MenuBarState {
    /// Pure mapping from a snapshot.
    pub fn from_snapshot(snap: &Snapshot) -> MenuBarState {
        match &snap.dictation {
            DictationPhase::Recording { .. } => MenuBarState::Recording,
            DictationPhase::Transcribing => MenuBarState::Transcribing,
            DictationPhase::Idle => match &snap.meeting {
                Some(_) => MenuBarState::Meeting,
                None if !snap.model.is_ready() || !snap.permissions.can_dictate() => {
                    MenuBarState::NeedsAttention
                }
                None => MenuBarState::Idle,
            },
        }
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
            mark_click();
            // Fails only once the receiver is gone (shutting down).
            let _ = self.ivars().tx.unbounded_send(StatusItemEvent::Clicked);
        }
    }
);

/// When the item was last clicked, for the open-latency debug log.
static CLICKED_AT: Mutex<Option<Instant>> = Mutex::new(None);

fn mark_click() {
    if let Ok(mut at) = CLICKED_AT.lock() {
        *at = Some(Instant::now());
    }
}

/// How long ago the item was last clicked (zero if never).
pub fn since_click() -> Duration {
    CLICKED_AT
        .lock()
        .ok()
        .and_then(|at| *at)
        .map_or(Duration::ZERO, |at| at.elapsed())
}

impl StatusItemTarget {
    fn new(tx: UnboundedSender<StatusItemEvent>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(TargetIvars { tx });
        unsafe { msg_send![super(this), init] }
    }
}

/// Owns the menu bar item for the lifetime of the app. Dropping it removes
/// the item, so keep it alive.
pub struct StatusItem {
    glyph: Rc<GlyphDriver>,
    // Held so the target outlives the button's unretained `target` pointer.
    _target: Retained<StatusItemTarget>,
    outside_monitor: Option<Retained<AnyObject>>,
}

/// The item's image and the timer that animates it.
struct GlyphDriver {
    mtm: MainThreadMarker,
    item: Retained<NSStatusItem>,
    anim: RefCell<Animator>,
    /// The mic's latest RMS (0..1).
    level: Box<dyn Fn() -> f32>,
    clock: Instant,
    /// What is drawn now, so an unchanged frame is not redrawn.
    shown: Cell<Option<Glyph>>,
    /// The running timer and its rate.
    timer: RefCell<Option<(Retained<NSTimer>, f64)>>,
}

impl StatusItem {
    /// `level` returns the live dictation's mic RMS; it is polled only while
    /// recording.
    pub fn new(
        mtm: MainThreadMarker,
        state: MenuBarState,
        level: impl Fn() -> f32 + 'static,
    ) -> (StatusItem, UnboundedReceiver<StatusItemEvent>) {
        let (tx, rx) = mpsc::unbounded();
        let tx_outside = tx.clone();
        let target = StatusItemTarget::new(tx);

        let bar = NSStatusBar::systemStatusBar();
        // Variable length, but the image is always GLYPH_WIDTH wide, so the
        // item never changes width.
        let item = bar.statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = item.button(mtm) {
            unsafe {
                button.setImagePosition(NSCellImagePosition::ImageOnly);
                let control: &NSControl = &button;
                control.setTarget(Some(&*target));
                control.setAction(Some(sel!(squawkStatusItemClicked:)));
            }
        }
        let glyph = Rc::new(GlyphDriver {
            mtm,
            item,
            anim: RefCell::new(Animator::new(state.clone(), 0.0, reduce_motion())),
            level: Box::new(level),
            clock: Instant::now(),
            shown: Cell::new(None),
            timer: RefCell::new(None),
        });
        let this = StatusItem {
            glyph,
            _target: target,
            outside_monitor: install_outside_click_monitor(tx_outside),
        };
        this.set_state(mtm, state);
        (this, rx)
    }

    /// Switch to a new state. Cheap to call often: an unchanged state does
    /// nothing, and the timer runs only while the state animates.
    pub fn set_state(&self, _mtm: MainThreadMarker, state: MenuBarState) {
        let now = self.glyph.now();
        self.glyph
            .anim
            .borrow_mut()
            .set_state(state, now, reduce_motion());
        GlyphDriver::tick(&self.glyph, now);
    }

    /// Where the item is and which display it is on, in gpui screen
    /// coordinates. AppKit's rects are bottom-left-origin, so both are
    /// flipped through the primary screen's height; the display comes along
    /// because the menu bar moves to whichever display has attention.
    pub fn anchor(&self, mtm: MainThreadMarker) -> Option<Anchor> {
        let button = self.glyph.item.button(mtm)?;
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

impl GlyphDriver {
    fn now(&self) -> f64 {
        self.clock.elapsed().as_secs_f64()
    }

    /// Draw the frame for `now`, then start, change or stop the timer to
    /// suit how the glyph moves from here.
    fn tick(this: &Rc<GlyphDriver>, now: f64) {
        let (glyph, fps) = {
            let mut anim = this.anim.borrow_mut();
            let glyph = anim.frame(now, (this.level)());
            (glyph, anim.motion(now).fps())
        };
        this.draw(glyph);
        let running = this.timer.borrow().as_ref().map(|(_, f)| *f);
        if running != fps {
            this.stop_timer();
            if let Some(fps) = fps {
                GlyphDriver::start_timer(this, fps);
            }
        }
    }

    fn draw(&self, glyph: Glyph) {
        if self.shown.get() == Some(glyph) {
            return;
        }
        let Some(button) = self.item.button(self.mtm) else {
            return;
        };
        button.setImage(Some(&menu_bar_icon::glyph_image(glyph)));
        self.shown.set(Some(glyph));
    }

    fn start_timer(this: &Rc<GlyphDriver>, fps: f64) {
        let weak: Weak<GlyphDriver> = Rc::downgrade(this);
        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            if let Some(this) = weak.upgrade() {
                let now = this.now();
                GlyphDriver::tick(&this, now);
            }
        });
        let interval = 1.0 / fps;
        // SAFETY: the timer goes on the main run loop only, so the block
        // (which is not Send) is only ever called on the main thread.
        let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(interval, true, &block) };
        // Let the system coalesce wakeups.
        timer.setTolerance(interval * 0.1);
        // Common modes: keep animating while a menu or drag is tracking.
        // SAFETY: a timer and a run loop mode constant.
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        *this.timer.borrow_mut() = Some((timer, fps));
    }

    fn stop_timer(&self) {
        if let Some((timer, _)) = self.timer.borrow_mut().take() {
            timer.invalidate();
        }
    }
}

impl Drop for GlyphDriver {
    fn drop(&mut self) {
        self.stop_timer();
    }
}

/// System Settings > Accessibility > Display > Reduce motion.
fn reduce_motion() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

impl Drop for StatusItem {
    fn drop(&mut self) {
        if let Some(monitor) = self.outside_monitor.take() {
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
    }
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
        assert_eq!(MenuBarState::from_snapshot(&snap()), MenuBarState::Idle);

        let mut s = snap();
        s.model = ModelStatus::Missing;
        assert_eq!(
            MenuBarState::from_snapshot(&s),
            MenuBarState::NeedsAttention
        );

        s.meeting = Some(MeetingSnap {
            title: "M".into(),
            path: "/m.md".into(),
            since: now - Duration::from_secs(724),
            started_at: chrono::Local::now(),
            app: None,
            mic_lost: false,
        });
        assert_eq!(MenuBarState::from_snapshot(&s), MenuBarState::Meeting);

        // Dictating during a meeting: the recording wins, push-to-talk and
        // hands-free alike.
        for hands_free in [false, true] {
            s.dictation = DictationPhase::Recording {
                since: now,
                hands_free,
            };
            assert_eq!(MenuBarState::from_snapshot(&s), MenuBarState::Recording);
        }

        s.dictation = DictationPhase::Transcribing;
        assert_eq!(MenuBarState::from_snapshot(&s), MenuBarState::Transcribing);

        // Back to idle with the meeting still on: the pulse again.
        s.dictation = DictationPhase::Idle;
        assert_eq!(MenuBarState::from_snapshot(&s), MenuBarState::Meeting);
    }
}
