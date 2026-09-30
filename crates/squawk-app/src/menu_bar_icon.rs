//! The menu bar item's image: five vertical rounded bars, drawn with
//! CoreGraphics into a template `NSImage`, so AppKit tints it for a light,
//! dark or tinted menu bar like every other menu extra. Nothing else: no
//! text, no colour.
//!
//! At rest the bars are a still waveform, 6/10/14/10/6 pt tall. While
//! recording they follow the mic level; after a recording they ease back to
//! rest; during a meeting they pulse slowly; when squawk needs attention the
//! resting shape is drawn faint. The maths is here and pure ([`Animator`]),
//! so it is tested without AppKit; `status_item.rs` owns the timer.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_app_kit::{NSColor, NSGraphicsContext, NSImage};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGContext, CGPath};
use objc2_foundation::{NSRect, NSSize};

use crate::status_item::MenuBarState;

/// Resting bar heights in points, left to right.
pub const IDLE_BARS: [f64; 5] = [6.0, 10.0, 14.0, 10.0, 6.0];
pub const BAR_WIDTH: f64 = 2.0;
pub const BAR_GAP: f64 = 1.5;
/// The glyph's width: five bars and four gaps. The image is always this
/// wide, so the status item never changes width.
pub const GLYPH_WIDTH: f64 = 5.0 * BAR_WIDTH + 4.0 * BAR_GAP;
/// The image's height. The menu bar is 24 pt; 18 leaves the same air above
/// and below as the system glyphs.
pub const CANVAS_HEIGHT: f64 = 18.0;

/// Live bars never get shorter or taller than this.
pub const LIVE_MIN: f64 = 3.0;
pub const LIVE_MAX: f64 = 16.0;
/// How much of the level each bar takes: tallest in the middle.
pub const SHAPE: [f64; 5] = [0.62, 0.86, 1.0, 0.86, 0.62];
/// Needs attention: the resting shape at this opacity.
pub const ATTENTION_ALPHA: f64 = 0.36;

/// Level smoothing time constants, seconds: quick to rise, slower to fall.
pub const ATTACK: f64 = 0.040;
pub const RELEASE: f64 = 0.150;
/// Mic RMS at which the bars reach full height (about -18 dBFS: ordinary
/// speech at laptop distance).
pub const FULL_SCALE_RMS: f64 = 0.12;
/// Mic RMS below which the bars sit on the floor (about -50 dBFS: a quiet
/// room), so room tone does not twitch them.
pub const NOISE_FLOOR_RMS: f64 = 0.003;

/// After a recording, the bars ease back to rest over this long.
pub const SETTLE_SECS: f64 = 0.25;
/// Meeting pulse: resting heights × (BASE + SWING·sin(2πt / PERIOD)).
pub const PULSE_PERIOD_SECS: f64 = 5.0;
pub const PULSE_BASE: f64 = 0.78;
pub const PULSE_SWING: f64 = 0.1;

/// The still frame for a recording when motion is reduced: a frozen
/// waveform, so it still reads differently from idle.
const RECORDING_STILL_LEVELS: [f64; 5] = [0.42, 0.7, 0.95, 0.62, 0.34];

/// Per-bar jitter: each bar's factor wanders within this range, picking a
/// new target this often and easing toward it with this time constant.
const JITTER_RANGE: (f64, f64) = (0.8, 1.12);
const JITTER_EVERY_SECS: f64 = 1.0 / 12.0;
const JITTER_TAU: f64 = 0.06;

/// Heights are rounded to this (one pixel on a Retina menu bar), so a
/// frame that would look the same is not redrawn.
pub const HEIGHT_STEP: f64 = 0.5;

/// Longest step the animation takes in one frame (a late or first frame
/// must not jump).
const MAX_STEP_SECS: f64 = 0.1;

/// One frame of the glyph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    pub heights: [f64; 5],
    pub alpha: f64,
}

impl Glyph {
    pub const IDLE: Glyph = Glyph {
        heights: IDLE_BARS,
        alpha: 1.0,
    };
}

