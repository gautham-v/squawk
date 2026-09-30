//! Colors and sizes for the popover, ported from claudebar's `ui/theme.rs`:
//! the system-menu material, black/white ink at the usual alphas, hairline
//! separators — plus squawk's one accent, copper, used only for the
//! recording/meeting state line (the same copper as the menu bar item).
//! Everything the views need comes from here; no literals in the views.

use gpui::{px, Pixels, Rgba, WindowAppearance};

/// `const`-friendly hex -> [`Rgba`] (gpui's own `rgb()` is not `const`).
const fn hex(value: u32) -> Rgba {
    Rgba {
        r: ((value >> 16) & 0xff) as f32 / 255.0,
        g: ((value >> 8) & 0xff) as f32 / 255.0,
        b: (value & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

/// Same, with an explicit alpha in 0.0..=1.0.
const fn hex_a(value: u32, alpha: f32) -> Rgba {
    let c = hex(value);
    Rgba { a: alpha, ..c }
}

// ── Colors ───────────────────────────────────────────────────────────────────

/// The appearance-dependent half of the tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// Popover background — the material the rows sit on.
    pub bg: Rgba,
    /// Hairline border around the popover.
    pub border: Rgba,
    /// Primary text.
    pub text: Rgba,
    /// Secondary text: row sources, meeting dates, unselected tabs.
    pub secondary: Rgba,
    /// Tertiary text: times, shortcuts, empty states, the hint line.
    pub tertiary: Rgba,
    /// Separator rules and the progress bar's track.
    pub separator: Rgba,
    /// Hover wash on a row.
    pub hover: Rgba,
    /// Copper: recording and meeting, nothing else.
    pub accent: Rgba,
}

/// Light appearance.
pub const LIGHT: Theme = Theme {
    bg: Rgba {
        r: 236.0 / 255.0,
        g: 236.0 / 255.0,
        b: 238.0 / 255.0,
        a: BG_ALPHA,
    },
    border: hex_a(0x000000, 0.06),
    text: hex_a(0x000000, 0.85),
    secondary: hex(0x6e6e73),
    tertiary: hex(0xaeaeb2),
    separator: hex_a(0x000000, 0.09),
    hover: hex_a(0x000000, 0.06),
    accent: hex(0xa16135),
};

/// Dark appearance: the same roles against a dark material.
pub const DARK: Theme = Theme {
    bg: Rgba {
        r: 40.0 / 255.0,
        g: 40.0 / 255.0,
        b: 42.0 / 255.0,
        a: BG_ALPHA,
    },
    border: hex_a(0xffffff, 0.10),
    text: hex_a(0xffffff, 0.85),
    secondary: hex(0x98989d),
    tertiary: hex(0x8e8e93),
    separator: hex_a(0xffffff, 0.12),
    hover: hex_a(0xffffff, 0.10),
    accent: hex(0xbb8669),
};

/// How opaque the popover material is over the blurred window: what is
/// behind shows through as a soft wash, as in a system menu.
pub const BG_ALPHA: f32 = 0.85;

impl Default for Theme {
    fn default() -> Self {
        LIGHT
    }
}

impl Theme {
    /// Pick the set matching the window's appearance.
    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => DARK,
            WindowAppearance::Light | WindowAppearance::VibrantLight => LIGHT,
        }
    }
}

// ── Sizes ────────────────────────────────────────────────────────────────────

/// Popover width in px. Wider than claudebar's 260 because History rows
/// carry two lines of dictated text.
pub const POPOVER_WIDTH_PX: f32 = 340.0;
pub const POPOVER_WIDTH: Pixels = px(POPOVER_WIDTH_PX);
/// The popover's height. Fixed, so switching tabs never makes the window
/// jump; the list between the tabs and the footer scrolls.
pub const POPOVER_HEIGHT_PX: f32 = 480.0;
/// Corner radius of the popover.
pub const POPOVER_RADIUS: Pixels = px(10.);
/// Zero: system menus hang straight off the menu bar.
pub const POPOVER_TOP_GAP: Pixels = px(0.);
/// The menu inset between the popover's edge and its rows.
pub const POPOVER_PAD: Pixels = px(5.0);

/// Horizontal padding inside a row — the text inset every line shares.
pub const ROW_PAD_X: Pixels = px(10.);
/// Vertical padding inside a menu row.
pub const ROW_PAD_Y: Pixels = px(3.0);
/// Vertical padding inside a list row (two-line History entries).
pub const LIST_ROW_PAD_Y: Pixels = px(5.0);
/// Corner radius of a row's hover wash.
pub const ROW_RADIUS: Pixels = px(6.);
/// The gap between the tabs.
pub const TAB_GAP: Pixels = px(14.);

/// Separator: inset from the edges, with air above and below.
pub const SEPARATOR_INSET: Pixels = px(10.);
pub const SEPARATOR_MARGIN: Pixels = px(5.0);
pub const HAIRLINE: Pixels = px(1.0);

/// The model download's progress bar.
pub const PROGRESS_HEIGHT: Pixels = px(2.0);

// ── Type scale ───────────────────────────────────────────────────────────────

/// The state line and every menu row.
pub const TEXT_BODY: Pixels = px(13.);
/// Dictated text in History rows, tabs.
pub const TEXT_SMALL: Pixels = px(12.);
/// Sources, times, notes.
pub const TEXT_TINY: Pixels = px(11.);

pub const LINE_BODY: Pixels = px(17.0);
pub const LINE_SMALL: Pixels = px(16.0);
pub const LINE_TINY: Pixels = px(14.0);

/// Monospace family, for the timers.
pub const MONO_FAMILY: &str = "SF Mono";
/// UI family.
pub const UI_FAMILY: &str = ".SystemUIFont";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_picks_the_matching_set() {
        assert_eq!(Theme::for_appearance(WindowAppearance::Light), LIGHT);
        assert_eq!(Theme::for_appearance(WindowAppearance::VibrantLight), LIGHT);
        assert_eq!(Theme::for_appearance(WindowAppearance::Dark), DARK);
        assert_eq!(Theme::for_appearance(WindowAppearance::VibrantDark), DARK);
    }

    /// The popover's copper is the menu bar's copper.
    #[test]
    fn the_accent_is_the_menu_bar_copper() {
        let (r, g, b) = crate::menu_bar_icon::COPPER_DARK;
        assert_eq!((DARK.accent.r * 255.0).round() as u8, r);
        assert_eq!((DARK.accent.g * 255.0).round() as u8, g);
        assert_eq!((DARK.accent.b * 255.0).round() as u8, b);
        let (r, _, _) = crate::menu_bar_icon::COPPER_LIGHT;
        assert_eq!((LIGHT.accent.r * 255.0).round() as u8, r);
    }

    #[test]
    fn the_popover_is_340_wide() {
        assert_eq!(POPOVER_WIDTH, px(340.));
        assert!((LIGHT.bg.a - BG_ALPHA).abs() < 1e-6);
    }
}
