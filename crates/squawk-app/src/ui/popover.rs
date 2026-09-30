//! Root popover view.
//!
//! Layout, top to bottom:
//! - header: one state line — "Ready · fn to talk", "Recording 0:07",
//!   "Meeting · Weekly sync · 12:04", or a first-run state with its fix:
//!   "Downloading model 42%" (thin progress bar), "Needs Accessibility
//!   [Open Settings]", "Needs Microphone [Open Settings]"; then the config
//!   note / last error as a muted line if any;
//! - tabs: History | Meetings | Dictionary (text tabs, the selected one in
//!   primary ink, the others secondary; no pills);
//! - body: History = the 50 most recent dictations, each row "app · project"
//!   and time on one line, the text clamped to 2 lines below; click copies
//!   it (brief "Copied" in the row). Meetings = title, date · length, click
//!   opens the file. Dictionary = entries, one per row, "Edit" opens
//!   dictionary.txt;
//! - footer: "Record meeting ⌥M" (or "Stop meeting ⌥M") and "Settings"
//!   (opens config.toml, writing the default file first if missing).
//!
//! Reads files through `squawk_core::Store` / `Dictionary` when opened and
//! when `Snapshot.dictations_this_run` changes; holds no other data.

use gpui::{div, Context, IntoElement, Render, Window};

use crate::controller::Snapshot;

/// Which tab is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    History,
    Meetings,
    Dictionary,
}

/// What the popover asks its owner (main.rs) to do.
pub enum PopoverEvent {
    Close,
    ToggleMeeting,
    OpenSettings,
}

pub struct Popover {
    pub tab: Tab,
    pub snapshot: Option<Snapshot>,
}

impl Render for Popover {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
