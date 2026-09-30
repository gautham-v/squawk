//! Drawing the menu bar item's image with CoreGraphics, the way claudebar's
//! `menu_bar_icon.rs` does: one NSImage built at runtime from a drawing
//! handler, text included, so spacing and fonts are ours rather than a
//! status item title's.
//!
//! The glyph: five vertical rounded bars, heights 6/10/14/10/6 pt, 2 pt wide,
//! 1.5 pt gaps — a still waveform. Idle, needs-attention and transcribing are
//! template images (AppKit tints them for the menu bar, and the status item's
//! disabled look dims them). Recording and meeting are the one place squawk
//! shows colour: glyph (or a dot) and a monospaced-digit time in copper, so
//! the image is *not* a template — a template would tint the copper away.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSColor, NSFont, NSFontAttributeName, NSFontWeightRegular,
    NSForegroundColorAttributeName, NSGraphicsContext, NSImage,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGContext, CGLineCap, CGPath};
use objc2_foundation::{NSAttributedString, NSDictionary, NSPoint, NSRect, NSSize, NSString};
use squawk_core::status::format_elapsed;

use crate::status_item::MenuBarState;

/// Copper for a dark menu bar.
pub const COPPER_DARK: (u8, u8, u8) = (0xbb, 0x86, 0x69);
/// Copper for a light menu bar.
pub const COPPER_LIGHT: (u8, u8, u8) = (0xa1, 0x61, 0x35);

/// Bar heights in points, left to right.
pub const GLYPH_BARS: [f64; 5] = [6.0, 10.0, 14.0, 10.0, 6.0];
pub const GLYPH_BAR_WIDTH: f64 = 2.0;
pub const GLYPH_BAR_GAP: f64 = 1.5;
/// The glyph's width: five bars and four gaps.
pub const GLYPH_WIDTH: f64 = 5.0 * GLYPH_BAR_WIDTH + 4.0 * GLYPH_BAR_GAP;

/// The image's height. The menu bar is 24 pt; 18 leaves the same air above
/// and below as the system glyphs.
pub const CANVAS_HEIGHT: f64 = 18.0;
/// The meeting dot's diameter.
pub const DOT: f64 = 6.0;
/// The hands-free lock mark's box.
pub const LOCK_WIDTH: f64 = 6.0;
pub const LOCK_HEIGHT: f64 = 8.0;
/// Between the glyph (or dot) and the time.
pub const MARK_GAP: f64 = 4.0;
/// Between the time and the lock.
pub const LOCK_GAP: f64 = 3.0;
/// The time's point size: a touch under the menu bar font, like the battery
/// percentage beside it.
pub const TEXT_POINT_SIZE: f64 = 12.0;

/// Where each piece of the item goes, left to right. Pure, so the spacing is
/// tested without AppKit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub width: f64,
    pub glyph_x: Option<f64>,
    pub dot_x: Option<f64>,
    pub text_x: Option<f64>,
    pub lock_x: Option<f64>,
}

/// The time the item prints, if any.
pub fn label(state: &MenuBarState) -> Option<String> {
    match state {
        MenuBarState::Recording { elapsed_secs, .. } | MenuBarState::Meeting { elapsed_secs } => {
            Some(format_elapsed(*elapsed_secs))
        }
        _ => None,
    }
}

/// Whether AppKit should tint the image (everything but the copper states).
pub fn is_template(state: &MenuBarState) -> bool {
    !matches!(
        state,
        MenuBarState::Recording { .. } | MenuBarState::Meeting { .. }
    )
}

/// Lay the item out, given the measured width of its [`label`].
pub fn layout(state: &MenuBarState, text_width: f64) -> Layout {
    match state {
        MenuBarState::Recording { hands_free, .. } => {
            let text_x = GLYPH_WIDTH + MARK_GAP;
            let mut width = text_x + text_width;
            let lock_x = hands_free.then(|| {
                let x = width + LOCK_GAP;
                width = x + LOCK_WIDTH;
                x
            });
            Layout {
                width,
                glyph_x: Some(0.0),
                dot_x: None,
                text_x: Some(text_x),
                lock_x,
            }
        }
        MenuBarState::Meeting { .. } => Layout {
            width: DOT + MARK_GAP + text_width,
            glyph_x: None,
            dot_x: Some(0.0),
            text_x: Some(DOT + MARK_GAP),
            lock_x: None,
        },
        MenuBarState::Idle | MenuBarState::NeedsAttention | MenuBarState::Transcribing => Layout {
            width: GLYPH_WIDTH,
            glyph_x: Some(0.0),
            dot_x: None,
            text_x: None,
            lock_x: None,
        },
    }
}

