//! Dev preview: puts one status item per menu bar state in the menu bar for
//! a few seconds, so the glyph, the copper and the timers can be checked
//! against the real menu bar in both appearances.
//!
//! `cargo run -p squawk-app --example menu_bar_preview [seconds] [panel]`
//!
//! With `panel`, one idle item instead, with the notetaker's call prompt
//! hanging under it the way the app shows it (a non-activating pop-up that
//! does not take focus); its screen rect is printed for a screenshot. A
//! crowded menu bar hides new items off screen; `panel <x>` hangs the panel
//! under screen x instead (the centre of an item that is visible).

use std::time::Duration;

use gpui::{
    point, px, size, App, AppContext, Application, Bounds, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions,
};
use objc2::MainThreadMarker;
use squawk_app::status_item::{MenuBarState, StatusItem};
use squawk_app::ui::panel::{self, PromptPanel};
use squawk_core::notetaker::calls::CallId;
use squawk_core::notetaker::Prompt;

fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    let with_panel = std::env::args().nth(2).as_deref() == Some("panel");
    let under_x: Option<f32> = std::env::args().nth(3).and_then(|s| s.parse().ok());
    Application::new().run(move |cx: &mut App| {
        let mtm = MainThreadMarker::new().expect("main thread");
        squawk_app::status_item::set_accessory_activation_policy(mtm);
        if with_panel {
            let (item, _clicks) = StatusItem::new(mtm, MenuBarState::Idle);
            cx.spawn(async move |cx| {
                // Let AppKit place the item before asking where it is.
                cx.background_executor()
                    .timer(Duration::from_millis(800))
                    .await;
                let anchor = item.anchor(mtm).expect("status item anchor");
                let centre = under_x.unwrap_or(anchor.item.x + anchor.item.width / 2.0);
                let x = centre - panel::PANEL_WIDTH_PX / 2.0;
                let top = anchor.item.y + anchor.item.height;
                println!(
                    "item at x={} y={}; panel at x={x} y={top} w={} h={}",
                    anchor.item.x,
                    anchor.item.y,
                    panel::PANEL_WIDTH_PX,
                    panel::PANEL_HEIGHT_PX
                );
                let prompt = Prompt::Call {
                    call: CallId(1),
                    app: "Zoom".into(),
                };
                let _ = cx.update(|cx| {
                    cx.open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds {
                                origin: point(px(x), px(top)),
                                size: size(panel::PANEL_WIDTH, px(panel::PANEL_HEIGHT_PX)),
                            })),
                            titlebar: None,
                            focus: false,
                            show: true,
                            kind: WindowKind::PopUp,
                            is_movable: false,
                            is_resizable: false,
                            window_background: WindowBackgroundAppearance::Blurred,
                            ..Default::default()
                        },
                        |_, cx| cx.new(|_| PromptPanel::new(Some(prompt))),
                    )
                });
                cx.background_executor()
                    .timer(Duration::from_secs(secs))
                    .await;
                drop(item);
                let _ = cx.update(|cx| cx.quit());
            })
            .detach();
            return;
        }
        // Right to left in the menu bar: the first item created sits
        // rightmost.
        let states = [
            MenuBarState::Idle,
            MenuBarState::NeedsAttention,
            MenuBarState::Recording {
                elapsed_secs: 7,
                hands_free: false,
            },
            MenuBarState::Recording {
                elapsed_secs: 65,
                hands_free: true,
            },
            MenuBarState::Transcribing,
            MenuBarState::Meeting { elapsed_secs: 724 },
        ];
        let items: Vec<StatusItem> = states
            .into_iter()
            .map(|state| StatusItem::new(mtm, state).0)
            .collect();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_secs(secs))
                .await;
            drop(items);
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });
}
