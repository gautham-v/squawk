//! squawk-app — the menu bar dictation app (bundled as Squawk.app).
//!
//! Plumbing only, like claudebar/daybar's main.rs: accessory activation
//! policy, the status item, the popover window anchored under it (toggled by
//! a click, closed on Esc, an outside click or focus loss), and the wiring
//! between the controller's snapshots and the views. See docs/design.md, "Startup".
//!
//! Unlike claudebar, the popover window is made once, hidden, at startup and
//! only moved, shown and hidden after that (see [`Panel`]). The notetaker's
//! prompt panel is a second PopUp window hung under the item while the
//! snapshot has a prompt and the popover is hidden.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::{
    point, px, size, App, AppContext, Application, AsyncApp, Bounds, Entity, Focusable, Pixels,
    Subscription, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions,
};
use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSView, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use squawk_core::hotkey::Timings;
use squawk_core::{Config, Paths};
use squawk_engine::{Engine, EngineConfig};

use squawk_app::controller::{Command, Controller, Snapshot};
use squawk_app::hotkey::{HotkeyTap, SharedMachine};
use squawk_app::status_item::{Anchor, MenuBarState, ScreenRect, StatusItem, StatusItemEvent};
use squawk_app::ui::panel::{self, PanelReply, PromptPanel};
use squawk_app::ui::popover::{self, Popover, PopoverEvent};
use squawk_app::ui::theme;
use squawk_app::{ipc_server, launch_at_login, logger, permissions};

/// Keep the popover this far from the screen edges.
const SCREEN_MARGIN: f32 = 8.0;
/// A status-item click this soon after the popover closed itself is the
/// click that closed it; swallow it rather than reopening.
const TOGGLE_GRACE: Duration = Duration::from_millis(250);
/// The popover never gets shorter than this, however little room there is.
const MIN_POPOVER_HEIGHT: f32 = 200.0;
/// How often an open popover's header clock is re-checked while it is
/// counting. Under a second, so the seconds tick over on time rather than up
/// to a second late.
const REDRAW_EVERY: Duration = Duration::from_millis(250);
/// How often a refused event tap is retried (Accessibility granted later
/// takes effect without a relaunch).
const TAP_RETRY: Duration = Duration::from_secs(2);
/// How long quitting waits for a running meeting's last chunks.
const QUIT_WAIT: Duration = Duration::from_secs(30);

type PanelSlot = Rc<RefCell<Option<WindowHandle<PromptPanel>>>>;

/// The popover's one window, made hidden at startup and then only moved,
/// shown and hidden, like a native menu. Measured: a fresh gpui window costs
/// a Metal renderer and its pipelines (~270 ms the first time, ~10 ms after)
/// before its first frame, while a window that becomes key again presents
/// its next frame synchronously, in the same transaction that shows it.
///
/// AppKit calls go through the `NSWindow` directly, outside any gpui update:
/// showing it re-enters gpui (the key-status change draws a frame), which
/// would find the app already borrowed if done from inside one.
#[derive(Clone)]
struct Panel {
    window: Rc<RefCell<Option<PanelWindow>>>,
    shown: Rc<Cell<bool>>,
    /// Whether it has become key since it was last shown: only losing key
    /// status after that is focus loss.
    became_key: Rc<Cell<bool>>,
    /// When the panel last hid itself on focus loss (see [`TOGGLE_GRACE`]).
    closed_at: Rc<Cell<Option<Instant>>>,
}

struct PanelWindow {
    ns_window: Retained<NSWindow>,
    handle: WindowHandle<Popover>,
    _activation: Subscription,
}

impl Panel {
    fn new() -> Panel {
        Panel {
            window: Rc::new(RefCell::new(None)),
            shown: Rc::new(Cell::new(false)),
            became_key: Rc::new(Cell::new(false)),
            closed_at: Rc::new(Cell::new(None)),
        }
    }

    fn is_shown(&self) -> bool {
        self.shown.get()
    }

