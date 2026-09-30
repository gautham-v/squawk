//! TUI state and key handling, kept free of drawing so it is testable.

#[allow(dead_code)] // scaffold
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    History,
    Meetings,
}