/// How the glyph moves, and so how often the item redraws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// No timer.
    Still,
    /// Following the mic.
    Live,
    /// Easing back to rest after a recording.
    Settle,
    /// The slow meeting pulse.
    Pulse,
}

impl Motion {
    /// Frames per second, or `None` for no timer.
    pub fn fps(self) -> Option<f64> {
        match self {
            Motion::Still => None,
            Motion::Live => Some(24.0),
            Motion::Settle => Some(30.0),
            Motion::Pulse => Some(8.0),
        }
    }
}

/// Mic RMS to a 0..1 level: gated below the room's floor, full at
/// [`FULL_SCALE_RMS`].
pub fn normalize_rms(rms: f32) -> f64 {
    let rms = rms as f64;
    if !rms.is_finite() {
        return 0.0;
    }
    ((rms - NOISE_FLOOR_RMS) / (FULL_SCALE_RMS - NOISE_FLOOR_RMS)).clamp(0.0, 1.0)
}

/// One-pole smoothing with separate attack and release.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LevelSmoother {
    pub value: f64,
}

impl LevelSmoother {
    /// Move toward `target` over `dt` seconds; returns the new value.
    pub fn step(&mut self, target: f64, dt: f64) -> f64 {
        let tau = if target > self.value { ATTACK } else { RELEASE };
        let k = 1.0 - (-dt.max(0.0) / tau).exp();
        self.value += (target - self.value) * k;
        self.value
    }
}

/// Small per-bar wobble so the five bars do not move in lockstep.
#[derive(Debug, Clone)]
pub struct Jitter {
    rng: u64,
    current: [f64; 5],
    target: [f64; 5],
    until_next: f64,
}

impl Jitter {
    pub fn new(seed: u64) -> Jitter {
        Jitter {
            rng: seed.max(1),
            current: [1.0; 5],
            target: [1.0; 5],
            until_next: 0.0,
        }
    }

    /// Advance by `dt` seconds; returns each bar's factor.
    pub fn step(&mut self, dt: f64) -> [f64; 5] {
        self.until_next -= dt;
        if self.until_next <= 0.0 {
            self.until_next = JITTER_EVERY_SECS;
            for i in 0..5 {
                let u = self.next_unit();
                self.target[i] = JITTER_RANGE.0 + (JITTER_RANGE.1 - JITTER_RANGE.0) * u;
            }
        }
        let k = 1.0 - (-dt.max(0.0) / JITTER_TAU).exp();
        for i in 0..5 {
            self.current[i] += (self.target[i] - self.current[i]) * k;
        }
        self.current
    }

