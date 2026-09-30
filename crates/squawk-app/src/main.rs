//! squawk-app — the menu bar dictation app (bundled as Squawk.app).
//!
//! Plumbing only, like claudebar/daybar's main.rs: accessory activation
//! policy, the status item, the popover window anchored under it (toggled by
//! a click, closed on Esc, an outside click or focus loss), and the wiring
//! between the controller's snapshots and the views. See SPEC.md, "Startup".

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::{
    point, px, size, App, AppContext, Application, Bounds, Entity, Focusable, Pixels, Subscription,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions,
};
use objc2::MainThreadMarker;
use squawk_core::hotkey::Timings;
use squawk_core::{Config, Paths};
use squawk_engine::{Engine, EngineConfig};

use squawk_app::controller::{Command, Controller, Snapshot};
use squawk_app::hotkey::{HotkeyTap, SharedMachine};
use squawk_app::status_item::{Anchor, MenuBarState, ScreenRect, StatusItem, StatusItemEvent};
use squawk_app::ui::popover::{self, Popover, PopoverEvent};
use squawk_app::ui::theme;
use squawk_app::{ipc_server, logger, permissions};

/// Keep the popover this far from the screen edges.
const SCREEN_MARGIN: f32 = 8.0;
/// A status-item click this soon after the popover closed itself is the
/// click that closed it; swallow it rather than reopening.
const TOGGLE_GRACE: Duration = Duration::from_millis(250);
/// The popover never gets shorter than this, however little room there is.
const MIN_POPOVER_HEIGHT: f32 = 200.0;
/// How often the menu bar timer is re-checked while it is counting. Under a
/// second, so the seconds tick over on time rather than up to a second late.
const REDRAW_EVERY: Duration = Duration::from_millis(250);
/// How often a refused event tap is retried (Accessibility granted later
/// takes effect without a relaunch).
const TAP_RETRY: Duration = Duration::from_secs(2);
/// How long quitting waits for a running meeting's last chunks.
const QUIT_WAIT: Duration = Duration::from_secs(30);