/// The five bars as (x, y, width, height) in the image's bottom-left-origin
/// space, vertically centred on the canvas.
pub fn bar_rects(origin_x: f64) -> [(f64, f64, f64, f64); 5] {
    let mut rects = [(0.0, 0.0, 0.0, 0.0); 5];
    for (i, height) in GLYPH_BARS.iter().enumerate() {
        let x = origin_x + i as f64 * (GLYPH_BAR_WIDTH + GLYPH_BAR_GAP);
        let y = (CANVAS_HEIGHT - height) / 2.0;
        rects[i] = (x, y, GLYPH_BAR_WIDTH, *height);
    }
    rects
}

/// Copper for the menu bar's appearance, as 0..1 sRGB components.
pub fn copper(dark: bool) -> (f64, f64, f64) {
    let (r, g, b) = if dark { COPPER_DARK } else { COPPER_LIGHT };
    (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0)
}

/// The image for `state`. `dark` = the menu bar's effective appearance is
/// dark (picks the copper). Returns (image, is_template).
pub fn item_image(state: &MenuBarState, dark: bool) -> (Retained<NSImage>, bool) {
    let template = is_template(state);
    let colour = if template {
        NSColor::blackColor()
    } else {
        let (r, g, b) = copper(dark);
        NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0)
    };
    let text = label(state).map(|l| text_run(&l, &colour));
    let text_width = text.as_ref().map_or(0.0, |t| t.size().width.ceil());
    let layout = layout(state, text_width);

    let handler = RcBlock::new(move |_dirty: NSRect| -> Bool {
        draw(&layout, text.as_deref(), &colour);
        Bool::YES
    });
    let image = NSImage::imageWithSize_flipped_drawingHandler(
        NSSize::new(layout.width, CANVAS_HEIGHT),
        false,
        &handler,
    );
    image.setTemplate(template);
    (image, template)
}

fn text_run(label: &str, colour: &NSColor) -> Retained<NSAttributedString> {
    // SAFETY: reading an immutable AppKit constant.
    let weight = unsafe { NSFontWeightRegular };
    let font = NSFont::monospacedDigitSystemFontOfSize_weight(TEXT_POINT_SIZE, weight);
    // SAFETY: NSFontAttributeName takes an NSFont and
    // NSForegroundColorAttributeName an NSColor, which is what is passed.
    unsafe {
        let attrs = NSDictionary::from_slices(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[&*font as &AnyObject, colour as &AnyObject],
        );
        NSAttributedString::new_with_attributes(&NSString::from_str(label), &attrs)
    }
}

fn draw(layout: &Layout, text: Option<&NSAttributedString>, colour: &NSColor) {
    let Some(ctx) = NSGraphicsContext::currentContext() else {
        return;
    };
    let cg = ctx.CGContext();
    let cg = Some(&*cg);
    CGContext::set_should_antialias(cg, true);
    colour.setFill();
    colour.setStroke();

    if let Some(x) = layout.glyph_x {
        for (bx, by, bw, bh) in bar_rects(x) {
            let rect = CGRect::new(CGPoint::new(bx, by), CGSize::new(bw, bh));
            let radius = bw / 2.0;
            // SAFETY: a null transform is allowed.
            let path = unsafe { CGPath::with_rounded_rect(rect, radius, radius, std::ptr::null()) };
            CGContext::add_path(cg, Some(&path));
            CGContext::fill_path(cg);
        }
    }
    if let Some(x) = layout.dot_x {
        let y = (CANVAS_HEIGHT - DOT) / 2.0;
        CGContext::fill_ellipse_in_rect(cg, CGRect::new(CGPoint::new(x, y), CGSize::new(DOT, DOT)));
    }
    if let (Some(x), Some(text)) = (layout.text_x, text) {
        let y = (CANVAS_HEIGHT - text.size().height) / 2.0;
        text.drawAtPoint(NSPoint::new(x, y));
    }
    if let Some(x) = layout.lock_x {
        draw_lock(cg, x);
    }
}

