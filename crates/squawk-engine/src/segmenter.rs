//! Cutting a live stream at pauses. Pure: samples in, segments out, so the
//! cut rules are unit-tested without a mic.
//!
//! Frames of 20 ms are classed as speech or silence by RMS against an
//! adaptive threshold: a noise floor × 3, never below `min_speech_rms`. The
//! floor is the quieter of two estimates: the quietest frame of the last
//! 4 s (so talking does not lift it: there is always a gap between words
//! in there), and a slow tracker that drops at once to any quieter frame
//! and rises with a ~40 s time constant (so it starts sane even when the
//! stream opens mid-word, and a steadily noisy room stops counting as
//! speech within seconds).
//!
//! Once at least `min_segment` of audio is buffered, a run of silence long
//! enough is a cut point: everything up to its quietest frame (the middle,
//! if it is evenly quiet) is committed. "Long enough" starts at `pause` and
//! shrinks by 80 ms per second buffered beyond `min_segment`, down to
//! `min_pause`: early on only a real pause cuts, but a long unbroken stretch
//! is cut at the next gap between words, which keeps the tail short. If
//! `max_segment` is reached with no pause at all, cut after the quietest
//! frame in its last 2 s. What is left at `finish` is the tail.
//!
//! A committed segment without speech (fewer than [`MIN_SPEECH_FRAMES`]
//! speech frames) is dropped by `push` rather than returned, which also
//! keeps Parakeet from hallucinating on empty input.
//!
//! Speculation: once speech is followed by a short silence the user may be
//! done, so [`Segmenter::speculate`] hands out a copy of everything pending
//! to transcribe early. If no speech follows before `finish`, that result
//! covers the whole tail and nothing is left to do after fn comes up.

/// Tunables. The defaults are what dictation uses; meetings use
/// [`SegmenterConfig::for_meeting`].
///
/// Dictation segments are long on purpose: Parakeet hears each one alone,
/// and with 3–10 s pieces cut at gaps between words it lost context and
/// cuts landed inside phrases ("no wait" → "no eight", "three thirty PM" →
/// "three thirty two. PM"). 8–20 s pieces cut only at a real pause cost
/// nothing on short dictations (one piece either way) and little on long
/// ones: the tail is usually transcribed early by speculation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmenterConfig {
    pub sample_rate: u32,
    /// Do not cut before this much audio (seconds).
    pub min_segment: f32,
    /// Always cut by this much (seconds).
    pub max_segment: f32,
    /// A silence this long is a cut point right after `min_segment`
    /// (seconds).
    pub pause: f32,
    /// The shortest silence that is ever a cut point (seconds).
    pub min_pause: f32,
    /// Floor for the speech threshold (RMS, 0..1).
    pub min_speech_rms: f32,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        SegmenterConfig {
            sample_rate: crate::SAMPLE_RATE,
            min_segment: 8.0,
            max_segment: 20.0,
            pause: 0.5,
            min_pause: 0.3,
            min_speech_rms: 0.004,
        }
    }
}

impl SegmenterConfig {
    /// Meeting chunks: about `chunk_secs` long, cut at a pause within ±5 s.
    pub fn for_meeting(chunk_secs: u64) -> SegmenterConfig {
        let chunk = chunk_secs.max(5) as f32;
        SegmenterConfig {
            min_segment: (chunk - 5.0).max(2.0),
            max_segment: chunk + 5.0,
            pause: 0.5,
            min_pause: 0.25,
            ..SegmenterConfig::default()
        }
    }

    /// The silence needed to cut with `buffered` seconds pending.
    pub fn pause_needed(&self, buffered: f32) -> f32 {
        let over = (buffered - self.min_segment).max(0.0);
        (self.pause - over * PAUSE_SHRINK_PER_SEC).max(self.min_pause.min(self.pause))
    }
}

/// Frame length for the speech/silence decision.
pub const FRAME_MS: usize = 20;
/// A segment needs this many speech frames (60 ms) to count as speech, so a
/// lone click or breath does not reach the model.
pub const MIN_SPEECH_FRAMES: usize = 3;
/// How much shorter a pause may be per second buffered past `min_segment`.
const PAUSE_SHRINK_PER_SEC: f32 = 0.08;
/// How far back a forced cut looks for the quietest frame.
const FORCED_CUT_WINDOW_SECS: f32 = 2.0;
/// Per-frame rise rate of the noise floor (~40 s time constant at 50 fps):
/// slow enough that seconds of unbroken speech do not become "silence",
/// fast enough that a steadily noisy room stops counting as speech within
/// ~6 s.
const FLOOR_RISE: f32 = 0.0005;
const THRESHOLD_OVER_FLOOR: f32 = 3.0;
/// The window for the quietest-recent-frame floor.
const FLOOR_WINDOW_SECS: f32 = 4.0;