    /// Make the window, hidden. Called at startup, and again on a click if
    /// that failed.
    fn create(&self, cx: &mut App, popover: Entity<Popover>) -> anyhow::Result<()> {
        if self.window.borrow().is_some() {
            return Ok(());
        }
        let bounds = Bounds {
            origin: point(px(0.), px(0.)),
            size: size(theme::POPOVER_WIDTH, px(theme::POPOVER_HEIGHT_PX)),
        };
        let panel = self.clone();
        let mut made: Option<(Retained<NSWindow>, Subscription)> = None;
        let handle = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                focus: false,
                show: false,
                // PopUp is a borderless, non-activating NSPanel: the menu bar
                // popover behaviour.
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Blurred,
                window_min_size: None,
                display_id: None,
                app_id: None,
                window_decorations: None,
                tabbing_identifier: None,
            },
            |window, cx| {
                let subscription = popover.update(cx, |_, cx| {
                    // Focus loss hides it. gpui also reports the state once
                    // at registration, and AppKit can report a spurious
                    // resign while the panel is being shown; only a
                    // deactivation after it really became key counts.
                    cx.observe_window_activation(window, move |_, window, cx| {
                        if window.is_window_active() {
                            panel.became_key.set(panel.is_shown());
                        } else if panel.is_shown() && panel.became_key.get() {
                            panel.closed_at.set(Some(Instant::now()));
                            panel.hide(cx);
                        }
                    })
                });
                made = ns_window_of(window).map(|w| (w, subscription));
                window.focus(&popover.focus_handle(cx));
                popover.clone()
            },
        )?;
        let (ns_window, activation) =
            made.ok_or_else(|| anyhow::anyhow!("popover window has no NSWindow"))?;
        *self.window.borrow_mut() = Some(PanelWindow {
            ns_window,
            handle,
            _activation: activation,
        });
        Ok(())
    }

    /// Give the popover's view keyboard focus again, so ←/→/↑/↓, Return
    /// and Esc reach it as soon as the window is key. Call before
    /// [`Panel::show`].
    fn focus(&self, cx: &mut App) {
        let Some(handle) = self.window.borrow().as_ref().map(|w| w.handle) else {
            return;
        };
        // The window's root view is the popover, leased for this update:
        // take its focus handle from the closure's view, not the entity.
        let _ = handle.update(cx, |popover, window, cx| {
            window.focus(&popover.focus_handle(cx));
        });
    }

    /// Move the window to `frame` (AppKit coordinates) and make it key.
    /// Must run outside any gpui update: see the type's docs.
    fn show(&self, frame: NSRect) {
        let Some(ns_window) = self.ns_window() else {
            return;
        };
        self.shown.set(true);
        self.became_key.set(false);
        ns_window.setFrame_display(frame, false);
        ns_window.makeKeyAndOrderFront(None);
    }

    /// Hide the window if it is showing; returns whether it was. The
    /// `orderOut` itself runs on the next turn of the main loop, outside
    /// the current gpui update.
    fn hide(&self, cx: &mut App) -> bool {
        if !self.shown.replace(false) {
            return false;
        }
        if let Some(ns_window) = self.ns_window() {
            let shown = self.shown.clone();
            cx.spawn(async move |_| {
                // A click may have shown it again in the meantime.
                if !shown.get() {
                    ns_window.orderOut(None);
                }
            })
            .detach();
        }
        true
    }

    fn ns_window(&self) -> Option<Retained<NSWindow>> {
        self.window.borrow().as_ref().map(|w| w.ns_window.clone())
    }
}

/// The `NSWindow` behind a gpui window.
fn ns_window_of(window: &gpui::Window) -> Option<Retained<NSWindow>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    // SAFETY: gpui's AppKit handle points at its live content NSView; we
    // are on the main thread, inside gpui's window callback.
    let view: &NSView = unsafe { appkit.ns_view.cast::<NSView>().as_ref() };
    view.window()
}

/// Where the popover goes: the left and top edges it is pinned to, and the
/// bottom of the display it is on.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Placement {
    x: f32,
    top: f32,
    screen_bottom: f32,
}

