//! Drawing the menu bar item's image with CoreGraphics (port the approach of
//! claudebar's `menu_bar_icon.rs`: draw into an NSImage at the item's
//! height, mark it template when it is monochrome so AppKit tints it for the
//! menu bar's appearance).
//!
//! The glyph: five vertical rounded bars, heights 6/10/14/10/6 pt on an 18 pt
//! canvas, 2 pt wide, 1.5 pt gaps — a still waveform. Idle is a template
//! image. Recording/meeting draw the glyph (or a 6 pt dot for meetings) and
//! the monospaced-digit time in copper, not template.

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::NSImage;

use crate::status_item::MenuBarState;

/// Copper for a dark menu bar.
pub const COPPER_DARK: (u8, u8, u8) = (0xbb, 0x86, 0x69);
/// Copper for a light menu bar.
pub const COPPER_LIGHT: (u8, u8, u8) = (0xa1, 0x61, 0x35);

/// Bar heights in points, left to right.
pub const GLYPH_BARS: [f64; 5] = [6.0, 10.0, 14.0, 10.0, 6.0];
pub const GLYPH_BAR_WIDTH: f64 = 2.0;
pub const GLYPH_BAR_GAP: f64 = 1.5;

/// The image for `state`. `dark` = the menu bar's effective appearance is
/// dark (picks the copper). Returns (image, is_template).
pub fn item_image(
    mtm: MainThreadMarker,
    state: &MenuBarState,
    dark: bool,
) -> (Retained<NSImage>, bool) {
    let _ = (mtm, state, dark);
    todo!("app agent")
}
