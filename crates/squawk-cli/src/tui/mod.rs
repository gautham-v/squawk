//! The TUI: History and Meetings, a list on the left and a preview on the
//! right, "/" to search, enter to copy, "o" to open a meeting in $EDITOR,
//! tab to switch, q to quit, one key bar at the bottom.
//!
//! Look: like catcher (`theme.rs`/`ui.rs`) — the terminal's own default
//! fg/bg and ANSI palette colors only (`Color::Reset`, `Color::DarkGray`,
//! indexed 0–15), so it follows the user's Ghostty theme; no RGB, no heavy
//! borders (a single dim vertical rule between list and preview at most).

mod app;
mod theme;
mod ui;

use crate::commands::Env;

pub fn run(env: &Env) -> anyhow::Result<()> {
    let _ = env;
    todo!("cli agent")
}