/// A small padlock: a filled body and an open-bottomed shackle arc.
fn draw_lock(cg: Option<&CGContext>, x: f64) {
    let bottom = (CANVAS_HEIGHT - LOCK_HEIGHT) / 2.0;
    let body_height = 4.5;
    let body = CGRect::new(
        CGPoint::new(x, bottom),
        CGSize::new(LOCK_WIDTH, body_height),
    );
    // SAFETY: a null transform is allowed.
    let path = unsafe { CGPath::with_rounded_rect(body, 1.0, 1.0, std::ptr::null()) };
    CGContext::add_path(cg, Some(&path));
    CGContext::fill_path(cg);

    let stroke = 1.2;
    let radius = (LOCK_WIDTH - stroke) / 2.0 - 0.6;
    let cx = x + LOCK_WIDTH / 2.0;
    let cy = bottom + body_height;
    CGContext::set_line_width(cg, stroke);
    CGContext::set_line_cap(cg, CGLineCap::Butt);
    CGContext::begin_path(cg);
    CGContext::move_to_point(cg, cx - radius, cy);
    CGContext::add_arc(cg, cx, cy + 0.6, radius, std::f64::consts::PI, 0.0, 1);
    CGContext::add_line_to_point(cg, cx + radius, cy);
    CGContext::stroke_path(cg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_glyph_is_five_bars_sixteen_points_wide() {
        assert_eq!(GLYPH_WIDTH, 16.0);
        let rects = bar_rects(0.0);
        assert_eq!(rects[0], (0.0, 6.0, 2.0, 6.0));
        assert_eq!(rects[2], (7.0, 2.0, 2.0, 14.0));
        assert_eq!(rects[4].0 + rects[4].2, GLYPH_WIDTH);
        // Every bar is centred on the canvas.
        for (_, y, _, h) in rects {
            assert_eq!(y + h / 2.0, CANVAS_HEIGHT / 2.0);
        }
    }

    #[test]
    fn idle_is_just_the_template_glyph() {
        let l = layout(&MenuBarState::Idle, 0.0);
        assert_eq!(l.width, GLYPH_WIDTH);
        assert_eq!(l.text_x, None);
        assert!(is_template(&MenuBarState::Idle));
        assert!(is_template(&MenuBarState::NeedsAttention));
        assert!(is_template(&MenuBarState::Transcribing));
        assert_eq!(label(&MenuBarState::Idle), None);
    }

    #[test]
    fn recording_is_copper_glyph_then_time() {
        let state = MenuBarState::Recording {
            elapsed_secs: 7,
            hands_free: false,
        };
        assert!(!is_template(&state));
        assert_eq!(label(&state).as_deref(), Some("0:07"));
        let l = layout(&state, 24.0);
        assert_eq!(l.glyph_x, Some(0.0));
        assert_eq!(l.text_x, Some(GLYPH_WIDTH + MARK_GAP));
        assert_eq!(l.lock_x, None);
        assert_eq!(l.width, GLYPH_WIDTH + MARK_GAP + 24.0);
    }

    #[test]
    fn hands_free_adds_the_lock_after_the_time() {
        let state = MenuBarState::Recording {
            elapsed_secs: 65,
            hands_free: true,
        };
        assert_eq!(label(&state).as_deref(), Some("1:05"));
        let l = layout(&state, 24.0);
        let text_end = GLYPH_WIDTH + MARK_GAP + 24.0;
        assert_eq!(l.lock_x, Some(text_end + LOCK_GAP));
        assert_eq!(l.width, text_end + LOCK_GAP + LOCK_WIDTH);
    }

    #[test]
    fn a_meeting_is_a_dot_and_the_time() {
        let state = MenuBarState::Meeting { elapsed_secs: 724 };
        assert_eq!(label(&state).as_deref(), Some("12:04"));
        let l = layout(&state, 30.0);
        assert_eq!(l.glyph_x, None);
        assert_eq!(l.dot_x, Some(0.0));
        assert_eq!(l.text_x, Some(DOT + MARK_GAP));
        assert!(!is_template(&state));
    }

    #[test]
    fn copper_follows_the_menu_bar() {
        let (r, _, _) = copper(true);
        assert!((r - 0xbb as f64 / 255.0).abs() < 1e-9);
        let (_, g, _) = copper(false);
        assert!((g - 0x61 as f64 / 255.0).abs() < 1e-9);
    }
}