fn main() {
    let base_paths = match Paths::detect() {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("squawk: {e}");
            std::process::exit(1);
        }
    };
    let (config, config_note) = Config::load(&base_paths.config_file);
    let paths = base_paths.clone().with_config(&config);
    if let Err(e) = paths.ensure_dirs() {
        eprintln!("squawk: could not create {}: {e}", paths.data_dir.display());
    }
    if let Err(e) = Config::write_default_if_missing(&paths.config_file) {
        eprintln!(
            "squawk: could not write {}: {e}",
            paths.config_file.display()
        );
    }
    logger::install(&paths.log_file);
    log::info!("squawk {} starting", squawk_core::VERSION);

    Application::new().run(move |cx: &mut App| {
        let mtm = MainThreadMarker::new().expect("gpui runs its callbacks on the main thread");
        squawk_app::status_item::set_accessory_activation_policy(mtm);
        popover::bind_keys(cx);

        let engine = Engine::new(EngineConfig::from_config(&paths, &config));
        let machine = SharedMachine::new(Timings::from(&config.hotkey));
        let initial = Snapshot::initial(paths.clone(), engine.model_status(), config_note.clone());

        let (snap_tx, mut snaps) = futures::channel::mpsc::unbounded::<Snapshot>();
        let controller = Controller::spawn(
            base_paths.clone(),
            config.clone(),
            config_note.clone(),
            engine.clone(),
            machine.clone(),
            move |snapshot| {
                let _ = snap_tx.unbounded_send(snapshot);
            },
        );

        if let Err(e) = ipc_server::spawn(&paths.socket, controller.clone()) {
            log::error!("{e}; quitting");
            eprintln!("squawk: {e}");
            cx.quit();
            return;
        }
        engine.ensure_model();

        {
            let controller = controller.clone();
            let socket = paths.socket.clone();
            cx.on_app_quit(move |_| {
                controller.shutdown(QUIT_WAIT);
                let _ = std::fs::remove_file(&socket);
                async {}
            })
            .detach();
        }

        let level = engine.input_level();
        let (item, mut clicks) =
            StatusItem::new(mtm, MenuBarState::from_snapshot(&initial), move || {
                level.rms()
            });
        let item = Rc::new(item);
        let current = Rc::new(RefCell::new(initial.clone()));
        let popover = cx.new(|cx| Popover::new(initial, cx));
        // Read the files and the launch-at-login status now, in the
        // background, so the first open already has them.
        popover.update(cx, |p, cx| p.reload(cx));
        cx.background_executor()
            .spawn(async { launch_at_login::refresh() })
            .detach();
        let panel = Panel::new();
        if let Err(err) = panel.create(cx, popover.clone()) {
            log::warn!("could not make the popover window: {err}");
        }
        let prompt_panel = cx.new(|_| PromptPanel::new(None));
        let panel_window: PanelSlot = Rc::new(RefCell::new(None));

        cx.subscribe(&prompt_panel, {
            let controller = controller.clone();
            move |_panel, PanelReply(reply), _cx| controller.send(Command::Prompt(*reply))
        })
        .detach();

        // Snapshots from the controller: redraw the item, the popover and
        // the prompt panel.
        {
            let item = item.clone();
            let popover = popover.clone();
            let current = current.clone();
            let panel = panel.clone();
            let prompt_panel = prompt_panel.clone();
            let panel_window = panel_window.clone();
            cx.spawn(async move |cx| {
                while let Some(snapshot) = snaps.next().await {
                    item.set_state(mtm, MenuBarState::from_snapshot(&snapshot));
                    *current.borrow_mut() = snapshot.clone();
                    let prompt = snapshot.prompt.clone();
                    let updated = cx.update(|cx| {
                        popover.update(cx, |p, cx| p.set_snapshot(snapshot, cx));
                        let popover_open = panel.is_shown();
                        sync_panel(
                            cx,
                            prompt,
                            popover_open,
                            &prompt_panel,
                            &panel_window,
                            item.anchor(mtm),
                        );
                    });
                    if updated.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }

        // The timers: an open popover repaints its header clock while a
        // recording or a meeting is counting (the menu bar item animates on
        // its own timer), and the prompt panel steps aside while the popover
        // is open and comes back when it closes.
        {
            let item = item.clone();
            let popover = popover.clone();
            let current = current.clone();
            let panel = panel.clone();
            let prompt_panel = prompt_panel.clone();
            let panel_window = panel_window.clone();
            cx.spawn(async move |cx| loop {
                cx.background_executor().timer(REDRAW_EVERY).await;
                let prompt = current.borrow().prompt.clone();
                let popover_open = panel.is_shown();
                let shown = panel_window.borrow().is_some();
                if shown != (prompt.is_some() && !popover_open)
                    && cx
                        .update(|cx| {
                            sync_panel(
                                cx,
                                prompt,
                                popover_open,
                                &prompt_panel,
                                &panel_window,
                                item.anchor(mtm),
                            )
                        })
                        .is_err()
                {
                    break;
                }
                let counting = matches!(
                    MenuBarState::from_snapshot(&current.borrow()),
                    MenuBarState::Recording | MenuBarState::Meeting
                );
                if !counting {
                    continue;
                }
                if panel.is_shown()
                    && cx
                        .update(|cx| popover.update(cx, |_, cx| cx.notify()))
                        .is_err()
                {
                    break;
                }
            })
            .detach();
        }

        start_hotkeys(cx, machine, controller.clone());

        cx.subscribe(&popover, {
            let panel = panel.clone();
            let controller = controller.clone();
            move |_popover, event, cx| match event {
                PopoverEvent::Close => {
                    panel.hide(cx);
                }
                PopoverEvent::ToggleMeeting => {
                    controller.send(Command::ToggleMeeting);
                    panel.hide(cx);
                }
                PopoverEvent::SetSetting(setting) => {
                    controller.send(Command::SetSetting(*setting));
                }
            }
        })
        .detach();

        cx.spawn({
            let item = item.clone();
            let popover = popover.clone();
            async move |cx| {
                while let Some(event) = clicks.next().await {
                    if event == StatusItemEvent::ClickedOutside {
                        if cx.update(|cx| panel.hide(cx)).is_err() {
                            break;
                        }
                        continue;
                    }
                    match toggle_popover(
                        cx,
                        &panel,
                        &panel_window,
                        &popover,
                        &controller,
                        item.anchor(mtm),
                    ) {
                        Ok(Some(frame)) => {
                            panel.show(frame);
                            log::debug!(
                                "popover shown {:.1} ms after the click",
                                squawk_app::status_item::since_click().as_secs_f64() * 1e3
                            );
                        }
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
            }
        })
        .detach();
    });
}

/// Install the fn tap, retrying every couple of seconds while Accessibility
/// is missing (with one system prompt), so granting it takes effect without
/// a relaunch.
fn start_hotkeys(cx: &mut App, machine: SharedMachine, controller: Controller) {
    cx.spawn(async move |cx| {
        let mut prompted = false;
        let mut reported: Option<bool> = None;
        loop {
            let sink = controller.clone();
            match HotkeyTap::start(machine.clone(), move |action| {
                sink.send(Command::Hotkey(action))
            }) {
                Ok(tap) => {
                    controller.send(Command::TapInstalled(true));
                    // Held for the life of the app.
                    std::mem::forget(tap);
                    return;
                }
                Err(e) => {
                    if reported != Some(false) {
                        log::warn!("event tap refused: {e:?}");
                        controller.send(Command::TapInstalled(false));
                        reported = Some(false);
                    }
                    if !prompted {
                        prompted = true;
                        permissions::prompt_accessibility();
                    }
                }
            }
            cx.background_executor().timer(TAP_RETRY).await;
        }
    })
    .detach();
}

/// Show the prompt panel when there is a prompt and the popover is closed;
/// otherwise take it down. An open panel just gets the new prompt.
fn sync_panel(
    cx: &mut App,
    prompt: Option<squawk_core::notetaker::Prompt>,
    popover_open: bool,
    panel: &Entity<PromptPanel>,
    slot: &PanelSlot,
    anchor: Option<Anchor>,
) {
    if prompt.is_none() || popover_open {
        close_panel(slot, cx);
        return;
    }
    panel.update(cx, |p, cx| p.set_prompt(prompt, cx));
    if slot.borrow().is_some() {
        return;
    }
    let placed = placement_for(cx, anchor, panel::PANEL_WIDTH_PX);
    let bounds = Bounds {
        origin: point(px(placed.x), px(placed.top)),
        size: size(panel::PANEL_WIDTH, px(panel::PANEL_HEIGHT_PX)),
    };
    let panel = panel.clone();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            // Never take focus from the call.
            focus: false,
            show: true,
            // A non-activating panel that also shows over full-screen apps.
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Blurred,
            window_min_size: None,
            display_id: None,
            app_id: None,
            window_decorations: None,
            tabbing_identifier: None,
        },
        |_, _| panel,
    );
    match opened {
        Ok(handle) => *slot.borrow_mut() = Some(handle),
        Err(err) => log::warn!("could not open the prompt panel: {err}"),
    }
}

fn close_panel(slot: &PanelSlot, cx: &mut App) {
    if let Some(handle) = slot.borrow_mut().take() {
        let _ = handle.update(cx, |_, w, _| w.remove_window());
    }
}

/// A click on the item: hide the popover if it is showing, else get it
/// ready to show and return where (the caller shows it, outside this
/// update). `Err` once the app is gone.
fn toggle_popover(
    cx: &mut AsyncApp,
    panel: &Panel,
    panel_window: &PanelSlot,
    popover: &Entity<Popover>,
    controller: &Controller,
    anchor: Option<Anchor>,
) -> anyhow::Result<Option<NSRect>> {
    cx.update(|cx| {
        if panel.hide(cx) {
            panel.closed_at.set(None);
            return None;
        }
        if panel
            .closed_at
            .take()
            .is_some_and(|t| t.elapsed() < TOGGLE_GRACE)
        {
            return None;
        }
        if let Err(err) = panel.create(cx, popover.clone()) {
            log::warn!("could not open popover: {err}");
            return None;
        }
        // Permissions, config and model are re-checked on the controller
        // thread; the popover reads its files in the background.
        controller.send(Command::RecheckPermissions);
        close_panel(panel_window, cx);
        popover.update(cx, |p, cx| p.reset(cx));
        panel.focus(cx);
        let bounds = popover_bounds(
            placement_for(cx, anchor, theme::POPOVER_WIDTH_PX),
            px(theme::POPOVER_HEIGHT_PX),
        );
        Some(appkit_frame(bounds, primary_height(cx)))
    })
}

fn primary_bounds(cx: &App) -> Bounds<Pixels> {
    cx.primary_display().map(|d| d.bounds()).unwrap_or(Bounds {
        origin: point(px(0.), px(0.)),
        size: size(px(1440.), px(900.)),
    })
}

fn primary_height(cx: &App) -> f32 {
    primary_bounds(cx).size.height.into()
}

fn placement_for(cx: &App, anchor: Option<Anchor>, width: f32) -> Placement {
    let b = primary_bounds(cx);
    let primary = ScreenRect {
        x: b.origin.x.into(),
        y: b.origin.y.into(),
        width: b.size.width.into(),
        height: b.size.height.into(),
    };
    placement(anchor, primary, width)
}

/// Centred under the item, top edge under the menu bar, kept inside the
/// display the item is on. Pure, so the arithmetic is testable.
fn placement(anchor: Option<Anchor>, primary: ScreenRect, width: f32) -> Placement {
    let gap: f32 = theme::POPOVER_TOP_GAP.into();
    let screen = anchor.map_or(primary, |a| a.screen);
    let left = screen.x + SCREEN_MARGIN;
    let right = (screen.x + screen.width - width - SCREEN_MARGIN).max(left);
    let (x, top) = match anchor {
        Some(a) => {
            let centered = a.item.x + a.item.width / 2.0 - width / 2.0;
            (centered.clamp(left, right), a.item.y + a.item.height + gap)
        }
        None => (right, screen.y + 28.0 + gap),
    };
    Placement {
        x,
        top,
        screen_bottom: screen.y + screen.height,
    }
}

fn popover_bounds(placed: Placement, height: Pixels) -> Bounds<Pixels> {
    let height = clamp_height(height.into(), placed.top, placed.screen_bottom);
    Bounds {
        origin: point(px(placed.x), px(placed.top)),
        size: size(theme::POPOVER_WIDTH, px(height)),
    }
}

/// gpui's screen coordinates (top-left origin, y down) to an AppKit frame
/// (bottom-left origin, y up), flipped through the primary display.
fn appkit_frame(bounds: Bounds<Pixels>, primary_height: f32) -> NSRect {
    let x: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    NSRect::new(
        NSPoint::new(x as f64, (primary_height - top - height) as f64),
        NSSize::new(width as f64, height as f64),
    )
}

/// Clip to the room under the menu bar rather than growing off the screen.
fn clamp_height(height: f32, top_y: f32, display_bottom: f32) -> f32 {
    let room = display_bottom - top_y - SCREEN_MARGIN;
    height.min(room.max(MIN_POPOVER_HEIGHT))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(x: f32, y: f32, width: f32, height: f32) -> ScreenRect {
        ScreenRect {
            x,
            y,
            width,
            height,
        }
    }

    fn anchor_on(screen: ScreenRect, x: f32) -> Option<Anchor> {
        Some(Anchor {
            item: ScreenRect {
                x,
                y: screen.y,
                width: 40.0,
                height: 24.0,
            },
            screen,
        })
    }

    #[test]
    fn centred_under_the_item() {
        let primary = screen(0.0, 0.0, 1440.0, 900.0);
        let placed = placement(anchor_on(primary, 1000.0), primary, theme::POPOVER_WIDTH_PX);
        assert_eq!(placed.x, 1000.0 + 20.0 - theme::POPOVER_WIDTH_PX / 2.0);
        assert_eq!(placed.top, 24.0);
    }

    #[test]
    fn clamped_to_the_right_edge() {
        let primary = screen(0.0, 0.0, 1440.0, 900.0);
        let placed = placement(anchor_on(primary, 1420.0), primary, theme::POPOVER_WIDTH_PX);
        assert_eq!(placed.x, 1440.0 - theme::POPOVER_WIDTH_PX - SCREEN_MARGIN);
    }

    #[test]
    fn an_item_on_a_second_display_stays_under_the_item() {
        let second = screen(-561.0, -1440.0, 2560.0, 1440.0);
        let placed = placement(
            anchor_on(second, 1600.0),
            screen(0.0, 0.0, 1512.0, 982.0),
            theme::POPOVER_WIDTH_PX,
        );
        assert_eq!(placed.x, 1600.0 + 20.0 - theme::POPOVER_WIDTH_PX / 2.0);
        assert_eq!(placed.top, second.y + 24.0);
        assert_eq!(placed.screen_bottom, 0.0);
    }

    #[test]
    fn the_prompt_panel_hangs_centred_under_the_item_too() {
        let primary = screen(0.0, 0.0, 1440.0, 900.0);
        let placed = placement(anchor_on(primary, 1000.0), primary, panel::PANEL_WIDTH_PX);
        assert_eq!(placed.x, 1000.0 + 20.0 - panel::PANEL_WIDTH_PX / 2.0);
        assert_eq!(placed.top, 24.0);
        let placed = placement(anchor_on(primary, 1420.0), primary, panel::PANEL_WIDTH_PX);
        assert_eq!(placed.x, 1440.0 - panel::PANEL_WIDTH_PX - SCREEN_MARGIN);
    }

    #[test]
    fn frames_flip_into_appkit_coordinates() {
        let bounds = Bounds {
            origin: point(px(100.), px(24.)),
            size: size(px(340.), px(480.)),
        };
        let frame = appkit_frame(bounds, 900.0);
        assert_eq!(frame.origin, NSPoint::new(100.0, 900.0 - 24.0 - 480.0));
        assert_eq!(frame.size, NSSize::new(340.0, 480.0));
        // A display above the primary one has negative gpui y.
        let above = Bounds {
            origin: point(px(0.), px(-1416.)),
            size: size(px(340.), px(480.)),
        };
        assert_eq!(appkit_frame(above, 982.0).origin.y, 982.0 + 1416.0 - 480.0);
    }

    #[test]
    fn a_short_screen_clips_the_popover() {
        assert_eq!(clamp_height(480.0, 24.0, 400.0), 400.0 - 24.0 - 8.0);
        assert_eq!(clamp_height(480.0, 24.0, 900.0), 480.0);
        assert_eq!(clamp_height(480.0, 890.0, 900.0), MIN_POPOVER_HEIGHT);
    }
}
