//! Colors and sizes for the popover. Port claudebar's `ui/theme.rs` (light
//! and dark materials, text ramps, separators, fonts) and add the copper
//! accent from `menu_bar_icon`.

/// Popover width in px. Wider than claudebar's 260 because History rows
/// carry two lines of dictated text.
pub const POPOVER_WIDTH: f32 = 340.0;
/// Popover max height before the list scrolls.
pub const POPOVER_MAX_HEIGHT: f32 = 520.0;