/// A committed piece of audio and where it starts in the stream.
#[derive(Debug, Clone, PartialEq)]
pub struct Cut {
    /// Offset of the first sample from the stream start, in samples.
    pub start: usize,
    pub samples: Vec<f32>,
    /// Whether it held speech (at least [`MIN_SPEECH_FRAMES`] speech frames).
    pub has_speech: bool,
    /// The last [`Speculation`] started where this cut starts and no
    /// speech came after it: its transcript is this cut's (the rest is
    /// silence), so the cut need not be transcribed again.
    pub speculated: bool,
}

/// Identifies a speculative snapshot: the pending audio from `start`,
/// `len` samples long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Speculation {
    pub start: usize,
    pub len: usize,
}

#[derive(Debug, Clone, Copy)]
struct Frame {
    rms: f32,
    speech: bool,
}

pub struct Segmenter {
    config: SegmenterConfig,
    frame_len: usize,
    /// Uncommitted samples; `pending[0]` is stream sample `pending_start`.
    pending: Vec<f32>,
    pending_start: usize,
    /// One entry per complete frame of `pending` (cuts land on frame
    /// boundaries, so frames stay aligned).
    frames: Vec<Frame>,
    /// Speech frames in `frames`.
    speech_frames: usize,
    /// Trailing silence frames at the end of `frames`.
    silence_run: usize,
    floor: f32,
    /// Monotonic queue of (frame number, rms) for the sliding minimum.
    recent_min: std::collections::VecDeque<(usize, f32)>,
    frames_seen: usize,
    /// The last snapshot handed out, while it still covers all pending
    /// speech (cleared by new speech or a cut).
    speculation: Option<Speculation>,
}

impl Segmenter {
    pub fn new(config: SegmenterConfig) -> Segmenter {
        let frame_len = (config.sample_rate as usize * FRAME_MS / 1000).max(1);
        Segmenter {
            config,
            frame_len,
            pending: Vec::new(),
            pending_start: 0,
            frames: Vec::new(),
            speech_frames: 0,
            silence_run: 0,
            // Start where the threshold is exactly `min_speech_rms`; the
            // first quiet frame pulls it down to the room's level.
            floor: config.min_speech_rms / THRESHOLD_OVER_FLOOR,
            recent_min: std::collections::VecDeque::new(),
            frames_seen: 0,
            speculation: None,
        }
    }

    pub fn config(&self) -> &SegmenterConfig {
        &self.config
    }

    /// Samples seen so far, committed or not.
    pub fn total_samples(&self) -> usize {
        self.pending_start + self.pending.len()
    }