type WindowSlot = Rc<RefCell<Option<WindowHandle<Popover>>>>;

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

        let (item, mut clicks) =
            StatusItem::new(mtm, MenuBarState::from_snapshot(&initial, Instant::now()));
        let item = Rc::new(item);
        let current = Rc::new(RefCell::new(initial.clone()));
        let popover = cx.new(|cx| Popover::new(initial, cx));
        let window: WindowSlot = Rc::new(RefCell::new(None));

        // Snapshots from the controller: redraw the item and the popover.
        {
            let item = item.clone();
            let popover = popover.clone();
            let current = current.clone();
            cx.spawn(async move |cx| {
                while let Some(snapshot) = snaps.next().await {
                    item.set_state(mtm, MenuBarState::from_snapshot(&snapshot, Instant::now()));
                    *current.borrow_mut() = snapshot.clone();
                    let updated = cx.update(|cx| {
                        popover.update(cx, |p, cx| p.set_snapshot(snapshot, cx));
                    });
                    if updated.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }

        // The timers: the item redraws only when its text changes, and an
        // open popover repaints its header clock.
        {
            let item = item.clone();
            let popover = popover.clone();
            let current = current.clone();
            let window = window.clone();
            cx.spawn(async move |cx| loop {
                cx.background_executor().timer(REDRAW_EVERY).await;
                let state = MenuBarState::from_snapshot(&current.borrow(), Instant::now());
                if !state.ticks() {
                    continue;
                }
                item.set_state(mtm, state);
                if window.borrow().is_some()
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
            let window = window.clone();
            let controller = controller.clone();
            move |_popover, event, cx| match event {
                PopoverEvent::Close => {
                    close_popover(&window, cx);
                }
                PopoverEvent::ToggleMeeting => {
                    controller.send(Command::ToggleMeeting);
                    close_popover(&window, cx);
                }
            }
        })
        .detach();

        let closed_at: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
        let activation: Rc<RefCell<Option<Subscription>>> = Rc::new(RefCell::new(None));
        cx.spawn({
            let item = item.clone();
            let popover = popover.clone();
            async move |cx| {
                while let Some(event) = clicks.next().await {
                    if event == StatusItemEvent::ClickedOutside {
                        if cx.update(|cx| close_popover(&window, cx)).is_err() {
                            break;
                        }
                        continue;
                    }
                    let anchor = item.anchor(mtm);
                    let result = cx.update(|cx| {
                        if close_popover(&window, cx) {
                            closed_at.set(None);
                            return;
                        }
                        if closed_at.take().is_some_and(|t| t.elapsed() < TOGGLE_GRACE) {
                            return;
                        }
                        controller.send(Command::RecheckPermissions);
                        popover.update(cx, |p, cx| p.reset(cx));
                        let placed = placement_for(cx, anchor);
                        match open_popover(
                            cx,
                            placed,
                            popover.clone(),
                            &activation,
                            &window,
                            &closed_at,
                        ) {
                            Ok(handle) => *window.borrow_mut() = Some(handle),
                            Err(err) => log::warn!("could not open popover: {err}"),
                        }
                    });
                    if result.is_err() {
                        break;
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

/// Close the popover if one is open. Returns whether it closed something.
fn close_popover(window: &WindowSlot, cx: &mut App) -> bool {
    let handle = window.borrow_mut().take();
    match handle {
        Some(handle) => handle.update(cx, |_, w, _| w.remove_window()).is_ok(),
        None => false,
    }
}

fn open_popover(
    cx: &mut App,
    placed: Placement,
    popover: Entity<Popover>,
    activation: &Rc<RefCell<Option<Subscription>>>,
    window_slot: &WindowSlot,
    closed_at: &Rc<Cell<Option<Instant>>>,
) -> anyhow::Result<WindowHandle<Popover>> {
    let bounds = popover_bounds(placed, px(theme::POPOVER_HEIGHT_PX));
    let activation = activation.clone();
    let window_slot = window_slot.clone();
    let closed_at = closed_at.clone();
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            focus: true,
            show: true,
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
                // gpui fires this once at registration, before the panel is
                // key; only a deactivation after a real activation closes.
                let was_active = Cell::new(false);
                cx.observe_window_activation(window, move |_, window, _| {
                    if window.is_window_active() {
                        was_active.set(true);
                    } else if was_active.get() {
                        *window_slot.borrow_mut() = None;
                        closed_at.set(Some(Instant::now()));
                        window.remove_window();
                    }
                })
            });
            *activation.borrow_mut() = Some(subscription);
            window.focus(&popover.focus_handle(cx));
            popover.clone()
        },
    )?;
    handle.update(cx, |_, window, _| window.activate_window())?;
    Ok(handle)
}

fn placement_for(cx: &App, anchor: Option<Anchor>) -> Placement {
    let b = cx.primary_display().map(|d| d.bounds()).unwrap_or(Bounds {
        origin: point(px(0.), px(0.)),
        size: size(px(1440.), px(900.)),
    });
    let primary = ScreenRect {
        x: b.origin.x.into(),
        y: b.origin.y.into(),
        width: b.size.width.into(),
        height: b.size.height.into(),
    };
    placement(anchor, primary)
}

/// Centred under the item, top edge under the menu bar, kept inside the
/// display the item is on. Pure, so the arithmetic is testable.
fn placement(anchor: Option<Anchor>, primary: ScreenRect) -> Placement {
    let width = theme::POPOVER_WIDTH_PX;
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
        let placed = placement(anchor_on(primary, 1000.0), primary);
        assert_eq!(placed.x, 1000.0 + 20.0 - theme::POPOVER_WIDTH_PX / 2.0);
        assert_eq!(placed.top, 24.0);
    }

    #[test]
    fn clamped_to_the_right_edge() {
        let primary = screen(0.0, 0.0, 1440.0, 900.0);
        let placed = placement(anchor_on(primary, 1420.0), primary);
        assert_eq!(placed.x, 1440.0 - theme::POPOVER_WIDTH_PX - SCREEN_MARGIN);
    }

    #[test]
    fn an_item_on_a_second_display_stays_under_the_item() {
        let second = screen(-561.0, -1440.0, 2560.0, 1440.0);
        let placed = placement(anchor_on(second, 1600.0), screen(0.0, 0.0, 1512.0, 982.0));
        assert_eq!(placed.x, 1600.0 + 20.0 - theme::POPOVER_WIDTH_PX / 2.0);
        assert_eq!(placed.top, second.y + 24.0);
        assert_eq!(placed.screen_bottom, 0.0);
    }

    #[test]
    fn a_short_screen_clips_the_popover() {
        assert_eq!(clamp_height(480.0, 24.0, 400.0), 400.0 - 24.0 - 8.0);
        assert_eq!(clamp_height(480.0, 24.0, 900.0), 480.0);
        assert_eq!(clamp_height(480.0, 890.0, 900.0), MIN_POPOVER_HEIGHT);
    }
}
