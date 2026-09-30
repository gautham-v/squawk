//! Dev preview of the menu bar glyph.
//!
//! ```text
//! cargo run -p squawk-app --example menu_bar_preview -- [seconds] [--only STATE]
//! cargo run -p squawk-app --example menu_bar_preview -- --png DIR
//! ```
//!
//! Without `--png`, puts one status item per state in the real menu bar for
//! `seconds` (default 8): idle, needs attention, recording (fed by a
//! synthetic voice), a loop of recording → transcribing → idle, and a
//! meeting. `--only idle|attention|recording|transcribing|meeting` shows just
//! that one, for measuring its CPU cost.
//!
//! With `--png DIR`, writes strips of frames (6× zoom) for each state on a
//! light and a dark menu bar, drawn by the same code as the status item.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{App, Application};
use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSColor, NSDeviceRGBColorSpace, NSGraphicsContext,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGContext;
use objc2_foundation::{NSDictionary, NSSize};
use squawk_app::menu_bar_icon::{
    self, still_glyph, Animator, Glyph, CANVAS_HEIGHT, FULL_SCALE_RMS, GLYPH_WIDTH,
    NOISE_FLOOR_RMS, PULSE_PERIOD_SECS,
};
use squawk_app::status_item::{MenuBarState, StatusItem};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(dir) = flag(&args, "--png") {
        write_pngs(Path::new(&dir));
        return;
    }
    let secs: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(8);
    let only = flag(&args, "--only");
    Application::new().run(move |cx: &mut App| {
        let mtm = MainThreadMarker::new().expect("main thread");
        squawk_app::status_item::set_accessory_activation_policy(mtm);
        let wanted = |name: &str| only.as_deref().is_none_or(|o| o == name);
        let mut items: Vec<Rc<StatusItem>> = Vec::new();
        // Right to left in the menu bar: the first item created sits
        // rightmost.
        if wanted("idle") {
            items.push(Rc::new(StatusItem::new(mtm, MenuBarState::Idle, || 0.0).0));
        }
        if wanted("attention") {
            items.push(Rc::new(
                StatusItem::new(mtm, MenuBarState::NeedsAttention, || 0.0).0,
            ));
        }
        if wanted("recording") {
            items.push(Rc::new(
                StatusItem::new(mtm, MenuBarState::Recording, live_voice()).0,
            ));
        }
        if wanted("meeting") {
            items.push(Rc::new(
                StatusItem::new(mtm, MenuBarState::Meeting, || 0.0).0,
            ));
        }
        if wanted("transcribing") {
            let item = Rc::new(StatusItem::new(mtm, MenuBarState::Recording, live_voice()).0);
            let looped = Rc::downgrade(&item);
            items.push(item);
            cx.spawn(async move |cx| loop {
                for (state, ms) in [
                    (MenuBarState::Recording, 1600),
                    (MenuBarState::Transcribing, 250),
                    (MenuBarState::Idle, 1200),
                ] {
                    let Some(item) = looped.upgrade() else {
                        return;
                    };
                    item.set_state(mtm, state);
                    drop(item);
                    cx.background_executor()
                        .timer(Duration::from_millis(ms))
                        .await;
                }
            })
            .detach();
        }
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

fn flag(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

/// A speech-like mic RMS: phrases and pauses, syllables inside phrases.
struct Voice {
    rng: u64,
    in_phrase: bool,
    phrase_left: f64,
    syllable_left: f64,
    syllable_len: f64,
    syllable_amp: f64,
}

impl Voice {
    fn new() -> Voice {
        Voice {
            rng: 0x2545_f491_4f6c_dd1d,
            in_phrase: true,
            phrase_left: 1.2,
            syllable_left: 0.0,
            syllable_len: 0.2,
            syllable_amp: 0.8,
        }
    }

    fn rand(&mut self, lo: f64, hi: f64) -> f64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        lo + (hi - lo) * ((x >> 11) as f64 / (1u64 << 53) as f64)
    }

    /// Advance `dt` seconds; returns the RMS a mic block would have.
    fn step(&mut self, dt: f64) -> f32 {
        self.phrase_left -= dt;
        if self.phrase_left <= 0.0 {
            self.in_phrase = !self.in_phrase;
            self.phrase_left = if self.in_phrase {
                self.rand(0.6, 2.4)
            } else {
                self.rand(0.2, 0.7)
            };
        }
        self.syllable_left -= dt;
        if self.syllable_left <= 0.0 {
            self.syllable_len = self.rand(0.11, 0.26);
            self.syllable_left = self.syllable_len;
            self.syllable_amp = self.rand(0.4, 1.0);
        }
        let env = (std::f64::consts::PI * (1.0 - self.syllable_left / self.syllable_len)).sin();
        let level = if self.in_phrase {
            self.syllable_amp * (0.35 + 0.65 * env)
        } else {
            0.0
        };
        let rms = NOISE_FLOOR_RMS * 0.5 + level * (FULL_SCALE_RMS - NOISE_FLOOR_RMS);
        rms as f32
    }
}

/// A level source for a status item, driven by the wall clock.
fn live_voice() -> impl Fn() -> f32 + 'static {
    let state = RefCell::new((Voice::new(), Instant::now()));
    move || {
        let mut s = state.borrow_mut();
        let dt = s.1.elapsed().as_secs_f64();
        s.1 = Instant::now();
        s.0.step(dt)
    }
}

// ---- PNG strips ----

const ZOOM: f64 = 6.0;
/// One cell: the glyph with menu bar padding around it.
const CELL_W: f64 = GLYPH_WIDTH + 12.0;
const CELL_H: f64 = 24.0;

struct Bar {
    name: &'static str,
    background: (f64, f64, f64),
    /// How AppKit tints a template image on this bar.
    ink: (f64, f64, f64, f64),
}

const BARS: [Bar; 2] = [
    Bar {
        name: "light",
        background: (0.90, 0.91, 0.92),
        ink: (0.0, 0.0, 0.0, 0.85),
    },
    Bar {
        name: "dark",
        background: (0.13, 0.15, 0.18),
        ink: (1.0, 1.0, 1.0, 0.92),
    },
];

fn write_pngs(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create the output dir");
    let rows = frames();
    for bar in &BARS {
        for (name, frames) in &rows {
            let path = dir.join(format!("{}-{name}.png", bar.name));
            write_strip(&path, bar, frames);
            println!("{}", path.display());
        }
    }
}

/// Every state's frames, as the status item would draw them.
fn frames() -> Vec<(&'static str, Vec<Glyph>)> {
    let fps = 24.0;
    let mut voice = Voice::new();
    let mut anim = Animator::new(MenuBarState::Idle, 0.0, false);
    anim.set_state(MenuBarState::Recording, 0.0, false);
    // 3 s of talking at 24 fps; keep every 4th frame.
    let mut recording = Vec::new();
    let mut t = 0.0;
    for i in 0..72 {
        t = i as f64 / fps;
        let g = anim.frame(t, voice.step(1.0 / fps));
        if i % 4 == 3 {
            recording.push(g);
        }
    }
    // Release: the settle at 30 fps from the last live frame.
    anim.set_state(MenuBarState::Transcribing, t, false);
    let settle: Vec<Glyph> = (0..=8)
        .map(|k| anim.frame(t + k as f64 / 30.0, 0.0))
        .collect();
    let mut meeting_anim = Animator::new(MenuBarState::Meeting, 0.0, false);
    let meeting: Vec<Glyph> = (0..10)
        .map(|k| meeting_anim.frame(k as f64 * PULSE_PERIOD_SECS / 10.0, 0.0))
        .collect();
    let reduced: Vec<Glyph> = [
        MenuBarState::Idle,
        MenuBarState::NeedsAttention,
        MenuBarState::Recording,
        MenuBarState::Transcribing,
        MenuBarState::Meeting,
    ]
    .iter()
    .map(still_glyph)
    .collect();
    vec![
        ("idle", vec![Glyph::IDLE]),
        (
            "attention",
            vec![still_glyph(&MenuBarState::NeedsAttention)],
        ),
        ("recording", recording),
        ("settle", settle),
        ("meeting", meeting),
        ("reduced-motion", reduced),
    ]
}

fn write_strip(path: &Path, bar: &Bar, frames: &[Glyph]) {
    let width = CELL_W * frames.len() as f64;
    let (pw, ph) = ((width * ZOOM) as isize, (CELL_H * ZOOM) as isize);
    // SAFETY: null planes ask AppKit to allocate; the sizes are consistent
    // (8-bit RGBA, meshed, rows computed by AppKit).
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            pw,
            ph,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )
    }
    .expect("bitmap");
    rep.setSize(NSSize::new(width, CELL_H));
    let ctx: Retained<NSGraphicsContext> =
        NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep).expect("context");
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ctx));
    let cg = ctx.CGContext();
    let cg = Some(&*cg);
    let (r, g, b) = bar.background;
    CGContext::set_rgb_fill_color(cg, r, g, b, 1.0);
    CGContext::fill_rect(
        cg,
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width, CELL_H)),
    );
    let (ir, ig, ib, ia) = bar.ink;
    for (i, glyph) in frames.iter().enumerate() {
        let ink = NSColor::colorWithSRGBRed_green_blue_alpha(ir, ig, ib, ia * glyph.alpha);
        CGContext::save_g_state(cg);
        CGContext::translate_ctm(
            cg,
            i as f64 * CELL_W + (CELL_W - GLYPH_WIDTH) / 2.0,
            (CELL_H - CANVAS_HEIGHT) / 2.0,
        );
        menu_bar_icon::draw_bars(glyph.heights, 0.0, &ink);
        CGContext::restore_g_state(cg);
    }
    ctx.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();
    // SAFETY: an empty properties dictionary is valid for PNG.
    let data = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
    .expect("png");
    std::fs::write(PathBuf::from(path), data.to_vec()).expect("write png");
}
