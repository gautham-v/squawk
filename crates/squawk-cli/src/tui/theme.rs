//! The only styles the TUI uses: terminal defaults and attributes, never a
//! colour of its own. Text inherits the terminal's foreground, secondary
//! text is the terminal's own faint rendering of it, and the one rule is
//! ANSI "bright black". So whatever Ghostty theme is on, squawk looks like
//! it belongs there, in dark and light alike.

use ratatui::style::{Color, Modifier, Style};

/// Body text: the terminal's own ink.
pub const PLAIN: Style = Style::new();

/// What leads: the selected tab, the selected row, speaker labels.
pub const STRONG: Style = Style::new().add_modifier(Modifier::BOLD);

/// Secondary text that must still be read: times, dates, the key bar. Faint
/// rather than `DarkGray`, which some themes set almost to the background.
pub const DIM: Style = Style::new().add_modifier(Modifier::DIM);

/// The vertical rule between list and preview: structure, never read.
pub const RULE: Style = Style::new().fg(Color::DarkGray);

/// The bar beside the selected row, and the flash in the key bar.
pub const MARK: Style = Style::new().add_modifier(Modifier::BOLD);

/// The one word that says a meeting is still recording.
pub const LIVE: Style = Style::new().fg(Color::Red);