    /// xorshift64, as a 0..1 float.
    fn next_unit(&mut self) -> f64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        (x >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Bar heights for a smoothed 0..1 level. The square root lifts quiet
/// speech so it still moves the bars.
pub fn live_heights(level: f64, jitter: [f64; 5]) -> [f64; 5] {
    let loud = level.max(0.0).sqrt();
    let mut heights = [0.0; 5];
    for i in 0..5 {
        let v = (loud * SHAPE[i] * jitter[i]).clamp(0.0, 1.0);
        heights[i] = LIVE_MIN + (LIVE_MAX - LIVE_MIN) * v;
    }
    heights
}

/// Round heights to [`HEIGHT_STEP`].
pub fn snap(heights: [f64; 5]) -> [f64; 5] {
    heights.map(|h| (h / HEIGHT_STEP).round() * HEIGHT_STEP)
}

/// Cubic ease-out on 0..1.
pub fn ease_out_cubic(k: f64) -> f64 {
    1.0 - (1.0 - k.clamp(0.0, 1.0)).powi(3)
}

/// `elapsed` seconds into the settle from `from` back to the resting shape.
pub fn settle_heights(from: [f64; 5], elapsed: f64) -> [f64; 5] {
    let k = ease_out_cubic(elapsed / SETTLE_SECS);
    let mut heights = [0.0; 5];
    for i in 0..5 {
        heights[i] = from[i] + (IDLE_BARS[i] - from[i]) * k;
    }
    heights
}

/// The meeting pulse, `t` seconds in.
pub fn meeting_heights(t: f64) -> [f64; 5] {
    let scale = PULSE_BASE + PULSE_SWING * (std::f64::consts::TAU * t / PULSE_PERIOD_SECS).sin();
    IDLE_BARS.map(|h| h * scale)
}

/// The glyph for `state` when nothing may move.
pub fn still_glyph(state: &MenuBarState) -> Glyph {
    match state {
        MenuBarState::Idle | MenuBarState::Transcribing => Glyph::IDLE,
        MenuBarState::NeedsAttention => Glyph {
            heights: IDLE_BARS,
            alpha: ATTENTION_ALPHA,
        },
        MenuBarState::Recording => Glyph {
            heights: snap(RECORDING_STILL_LEVELS.map(|v| LIVE_MIN + (LIVE_MAX - LIVE_MIN) * v)),
            alpha: 1.0,
        },
        MenuBarState::Meeting => Glyph {
            heights: snap(IDLE_BARS.map(|h| h * PULSE_BASE)),
            alpha: 1.0,
        },
    }
}

/// The glyph's animation state. Times are seconds on the caller's clock.
#[derive(Debug, Clone)]
pub struct Animator {
    state: MenuBarState,
    reduce_motion: bool,
    smoother: LevelSmoother,
    jitter: Jitter,
    /// The last frame drawn.
    last: Glyph,
    last_t: f64,
    /// When the current state was entered (the pulse's zero).
    since: f64,
    /// A settle in progress: when it started and the heights it left from.
    settle: Option<(f64, [f64; 5])>,
}

impl Animator {
    pub fn new(state: MenuBarState, now: f64, reduce_motion: bool) -> Animator {
        let mut anim = Animator {
            state: state.clone(),
            reduce_motion,
            smoother: LevelSmoother::default(),
            jitter: Jitter::new(0x9e37_79b9_7f4a_7c15),
            last: Glyph::IDLE,
            last_t: now,
            since: now,
            settle: None,
        };
        anim.last = anim.frame(now, 0.0);
        anim
    }

    pub fn state(&self) -> &MenuBarState {
        &self.state
    }

    /// Enter `state` at `now`. Leaving a recording for transcribing or idle
    /// starts the settle from the bars as last drawn; a settle already under
    /// way carries on into idle, since it ends on the resting shape anyway.
    pub fn set_state(&mut self, state: MenuBarState, now: f64, reduce_motion: bool) {
        if state == self.state && reduce_motion == self.reduce_motion {
            return;
        }
        let rests = matches!(state, MenuBarState::Idle | MenuBarState::Transcribing);
        if self.state == MenuBarState::Recording && rests && !reduce_motion {
            self.settle = Some((now, self.last.heights));
        } else if !rests || reduce_motion {
            self.settle = None;
        }
        if state == MenuBarState::Recording && self.state != MenuBarState::Recording {
            self.smoother = LevelSmoother::default();
        }
        if state != self.state {
            self.since = now;
        }
        self.state = state;
        self.reduce_motion = reduce_motion;
        self.last_t = now;
    }

    /// How the glyph moves at `now`.
    pub fn motion(&self, now: f64) -> Motion {
        if self.reduce_motion {
            return Motion::Still;
        }
        match self.state {
            MenuBarState::Recording => Motion::Live,
            MenuBarState::Meeting => Motion::Pulse,
            MenuBarState::Idle | MenuBarState::Transcribing => match self.settle {
                Some((start, _)) if now - start < SETTLE_SECS => Motion::Settle,
                _ => Motion::Still,
            },
            MenuBarState::NeedsAttention => Motion::Still,
        }
    }

    /// Advance to `now`, with the mic's latest RMS, and return the frame
    /// (heights snapped to [`HEIGHT_STEP`]).
    pub fn frame(&mut self, now: f64, rms: f32) -> Glyph {
        let dt = (now - self.last_t).clamp(0.0, MAX_STEP_SECS);
        self.last_t = now;
        let glyph = match self.motion(now) {
            Motion::Live => {
                let level = self.smoother.step(normalize_rms(rms), dt);
                let jitter = self.jitter.step(dt);
                Glyph {
                    heights: live_heights(level, jitter),
                    alpha: 1.0,
                }
            }
            Motion::Settle => {
                let (start, from) = self.settle.expect("settling");
                Glyph {
                    heights: settle_heights(from, now - start),
                    alpha: 1.0,
                }
            }
            Motion::Pulse => Glyph {
                heights: meeting_heights(now - self.since),
                alpha: 1.0,
            },
            Motion::Still => {
                self.settle = None;
                still_glyph(&self.state)
            }
        };
        let glyph = Glyph {
            heights: snap(glyph.heights),
            ..glyph
        };
        self.last = glyph;
        glyph
    }
}

/// The bars as (x, y, width, height) in the image's bottom-left-origin
/// space, each vertically centred on the canvas.
pub fn bar_rects(heights: [f64; 5], origin_x: f64) -> [(f64, f64, f64, f64); 5] {
    let mut rects = [(0.0, 0.0, 0.0, 0.0); 5];
    for (i, height) in heights.iter().enumerate() {
        let height = height.clamp(BAR_WIDTH, CANVAS_HEIGHT);
        let x = origin_x + i as f64 * (BAR_WIDTH + BAR_GAP);
        let y = (CANVAS_HEIGHT - height) / 2.0;
        rects[i] = (x, y, BAR_WIDTH, height);
    }
    rects
}

/// The status item's image for `glyph`: a template, so AppKit picks the ink.
pub fn glyph_image(glyph: Glyph) -> Retained<NSImage> {
    let handler = RcBlock::new(move |_dirty: NSRect| -> Bool {
        let black = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, glyph.alpha);
        draw_bars(glyph.heights, 0.0, &black);
        Bool::YES
    });
    let image = NSImage::imageWithSize_flipped_drawingHandler(
        NSSize::new(GLYPH_WIDTH, CANVAS_HEIGHT),
        false,
        &handler,
    );
    image.setTemplate(true);
    image
}

/// Fill the five bars into the current `NSGraphicsContext` (bottom-left
/// origin, canvas [`CANVAS_HEIGHT`] tall), in `colour`.
pub fn draw_bars(heights: [f64; 5], origin_x: f64, colour: &NSColor) {
    let Some(ctx) = NSGraphicsContext::currentContext() else {
        return;
    };
    let cg = ctx.CGContext();
    let cg = Some(&*cg);
    CGContext::set_should_antialias(cg, true);
    colour.setFill();
    for (bx, by, bw, bh) in bar_rects(heights, origin_x) {
        let rect = CGRect::new(CGPoint::new(bx, by), CGSize::new(bw, bh));
        let radius = bw / 2.0;
        // SAFETY: a null transform is allowed.
        let path = unsafe { CGPath::with_rounded_rect(rect, radius, radius, std::ptr::null()) };
        CGContext::add_path(cg, Some(&path));
        CGContext::fill_path(cg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn the_glyph_is_five_bars_sixteen_points_wide() {
        assert_eq!(GLYPH_WIDTH, 16.0);
        let rects = bar_rects(IDLE_BARS, 0.0);
        assert_eq!(rects[0], (0.0, 6.0, 2.0, 6.0));
        assert_eq!(rects[2], (7.0, 2.0, 2.0, 14.0));
        assert_eq!(rects[4].0 + rects[4].2, GLYPH_WIDTH);
        for (_, y, _, h) in rects {
            assert_eq!(y + h / 2.0, CANVAS_HEIGHT / 2.0);
        }
        // Whatever the level, the bars stay inside the canvas.
        for (_, y, _, h) in bar_rects([0.0, 40.0, LIVE_MAX, 1.0, -3.0], 0.0) {
            assert!(y >= 0.0 && y + h <= CANVAS_HEIGHT);
        }
    }

    #[test]
    fn heights_snap_to_retina_pixels() {
        assert_eq!(
            snap([3.0, 8.46, 12.32, 11.74, 6.2]),
            [3.0, 8.5, 12.5, 11.5, 6.0]
        );
    }

    #[test]
    fn rms_is_gated_and_capped() {
        assert_eq!(normalize_rms(0.0), 0.0);
        assert_eq!(normalize_rms(0.002), 0.0);
        assert_eq!(normalize_rms(0.5), 1.0);
        assert_eq!(normalize_rms(f32::NAN), 0.0);
        let mid = normalize_rms(0.03);
        assert!(mid > 0.2 && mid < 0.3, "{mid}");
    }

    #[test]
    fn the_level_rises_fast_and_falls_slowly() {
        let mut s = LevelSmoother::default();
        // One 40 ms step up covers ~63% (one time constant).
        let up = s.step(1.0, ATTACK);
        assert!(close(up, 1.0 - (-1.0f64).exp()));
        // ~150 ms of rise gets close to the target.
        for _ in 0..3 {
            s.step(1.0, 1.0 / 24.0);
        }
        assert!(s.value > 0.97, "{}", s.value);
        // Falling, one 40 ms step loses far less than a rise gains.
        let before = s.value;
        let down = s.step(0.0, ATTACK);
        assert!(before - down < 0.3 * before, "{before} -> {down}");
        // And a release time constant takes ~63% off.
        let mut s = LevelSmoother { value: 1.0 };
        assert!(close(s.step(0.0, RELEASE), (-1.0f64).exp()));
        // No time, no change.
        assert_eq!(s.step(1.0, 0.0), s.value);
    }

    #[test]
    fn heights_map_level_to_three_through_sixteen_points() {
        let flat = [1.0; 5];
        assert_eq!(live_heights(0.0, flat), [LIVE_MIN; 5]);
        let full = live_heights(1.0, flat);
        assert!(close(full[2], LIVE_MAX));
        assert!(close(full[0], LIVE_MIN + (LIVE_MAX - LIVE_MIN) * 0.62));
        assert!(full[0] < full[1] && full[1] < full[2]);
        assert_eq!(full[0], full[4]);
        // Square root: a quarter level draws the middle bar half way up.
        assert!(close(
            live_heights(0.25, flat)[2],
            LIVE_MIN + (LIVE_MAX - LIVE_MIN) * 0.5
        ));
        // Jitter above 1 never pushes a bar past the ceiling.
        for h in live_heights(1.0, [1.12; 5]) {
            assert!((LIVE_MIN..=LIVE_MAX).contains(&h));
        }
    }

    #[test]
    fn jitter_stays_small_and_varies_per_bar() {
        let mut j = Jitter::new(7);
        let mut seen_different = false;
        for _ in 0..240 {
            let f = j.step(1.0 / 24.0);
            for v in f {
                assert!((JITTER_RANGE.0..=JITTER_RANGE.1).contains(&v), "{v}");
            }
            seen_different |= f.iter().any(|v| !close(*v, f[0]));
        }
        assert!(seen_different);
    }

    #[test]
    fn the_settle_eases_out_to_the_resting_shape() {
        let from = [3.0, 12.0, 16.0, 9.0, 4.0];
        assert_eq!(settle_heights(from, 0.0), from);
        assert_eq!(settle_heights(from, SETTLE_SECS), IDLE_BARS);
        assert_eq!(settle_heights(from, 5.0), IDLE_BARS);
        // Ease-out: more than half way at the midpoint.
        let mid = settle_heights(from, SETTLE_SECS / 2.0);
        assert!(close(mid[0], 3.0 + 3.0 * 0.875));
        assert!(close(ease_out_cubic(0.5), 0.875));
    }

    #[test]
    fn the_meeting_pulse_is_slow_and_small() {
        let base = meeting_heights(0.0);
        assert!(close(base[2], 14.0 * PULSE_BASE));
        let peak = meeting_heights(PULSE_PERIOD_SECS / 4.0);
        assert!(close(peak[2], 14.0 * (PULSE_BASE + PULSE_SWING)));
        let trough = meeting_heights(PULSE_PERIOD_SECS * 0.75);
        assert!(close(trough[2], 14.0 * (PULSE_BASE - PULSE_SWING)));
        assert!(close(meeting_heights(PULSE_PERIOD_SECS)[2], base[2]));
        // Never taller than the resting glyph.
        assert!(peak.iter().zip(IDLE_BARS).all(|(p, i)| *p < i));
    }

    #[test]
    fn idle_and_attention_are_still() {
        let a = Animator::new(MenuBarState::Idle, 0.0, false);
        assert_eq!(a.motion(0.0), Motion::Still);
        assert_eq!(Motion::Still.fps(), None);
        let mut a = Animator::new(MenuBarState::NeedsAttention, 0.0, false);
        assert_eq!(a.motion(1.0), Motion::Still);
        let g = a.frame(1.0, 0.5);
        assert_eq!(g.heights, IDLE_BARS);
        assert!(close(g.alpha, ATTENTION_ALPHA));
    }

    #[test]
    fn recording_follows_the_mic_then_settles_to_rest() {
        let mut a = Animator::new(MenuBarState::Idle, 0.0, false);
        a.set_state(MenuBarState::Recording, 1.0, false);
        assert_eq!(a.motion(1.0), Motion::Live);
        assert_eq!(Motion::Live.fps(), Some(24.0));
        let quiet = a.frame(1.0, 0.0);
        assert_eq!(quiet.heights, [LIVE_MIN; 5]);
        let mut t = 1.0;
        let mut loud = quiet;
        for _ in 0..12 {
            t += 1.0 / 24.0;
            loud = a.frame(t, 0.1);
        }
        assert!(loud.heights[2] > 12.0, "{:?}", loud.heights);

        a.set_state(MenuBarState::Transcribing, t, false);
        assert_eq!(a.motion(t), Motion::Settle);
        assert_eq!(a.frame(t, 0.0).heights, loud.heights);
        // Idle arriving mid-settle does not cut it short.
        a.set_state(MenuBarState::Idle, t + 0.1, false);
        assert_eq!(a.motion(t + 0.1), Motion::Settle);
        let mid = a.frame(t + 0.1, 0.0);
        assert!(mid.heights != IDLE_BARS && mid.heights != loud.heights);
        assert_eq!(a.motion(t + SETTLE_SECS), Motion::Still);
        assert_eq!(a.frame(t + SETTLE_SECS, 0.0), Glyph::IDLE);
    }

    #[test]
    fn a_meeting_pulses_and_attention_cuts_a_settle() {
        let mut a = Animator::new(MenuBarState::Meeting, 10.0, false);
        assert_eq!(a.motion(10.0), Motion::Pulse);
        assert_eq!(Motion::Pulse.fps(), Some(8.0));
        let g = a.frame(10.0 + PULSE_PERIOD_SECS / 4.0, 0.0);
        assert_eq!(g.heights[2], 12.5); // 14 × 0.88 = 12.32, snapped
                                        // Snapping means many pulse frames repeat the last one.
        let frames: Vec<Glyph> = (0..40)
            .map(|k| a.frame(10.0 + k as f64 / 8.0, 0.0))
            .collect();
        let changes = frames.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(changes < 30, "{changes}");

        a.set_state(MenuBarState::Recording, 20.0, false);
        a.set_state(MenuBarState::NeedsAttention, 20.1, false);
        assert_eq!(a.motion(20.1), Motion::Still);
    }

    #[test]
    fn reduced_motion_draws_a_still_glyph_per_state() {
        for state in [
            MenuBarState::Idle,
            MenuBarState::NeedsAttention,
            MenuBarState::Recording,
            MenuBarState::Transcribing,
            MenuBarState::Meeting,
        ] {
            let mut a = Animator::new(state.clone(), 0.0, true);
            assert_eq!(a.motion(0.0), Motion::Still, "{state:?}");
            assert_eq!(a.frame(1.0, 0.2), still_glyph(&state));
        }
        // Recording's still frame is not the idle glyph.
        assert_ne!(still_glyph(&MenuBarState::Recording), Glyph::IDLE);
        // No settle either.
        let mut a = Animator::new(MenuBarState::Recording, 0.0, true);
        a.set_state(MenuBarState::Transcribing, 1.0, true);
        assert_eq!(a.motion(1.0), Motion::Still);
    }
}