    /// Feed samples; returns any segments with speech that became final.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Cut> {
        self.pending.extend_from_slice(samples);
        let mut out = Vec::new();
        while self.pending.len() >= (self.frames.len() + 1) * self.frame_len {
            let at = self.frames.len() * self.frame_len;
            let rms = crate::audio::rms(&self.pending[at..at + self.frame_len]);
            self.add_frame(rms);
            if let Some(k) = self.cut_point() {
                let cut = self.cut(k);
                if cut.has_speech {
                    out.push(cut);
                }
            }
        }
        out
    }

    /// A copy of all pending audio to transcribe ahead of time, when speech
    /// has been followed by at least `after_silence` seconds of silence and
    /// no snapshot has been handed out since that speech. `None` otherwise.
    pub fn speculate(&mut self, after_silence: f32) -> Option<(Speculation, Vec<f32>)> {
        let fps = self.config.sample_rate as f32 / self.frame_len as f32;
        let quiet_enough = self.silence_run as f32 / fps >= after_silence;
        if self.speculation.is_some() || !quiet_enough || self.speech_frames < MIN_SPEECH_FRAMES {
            return None;
        }
        let spec = Speculation {
            start: self.pending_start,
            len: self.pending.len(),
        };
        self.speculation = Some(spec);
        Some((spec, self.pending.clone()))
    }

    /// The last snapshot, if it still covers every bit of pending speech.
    pub fn speculation(&self) -> Option<Speculation> {
        self.speculation
    }

    /// Everything not yet committed, `None` if nothing is. May be silent:
    /// check `has_speech`. Afterwards [`Segmenter::speculation`] says
    /// whether the last snapshot still covers it; the segmenter is empty.
    pub fn take_tail(&mut self) -> Option<Cut> {
        if self.pending.is_empty() {
            return None;
        }
        let whole = self.frames.len() * self.frame_len;
        if self.pending.len() > whole {
            let rms = crate::audio::rms(&self.pending[whole..]);
            self.add_frame(rms);
        }
        let spec = self.speculation;
        let n = self.frames.len();
        let mut cut = self.cut(n);
        cut.samples.append(&mut self.pending);
        self.speculation = spec;
        Some(cut)
    }

    /// [`Segmenter::take_tail`] for callers that do not speculate.
    pub fn finish(mut self) -> Option<Cut> {
        self.take_tail()
    }

    fn add_frame(&mut self, rms: f32) {
        let frame = self.classify(rms);
        self.frames.push(frame);
        if frame.speech {
            self.silence_run = 0;
            self.speech_frames += 1;
            self.speculation = None;
        } else {
            self.silence_run += 1;
        }
    }

    fn classify(&mut self, rms: f32) -> Frame {
        let floor = match self.recent_min.front() {
            Some(&(_, m)) => m.min(self.floor),
            None => self.floor,
        };
        let threshold = (floor * THRESHOLD_OVER_FLOOR).max(self.config.min_speech_rms);
        let speech = rms > threshold;
        self.floor = if rms < self.floor {
            rms
        } else {
            self.floor + (rms - self.floor) * FLOOR_RISE
        };
        let n = self.frames_seen;
        self.frames_seen += 1;
        while self.recent_min.back().is_some_and(|&(_, m)| m >= rms) {
            self.recent_min.pop_back();
        }
        self.recent_min.push_back((n, rms));
        let window = (FLOOR_WINDOW_SECS * 1000.0 / FRAME_MS as f32) as usize;
        while self
            .recent_min
            .front()
            .is_some_and(|&(i, _)| i + window <= n)
        {
            self.recent_min.pop_front();
        }
        Frame { rms, speech }
    }

    /// How many frames to commit now, if any.
    fn cut_point(&self) -> Option<usize> {
        let fps = self.config.sample_rate as f32 / self.frame_len as f32;
        let buffered = self.frames.len() as f32 / fps;
        let pause = self.config.pause_needed(buffered);
        let pause_frames = ((pause * fps).round() as usize).max(1);
        if buffered >= self.config.min_segment && self.silence_run >= pause_frames {
            return Some(self.pause_cut());
        }
        if buffered >= self.config.max_segment {
            let window = ((FORCED_CUT_WINDOW_SECS * fps) as usize).clamp(1, self.frames.len());
            let from = self.frames.len() - window;
            let quietest = (from..self.frames.len())
                .rev()
                .min_by(|&a, &b| self.frames[a].rms.total_cmp(&self.frames[b].rms))
                .unwrap_or(self.frames.len() - 1);
            return Some(quietest + 1);
        }
        None
    }

    /// Where to cut in the trailing pause: in its truly quiet part (frames
    /// within 2× of its quietest), at the frame nearest the middle. A pause
    /// often starts or ends with a soft consonant under the threshold (the
    /// /s/ of "tests"), and the plain middle can split it off.
    fn pause_cut(&self) -> usize {
        let end = self.frames.len();
        let start = end - self.silence_run;
        let quietest = self.frames[start..end]
            .iter()
            .map(|f| f.rms)
            .fold(f32::INFINITY, f32::min);
        let quiet_enough = quietest * 2.0 + 1e-4;
        let mid = (start + end) as f32 / 2.0;
        (start..end)
            .filter(|&i| self.frames[i].rms <= quiet_enough)
            .min_by(|&a, &b| {
                let da = (a as f32 + 0.5 - mid).abs();
                let db = (b as f32 + 0.5 - mid).abs();
                da.total_cmp(&db)
            })
            .map_or(end, |q| q + 1)
    }

    fn cut(&mut self, frames: usize) -> Cut {
        let frames = frames.min(self.frames.len());
        let n = (frames * self.frame_len).min(self.pending.len());
        let speech = self.frames[..frames].iter().filter(|f| f.speech).count();
        let samples: Vec<f32> = self.pending.drain(..n).collect();
        self.frames.drain(..frames);
        self.speech_frames -= speech;
        self.silence_run = self.silence_run.min(self.frames.len());
        let start = self.pending_start;
        let speculated = self.speculation.is_some_and(|s| s.start == start);
        self.speculation = None;
        self.pending_start += n;
        Cut {
            start,
            samples,
            has_speech: speech >= MIN_SPEECH_FRAMES,
            speculated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: usize = 16_000;

    /// A 200 Hz tone: "speech" as far as RMS is concerned.
    fn tone(secs: f32) -> Vec<f32> {
        let n = (secs * SR as f32) as usize;
        (0..n)
            .map(|i| (i as f32 * 200.0 * std::f32::consts::TAU / SR as f32).sin() * 0.2)
            .collect()
    }

    /// Low-level noise: a quiet room.
    fn hush(secs: f32) -> Vec<f32> {
        let n = (secs * SR as f32) as usize;
        let mut x = 12345u32;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                ((x >> 16) as f32 / 65_536.0 - 0.5) * 0.002
            })
            .collect()
    }

    fn feed(seg: &mut Segmenter, parts: &[Vec<f32>], block: usize) -> Vec<Cut> {
        let all: Vec<f32> = parts.concat();
        all.chunks(block).flat_map(|b| seg.push(b)).collect()
    }

    fn secs(samples: usize) -> f32 {
        samples as f32 / SR as f32
    }

    /// Short segments, so the cut mechanics are tested with seconds of
    /// audio (the numbers dictation used before its defaults grew).
    fn short() -> SegmenterConfig {
        SegmenterConfig {
            min_segment: 3.0,
            max_segment: 10.0,
            pause: 0.35,
            min_pause: 0.15,
            ..SegmenterConfig::default()
        }
    }

    #[test]
    fn dictation_cuts_only_at_a_real_pause_after_8_s() {
        let c = SegmenterConfig::default();
        assert_eq!((c.min_segment, c.max_segment), (8.0, 20.0));
        assert_eq!(c.pause_needed(8.0), 0.5);
        assert_eq!(c.pause_needed(19.0), 0.3);
        // Word gaps of 0.2 s never cut, even past 8 s; a 1 s pause at 6 s
        // is too early; the pause that starts at 9.8 s cuts, inside it.
        let mut seg = Segmenter::new(c);
        let mut parts = vec![tone(6.0), hush(1.0)];
        for _ in 0..3 {
            parts.push(tone(0.8));
            parts.push(hush(0.2));
        }
        parts.push(hush(0.6));
        parts.push(tone(1.0));
        let cuts = feed(&mut seg, &parts, 320);
        assert_eq!(cuts.len(), 1);
        let end = secs(cuts[0].samples.len());
        assert!((9.8..10.6).contains(&end), "cut at {end}");
    }

    #[test]
    fn cuts_in_the_middle_of_the_first_pause_after_min_segment() {
        let mut seg = Segmenter::new(short());
        // 1 s pause too early to cut, then speech past 3 s, then a pause.
        let cuts = feed(
            &mut seg,
            &[
                hush(0.3),
                tone(1.0),
                hush(1.0),
                tone(2.0),
                hush(0.6),
                tone(1.0),
            ],
            320,
        );
        assert_eq!(cuts.len(), 1);
        let c = &cuts[0];
        assert_eq!(c.start, 0);
        assert!(c.has_speech);
        // Speech ends at 4.3 s; the pause reaches 0.36 s at ~4.66 s, cut at
        // its middle (~4.48 s).
        let end = secs(c.samples.len());
        assert!((4.4..4.6).contains(&end), "cut at {end}");
        let tail = seg.finish().unwrap();
        assert_eq!(tail.start, c.samples.len());
        assert!(tail.has_speech);
        assert_eq!(tail.start + tail.samples.len(), (5.9 * SR as f32) as usize);
    }

    #[test]
    fn short_pauses_do_not_cut_without_grading() {
        let config = SegmenterConfig {
            min_pause: 0.35,
            ..short()
        };
        let mut seg = Segmenter::new(config);
        let mut parts = Vec::new();
        for _ in 0..8 {
            parts.push(tone(0.8));
            parts.push(hush(0.2));
        }
        assert!(feed(&mut seg, &parts, 512).is_empty());
        let tail = seg.finish().unwrap();
        assert_eq!(tail.samples.len(), 8 * 16_000);
    }

    #[test]
    fn the_pause_needed_shrinks_as_the_buffer_grows() {
        let c = short();
        assert_eq!(c.pause_needed(1.0), 0.35);
        assert_eq!(c.pause_needed(3.0), 0.35);
        assert!((c.pause_needed(5.0) - 0.19).abs() < 1e-6);
        assert_eq!(c.pause_needed(11.0), 0.15);
        // A 0.2 s gap between words cuts only once ~5 s are buffered.
        let mut seg = Segmenter::new(c);
        let mut parts = Vec::new();
        for _ in 0..8 {
            parts.push(tone(0.8));
            parts.push(hush(0.2));
        }
        let cuts = feed(&mut seg, &parts, 512);
        assert_eq!(cuts.len(), 1);
        let end = secs(cuts[0].samples.len());
        assert!((4.8..5.0).contains(&end), "cut at {end}");
    }

    #[test]
    fn speculation_covers_the_tail_until_speech_resumes() {
        let mut seg = Segmenter::new(short());
        seg.push(&tone(1.0));
        assert!(seg.speculate(0.2).is_none(), "still talking");
        seg.push(&hush(0.1));
        assert!(seg.speculate(0.2).is_none(), "pause too short");
        seg.push(&hush(0.12));
        let (spec, samples) = seg.speculate(0.2).expect("speculate");
        assert_eq!(
            spec,
            Speculation {
                start: 0,
                len: samples.len()
            }
        );
        assert_eq!(samples.len(), (1.22 * 16_000.0) as usize);
        assert!(seg.speculate(0.2).is_none(), "once per pause");
        // More silence: the snapshot still covers the tail.
        seg.push(&hush(0.3));
        let tail = seg.take_tail().unwrap();
        assert_eq!(seg.speculation(), Some(spec));
        assert!(tail.samples.len() > spec.len);

        // Speech after the snapshot invalidates it.
        let mut seg = Segmenter::new(short());
        seg.push(&[tone(1.0), hush(0.25)].concat());
        let (spec, _) = seg.speculate(0.2).unwrap();
        seg.push(&tone(0.3));
        assert_eq!(seg.speculation(), None);
        seg.push(&hush(0.25));
        let (again, _) = seg.speculate(0.2).unwrap();
        assert_eq!(again.start, spec.start);
        assert!(again.len > spec.len);
    }

    #[test]
    fn speculation_needs_speech_and_is_cleared_by_a_cut() {
        let mut seg = Segmenter::new(short());
        seg.push(&hush(1.0));
        assert!(seg.speculate(0.2).is_none(), "nothing said");
        let mut seg = Segmenter::new(short());
        seg.push(&[tone(3.2), hush(0.2)].concat());
        assert!(seg.speculate(0.2).is_some());
        let cuts = seg.push(&hush(0.3));
        assert_eq!(cuts.len(), 1);
        assert!(cuts[0].speculated, "the snapshot covers the cut");
        assert_eq!(seg.speculation(), None);
    }

    #[test]
    fn a_word_starting_right_at_release_invalidates_it() {
        let mut seg = Segmenter::new(short());
        seg.push(&[tone(1.0), hush(0.25)].concat());
        seg.speculate(0.2).unwrap();
        seg.push(&tone(0.01));
        assert!(seg.take_tail().unwrap().has_speech);
        assert_eq!(seg.speculation(), None);
    }

    #[test]
    fn forced_cut_at_the_quietest_frame_of_the_last_two_seconds() {
        let config = SegmenterConfig {
            max_segment: 5.0,
            min_pause: 0.35,
            ..short()
        };
        let mut seg = Segmenter::new(config);
        // Continuous speech with one soft (not silent) 100 ms dip at 4.0 s.
        let mut soft = tone(0.1);
        soft.iter_mut().for_each(|s| *s *= 0.3);
        let cuts = feed(&mut seg, &[tone(4.0), soft, tone(3.0)], 160);
        assert_eq!(cuts.len(), 1);
        let end = secs(cuts[0].samples.len());
        assert!((4.0..=4.1).contains(&end), "forced cut at {end}");
    }

    #[test]
    fn forced_cut_without_any_dip_still_cuts_by_max() {
        let config = SegmenterConfig {
            max_segment: 4.0,
            ..short()
        };
        let mut seg = Segmenter::new(config);
        let cuts = feed(&mut seg, &[tone(9.0)], 1000);
        assert_eq!(cuts.len(), 2);
        assert!(cuts.iter().all(|c| secs(c.samples.len()) <= 4.0));
        assert_eq!(cuts[1].start, cuts[0].samples.len());
    }

    #[test]
    fn silent_segments_are_dropped_but_keep_the_timeline() {
        let mut seg = Segmenter::new(short());
        let cuts = feed(&mut seg, &[hush(4.0), tone(3.5), hush(0.5)], 320);
        // The first 3+ s of hush is committed at a "pause" and dropped.
        assert_eq!(cuts.len(), 1);
        assert!(cuts[0].has_speech);
        assert!(cuts[0].start > 0, "the silent lead was cut off first");
        let end = cuts[0].start + cuts[0].samples.len();
        assert!(secs(end) > 7.5);
    }

    #[test]
    fn all_silence_tail_is_not_speech() {
        let mut seg = Segmenter::new(short());
        assert!(seg.push(&hush(1.0)).is_empty());
        let tail = seg.finish().unwrap();
        assert!(!tail.has_speech);
        assert_eq!(tail.samples.len(), 16_000);
    }

    #[test]
    fn a_soft_consonant_stays_with_its_word() {
        // Loud "speech", a 60 ms gap, a soft /s/ (quieter than the words
        // but well above the room), then a real pause. The cut must land in
        // the pause, not in the gap before the /s/.
        let config = SegmenterConfig {
            min_segment: 1.0,
            pause: 0.2,
            min_pause: 0.2,
            ..short()
        };
        let mut seg = Segmenter::new(config);
        let mut s_sound = tone(0.12);
        s_sound.iter_mut().for_each(|v| *v *= 0.06);
        let parts = [
            hush(0.3),
            tone(4.0),
            hush(0.06),
            s_sound,
            hush(0.3),
            tone(0.5),
        ];
        let cuts = feed(&mut seg, &parts, 320);
        assert_eq!(cuts.len(), 1);
        let end = secs(cuts[0].samples.len());
        assert!(end > 4.48 + 0.04, "cut at {end} splits the /s/");
    }

    #[test]
    fn talking_does_not_lift_the_floor() {
        // 8 s of loud speech with short gaps, then a soft word: still speech.
        let mut seg = Segmenter::new(SegmenterConfig {
            min_segment: 60.0,
            max_segment: 120.0,
            ..short()
        });
        for _ in 0..10 {
            seg.push(&tone(0.7));
            seg.push(&hush(0.1));
        }
        let mut soft = tone(0.3);
        soft.iter_mut().for_each(|v| *v *= 0.1);
        seg.push(&soft);
        let tail: Vec<bool> = seg.frames.iter().rev().take(10).map(|f| f.speech).collect();
        assert!(tail.iter().all(|&s| s), "{tail:?}");
    }

    #[test]
    fn a_click_is_not_speech() {
        let mut seg = Segmenter::new(short());
        let mut x = hush(1.0);
        x[8000..8100].iter_mut().for_each(|s| *s = 0.5);
        seg.push(&x);
        assert!(!seg.finish().unwrap().has_speech);
    }

    #[test]
    fn empty_and_partial_frames() {
        let seg = Segmenter::new(short());
        assert!(seg.finish().is_none());
        let mut seg = Segmenter::new(short());
        seg.push(&tone(0.105));
        let tail = seg.finish().unwrap();
        assert_eq!(tail.samples.len(), 1680);
        assert!(tail.has_speech);
    }

    #[test]
    fn loud_room_raises_the_threshold_over_time() {
        // Steady noise well above min_speech_rms: after the floor adapts,
        // the noise stops counting as speech and pauses in it cut again.
        let mut seg = Segmenter::new(short());
        let noise: Vec<f32> = hush(30.0).iter().map(|s| s * 10.0).collect();
        let cuts = seg.push(&noise);
        assert!(
            cuts.len() <= 2,
            "noise is not speech for long: {}",
            cuts.len()
        );
    }

    #[test]
    fn cuts_tile_the_stream_exactly() {
        let config = SegmenterConfig {
            max_segment: 3.5,
            ..short()
        };
        let mut seg = Segmenter::new(config);
        let parts = [tone(2.0), hush(0.5), tone(4.0), hush(0.4), tone(5.0)];
        let total: usize = parts.iter().map(Vec::len).sum();
        let mut cuts = feed(&mut seg, &parts, 777);
        cuts.extend(seg.finish());
        let mut at = 0;
        for c in &cuts {
            assert_eq!(c.start, at);
            at += c.samples.len();
        }
        assert_eq!(at, total);
    }

    #[test]
    fn meeting_config_brackets_the_chunk() {
        let c = SegmenterConfig::for_meeting(30);
        assert_eq!((c.min_segment, c.max_segment), (25.0, 35.0));
        let c = SegmenterConfig::for_meeting(5);
        assert_eq!((c.min_segment, c.max_segment), (2.0, 10.0));
    }
}
