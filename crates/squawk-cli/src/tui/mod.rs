//! The TUI: History and Meetings, a list on the left and a preview on the
//! right, "/" to search, enter to copy, "o" to open in $EDITOR, tab to
//! switch, q to quit, one key bar at the bottom.
//!
//! Look: like catcher, but stricter about colour — the terminal's own
//! default fg/bg and attributes only (see `theme.rs`), so it follows the
//! user's Ghostty theme; no boxes, one dim vertical rule.
//!
//! Live: the files are re-read when their fingerprint changes (checked every
//! two seconds), so a new dictation appears and a live meeting grows on
//! screen without a key press.

mod app;
mod data;
mod theme;
mod ui;

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::DefaultTerminal;

use crate::commands::Env;
use app::{App, Effect};

/// How often the files are checked for changes.
const POLL_FILES: Duration = Duration::from_secs(2);
/// How long to wait for a key before redrawing (flash expiry, file poll).
const TICK: Duration = Duration::from_millis(250);

pub fn run(env: &Env) -> anyhow::Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        anyhow::bail!("the TUI needs a terminal; try `squawk history` or `squawk --help`");
    }
    let store = env.store.clone();
    let data = data::load(&store)?;
    let mut app = App::new(store.clone(), data, chrono::Local::now().date_naive());
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &store);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    store: &squawk_core::Store,
) -> anyhow::Result<()> {
    let mut seen = data::signature(store);
    let mut checked = Instant::now();
    loop {
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(TICK)? {
            let ev = event::read()?;
            let Event::Key(key) = ev else {
                continue; // resize and the rest just redraw
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match app.handle_key(key) {
                Effect::None => {}
                Effect::Quit => return Ok(()),
                Effect::Copy(text) => match crate::system::copy(&text) {
                    Ok(()) => app.flash("Copied"),
                    Err(e) => app.flash(format!("Copy failed: {e}")),
                },
                Effect::Edit(path) => {
                    suspend(terminal, || crate::system::edit(&path))
                        .unwrap_or_else(|e| app.flash(format!("{e}")));
                    seen = reload(app, store);
                }
                Effect::Reload => {
                    seen = reload(app, store);
                    app.flash("Reloaded");
                }
            }
        }
        if checked.elapsed() >= POLL_FILES {
            checked = Instant::now();
            app.today = chrono::Local::now().date_naive();
            let now = data::signature(store);
            if now != seen {
                seen = reload(app, store);
            }
        }
    }
}

/// Re-read the files; returns the fingerprint that was read.
fn reload(app: &mut App, store: &squawk_core::Store) -> u64 {
    let sig = data::signature(store);
    match data::load(store) {
        Ok(data) => app.set_data(data),
        Err(e) => app.flash(format!("Could not read: {e}")),
    }
    sig
}

/// Hand the terminal to a child (the editor) and take it back after.
fn suspend(
    terminal: &mut DefaultTerminal,
    f: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    terminal::disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;
    let result = f();
    std::io::stdout().execute(EnterAlternateScreen)?;
    terminal::enable_raw_mode()?;
    terminal.clear()?;
    result
}
