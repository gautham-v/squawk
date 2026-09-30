//! Dev preview: puts one status item per menu bar state in the menu bar for
//! a few seconds, so the glyph, the copper and the timers can be checked
//! against the real menu bar in both appearances.
//!
//! `cargo run -p squawk-app --example menu_bar_preview [seconds]`

use std::time::Duration;

use gpui::{App, Application};
use objc2::MainThreadMarker;
use squawk_app::status_item::{MenuBarState, StatusItem};

fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    Application::new().run(move |cx: &mut App| {
        let mtm = MainThreadMarker::new().expect("main thread");
        squawk_app::status_item::set_accessory_activation_policy(mtm);
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
