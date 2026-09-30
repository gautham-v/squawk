//! One push-to-talk or hands-free dictation.
//!
//! Latency is the point of this module. While the user talks, the segmenter
//! cuts the audio at pauses and each committed segment is transcribed right
//! away, so by the time fn comes up only the tail since the last pause is
//! left. Target: `finish` returns within ~250 ms of being called for a 20 s
//! utterance on an M4.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::audio::{self, MicCapture, MicListener, MicShare};
use crate::engine::{BusyGuard, Emit, EngineEvent};
use crate::error::EngineError;
use crate::recognizer::{Priority, Recognizer, Reply};
use crate::segmenter::{Segmenter, SegmenterConfig, Speculation};
use crate::SAMPLE_RATE;

/// Less audio than this is a slip of the finger: no inference, empty text.
const MIN_AUDIO: Duration = Duration::from_millis(250);
/// Silence after speech that starts an early transcription of the tail:
/// people let go of fn a few hundred ms after their last word, so by then
/// the tail is usually already done.
const SPECULATE_AFTER_SECS: f32 = 0.2;

/// A dictation in progress. `Send`, so it can be started on the hotkey
/// thread and finished on a worker. Dropping it cancels.
pub struct DictationSession {
    id: u64,
    started_at: Instant,
    shared: Arc<Shared>,
    starter: Option<JoinHandle<Result<Box<dyn Source>, EngineError>>>,
    audio_dir: Option<PathBuf>,
    _busy: Option<BusyGuard>,
}

/// What a finished dictation produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    /// Raw model text, segments joined with single spaces. Empty when
    /// nothing was said (too short, or silence).
    pub text: String,
    /// Seconds of audio captured.
    pub audio_secs: f32,
    /// From the `finish` call to the text being ready: the number to log.
    pub tail_latency: Duration,
    /// How many segments were transcribed (committed ones plus the tail).
    pub segments: usize,
    /// The kept WAV, when `keep_audio` is on.
    pub audio_path: Option<PathBuf>,
}

/// Where the audio comes from: the mic, or a clip replayed in real time
/// (benchmarks and tests exercise the exact same path minus CoreAudio).
pub(crate) enum Input {
    /// The named device (`None` = default); or, while an echo-cancelled
    /// meeting holds the mic, that meeting's stream.
    Mic {
        device: Option<String>,
        share: MicShare,
    },
    Replay {
        samples: Vec<f32>,
        speed: f32,
    },
}

/// A running audio source that can be stopped (flushing what it holds).
trait Source: Send {
    fn stop(self: Box<Self>);
}

impl Source for MicCapture {
    fn stop(self: Box<Self>) {
        MicCapture::stop(*self);
    }
}

impl Source for MicListener {
    fn stop(self: Box<Self>) {}
}

struct Replay {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Source for Replay {
    fn stop(self: Box<Self>) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.thread.join();
    }
}

/// Session ids, unique for the life of the process.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

struct Shared {
    id: u64,
    state: Mutex<Capture>,
    /// f32 bits: RMS of the latest block.
    level: AtomicU32,
    cancelled: Arc<AtomicBool>,
}

struct Capture {
    segmenter: Option<Segmenter>,
    jobs: Vec<Reply>,
    total: usize,
    limit: usize,
    too_long: bool,
    kept: Option<Vec<f32>>,
    /// The in-flight early transcription of everything pending, and its
    /// own cancel flag (set when speech resumes and it goes stale).
    spec: Option<(Speculation, Reply, Arc<AtomicBool>)>,
    recognizer: Recognizer,
    emit: Emit,
}

impl Capture {
    fn drop_speculation(&mut self) {
        if let Some((_, _, flag)) = self.spec.take() {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

impl Shared {
    /// Runs on the capture thread for every 16 kHz block.
    fn feed(&self, block: &[f32]) {
        self.level
            .store(audio::rms(block).to_bits(), Ordering::Relaxed);
        let mut st = self.state.lock().expect("dictation lock");
        let room = st.limit.saturating_sub(st.total);
        if room == 0 {
            if !st.too_long {
                st.too_long = true;
                (st.emit)(EngineEvent::DictationTooLong { session: self.id });
            }
            return;
        }
        let block = &block[..block.len().min(room)];
        st.total += block.len();
        if let Some(kept) = st.kept.as_mut() {
            kept.extend_from_slice(block);
        }
        let Some(seg) = st.segmenter.as_mut() else {
            return;
        };
        let cuts = seg.push(block);
        let still_valid = seg.speculation();
        let fresh = seg.speculate(SPECULATE_AFTER_SECS);
        for cut in cuts {
            let early = match st.spec.take() {
                Some((spec, rx, _)) if cut.speculated && spec.start == cut.start => Some(rx),
                other => {
                    st.spec = other;
                    None
                }
            };
            log::debug!(
                "dictation: segment {:.2} s at {:.2} s{}",
                cut.samples.len() as f32 / SAMPLE_RATE as f32,
                cut.start as f32 / SAMPLE_RATE as f32,
                if early.is_some() {
                    " (early run reused)"
                } else {
                    ""
                }
            );
            let rx = early.unwrap_or_else(|| {
                st.recognizer.submit_cancellable(
                    cut.samples,
                    Priority::Dictation,
                    Some(self.cancelled.clone()),
                )
            });
            st.jobs.push(rx);
        }
        if st.spec.as_ref().map(|s| s.0) != still_valid {
            st.drop_speculation();
        }
        if let Some((spec, samples)) = fresh {
            log::debug!(
                "dictation: early run of {:.2} s at {:.2} s",
                spec.len as f32 / SAMPLE_RATE as f32,
                st.total as f32 / SAMPLE_RATE as f32
            );
            st.drop_speculation();
            let flag = Arc::new(AtomicBool::new(self.cancelled.load(Ordering::Relaxed)));
            let rx =
                st.recognizer
                    .submit_cancellable(samples, Priority::Dictation, Some(flag.clone()));
            st.spec = Some((spec, rx, flag));
        }
    }
}

impl DictationSession {
    pub(crate) fn start(
        recognizer: Recognizer,
        input: Input,
        max: Duration,
        audio_dir: Option<PathBuf>,
        emit: Emit,
        busy: BusyGuard,
    ) -> DictationSession {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let shared = Arc::new(Shared {
            id,
            state: Mutex::new(Capture {
                segmenter: Some(Segmenter::new(SegmenterConfig::default())),
                jobs: Vec::new(),
                total: 0,
                limit: (max.as_secs_f64() * SAMPLE_RATE as f64) as usize,
                too_long: false,
                kept: audio_dir.is_some().then(Vec::new),
                spec: None,
                recognizer,
                emit: emit.clone(),
            }),
            level: AtomicU32::new(0),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        let feeder = shared.clone();
        // Opening the mic takes 50–150 ms; do it off the caller's thread so
        // the fn-down handler returns at once. Recording has still started
        // on fn down: nothing said after the stream opens is lost.
        let starter = std::thread::Builder::new()
            .name("squawk-dictation".into())
            .spawn(move || open(input, feeder, emit));
        let starter = match starter {
            Ok(h) => Some(h),
            Err(e) => {
                log::error!("dictation: could not spawn: {e}");
                None
            }
        };
        DictationSession {
            id,
            started_at: Instant::now(),
            shared,
            starter,
            audio_dir,
            _busy: Some(busy),
        }
    }

    /// Tags this session's [`EngineEvent`]s, so a late event from an
    /// earlier session is not taken for the live one's.
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Input level for a meter: 0 at -60 dBFS or below, 1 at 0 dBFS.
    pub fn level(&self) -> f32 {
        let rms = f32::from_bits(self.shared.level.load(Ordering::Relaxed));
        if rms <= 1e-6 {
            return 0.0;
        }
        ((20.0 * rms.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
    }

    /// Stop the mic, transcribe what is left, and return the whole text.
    /// Blocks: call it off the main thread. Waits for the model if it is
    /// still loading.
    pub fn finish(mut self) -> Result<Transcript, EngineError> {
        let t0 = Instant::now();
        self.stop_source()?;
        let mut st = self.shared.state.lock().expect("dictation lock");
        let total = st.total;
        let audio_secs = total as f32 / SAMPLE_RATE as f32;
        let (tail, covered) = match st.segmenter.take() {
            Some(mut seg) => {
                let tail = seg.take_tail();
                (tail, seg.speculation())
            }
            None => (None, None),
        };
        // The early transcription stands in for the tail when no speech
        // came after it: then nothing is left to run now.
        let early = match st.spec.take() {
            Some((spec, rx, _)) if Some(spec) == covered => Some(rx),
            Some((_, _, flag)) => {
                flag.store(true, Ordering::Relaxed);
                None
            }
            None => None,
        };
        let mut jobs = std::mem::take(&mut st.jobs);
        let kept = st.kept.take();
        let recognizer = st.recognizer.clone();
        drop(st);

        let mut text = String::new();
        let mut segments = 0;
        if audio_secs >= MIN_AUDIO.as_secs_f32() {
            if let Some(tail) = tail.filter(|t| t.has_speech) {
                let secs = tail.samples.len() as f32 / SAMPLE_RATE as f32;
                jobs.push(match early {
                    Some(rx) => {
                        log::debug!("dictation: tail {secs:.2} s covered by the early run");
                        rx
                    }
                    None => {
                        log::debug!("dictation: tail {secs:.2} s submitted at release");
                        recognizer.submit(tail.samples, Priority::Dictation)
                    }
                });
            }
            segments = jobs.len();
            let mut parts = Vec::with_capacity(jobs.len());
            for rx in jobs {
                let got = rx
                    .recv()
                    .map_err(|_| EngineError::Transcribe("the recognizer stopped".into()))??;
                parts.push(got.text);
            }
            text = join_segments(&parts);
        } else {
            self.shared.cancelled.store(true, Ordering::Relaxed);
        }
        let tail_latency = t0.elapsed();

        let audio_path = match (self.audio_dir.as_ref(), kept) {
            (Some(dir), Some(samples)) if !samples.is_empty() => {
                let name = chrono::Local::now()
                    .format("%Y-%m-%d %H%M%S.wav")
                    .to_string();
                let path = dir.join(name);
                match audio::write_wav(&path, &samples) {
                    Ok(()) => Some(path),
                    Err(e) => {
                        log::warn!("dictation: could not keep audio: {e}");
                        None
                    }
                }
            }
            _ => None,
        };
        Ok(Transcript {
            text,
            audio_secs,
            tail_latency,
            segments,
            audio_path,
        })
    }

    /// Stop the mic and drop everything. Returns immediately; queued segment
    /// jobs are skipped, and one already running is discarded.
    pub fn cancel(self) {
        drop(self);
    }

    fn stop_source(&mut self) -> Result<(), EngineError> {
        let Some(starter) = self.starter.take() else {
            return Err(EngineError::Mic("the capture thread did not start".into()));
        };
        let source = starter
            .join()
            .map_err(|_| EngineError::Mic("the capture thread panicked".into()))??;
        source.stop();
        Ok(())
    }
}

impl Drop for DictationSession {
    fn drop(&mut self) {
        let Some(starter) = self.starter.take() else {
            return;
        };
        self.shared.cancelled.store(true, Ordering::Relaxed);
        if let Ok(mut st) = self.shared.state.lock() {
            st.segmenter = None;
            st.jobs.clear();
            st.drop_speculation();
        }
        // The mic may still be opening; close it off this thread.
        let _ = std::thread::Builder::new()
            .name("squawk-dictation-cancel".into())
            .spawn(move || {
                if let Ok(Ok(source)) = starter.join() {
                    source.stop();
                }
            });
    }
}

fn open(input: Input, shared: Arc<Shared>, emit: Emit) -> Result<Box<dyn Source>, EngineError> {
    match input {
        Input::Mic { device, share } => {
            let feeder = shared.clone();
            if let Some(listener) = share.listen(move |b| feeder.feed(b)) {
                log::info!("dictation: using the meeting's echo-cancelled mic");
                return Ok(Box::new(listener));
            }
            let session = shared.id;
            let feeder = shared.clone();
            let lost = emit.clone();
            let mic = MicCapture::start_with_errors(
                device.as_deref(),
                move |b| feeder.feed(b),
                move |message| lost(EngineEvent::MicLost { session, message }),
            );
            match mic {
                Ok(m) => Ok(Box::new(m)),
                Err(e) => {
                    emit(EngineEvent::MicLost {
                        session,
                        message: e.to_string(),
                    });
                    Err(e)
                }
            }
        }
        Input::Replay { samples, speed } => {
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            let thread = std::thread::Builder::new()
                .name("squawk-replay".into())
                .spawn(move || replay(&samples, speed, &flag, |b| shared.feed(b)))?;
            Ok(Box::new(Replay { stop, thread }))
        }
    }
}

/// Deliver `samples` in 20 ms blocks on the real-time schedule a mic would,
/// scaled by `speed`, until done or stopped.
fn replay(samples: &[f32], speed: f32, stop: &AtomicBool, mut sink: impl FnMut(&[f32])) {
    let block = SAMPLE_RATE as usize / 50;
    let t0 = Instant::now();
    for (i, chunk) in samples.chunks(block).enumerate() {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let due = Duration::from_secs_f64((i + 1) as f64 * 0.02 / speed.max(0.01) as f64);
        if let Some(wait) = due.checked_sub(t0.elapsed()) {
            std::thread::sleep(wait);
        }
        sink(chunk);
    }
}

/// Join per-segment texts with single spaces, skipping empty ones, and
/// mend the seams our cuts made. The model sees each segment alone, so a
/// cut mid-sentence tends to come back as "pauses. And each": when the next
/// segment opens with a word that continues a sentence, the period becomes a
/// comma and the word goes lowercase; when the previous one ended without
/// punctuation, a capitalised function word is lowercased.
pub fn join_segments(parts: &[String]) -> String {
    let mut out = String::new();
    for part in parts.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
        if out.is_empty() {
            out.push_str(part);
            continue;
        }
        let first = part.split_whitespace().next().unwrap_or("");
        let bare = first.trim_end_matches(|c: char| !c.is_alphanumeric());
        let lower = bare.to_lowercase();
        let capitalised = is_capitalised(bare);
        let ends_period = out.ends_with('.') && !out.ends_with("..");
        let ends_open = out.ends_with(|c: char| c.is_alphanumeric());
        let mut next = part.to_string();
        if capitalised && ends_period && CONTINUERS.contains(&lower.as_str()) {
            out.pop();
            out.push(',');
            next = lowercase_first(part);
        } else if capitalised && ends_open && FUNCTION_WORDS.contains(&lower.as_str()) {
            next = lowercase_first(part);
        }
        out.push(' ');
        out.push_str(&next);
    }
    out
}

/// Words that open a clause continuing the previous sentence.
const CONTINUERS: &[&str] = &[
    "and", "but", "or", "because", "which", "so", "then", "with", "to", "of", "for", "that",
];

/// Words that are lowercase mid-sentence (not "I", not names).
const FUNCTION_WORDS: &[&str] = &[
    "a", "an", "the", "and", "but", "or", "so", "to", "of", "for", "in", "on", "at", "with",
    "that", "this", "it", "is", "are", "was", "we", "you", "they", "then", "because", "which",
    "if", "when", "where", "while", "from", "by", "as", "into", "about",
];

/// "And" yes; "AND", "GitHub", "I" no.
fn is_capitalised(word: &str) -> bool {
    let mut chars = word.chars();
    matches!(chars.next(), Some(c) if c.is_uppercase())
        && word.chars().count() > 1
        && chars.all(|c| !c.is_uppercase())
}

fn lowercase_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Recognized;
    use crate::recognizer::Backend;
    use std::sync::atomic::AtomicUsize;

    /// "Transcribes" a clip as the number of samples it had.
    struct Count {
        calls: Arc<AtomicUsize>,
    }

    impl Backend for Count {
        fn transcribe(&mut self, samples: &[f32]) -> Result<Recognized, EngineError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(Recognized {
                text: format!("[{}]", samples.len()),
                segments: vec![],
            })
        }
    }

    fn recognizer() -> (Recognizer, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let r = Recognizer::spawn_with(
            PathBuf::from("/fake"),
            move || Ok(Box::new(Count { calls: c }) as Box<dyn Backend>),
            |_| {},
        );
        (r, calls)
    }

    fn tone(secs: f32) -> Vec<f32> {
        (0..(secs * 16_000.0) as usize)
            .map(|i| (i as f32 * 0.08).sin() * 0.2)
            .collect()
    }

    fn silence(secs: f32) -> Vec<f32> {
        vec![0.0; (secs * 16_000.0) as usize]
    }

    fn session(
        samples: Vec<f32>,
        max: Duration,
    ) -> (DictationSession, Arc<Mutex<Vec<EngineEvent>>>) {
        let (r, _) = recognizer();
        let events = Arc::new(Mutex::new(Vec::new()));
        let e = events.clone();
        let emit: Emit = Arc::new(move |ev| e.lock().unwrap().push(ev));
        let busy = BusyGuard::acquire(&Arc::new(AtomicBool::new(false)), "a dictation").unwrap();
        let s = DictationSession::start(
            r,
            Input::Replay {
                samples,
                speed: 50.0,
            },
            max,
            None,
            emit,
            busy,
        );
        (s, events)
    }

    fn wait_for_replay(secs: f32) {
        std::thread::sleep(Duration::from_secs_f32(secs / 50.0 + 0.1));
    }

    #[test]
    fn segments_are_transcribed_while_talking_and_joined() {
        let audio = [tone(3.5), silence(0.5), tone(3.2), silence(0.5), tone(1.0)].concat();
        let n = audio.len();
        let (s, _) = session(audio, Duration::from_secs(600));
        wait_for_replay(n as f32 / 16_000.0);
        let t = s.finish().unwrap();
        assert_eq!(t.segments, 3);
        let lens: Vec<usize> = t
            .text
            .split(' ')
            .map(|p| p.trim_matches(['[', ']']).parse().unwrap())
            .collect();
        // Early runs cover speech plus 0.2 s of pause, where the cut would
        // have taken half the pause: within a few frames of the whole.
        let sum: usize = lens.iter().sum();
        assert!(sum.abs_diff(n) <= 3 * 1600, "{sum} vs {n}");
        assert!((t.audio_secs - n as f32 / 16_000.0).abs() < 1e-3);
    }

    #[test]
    fn too_short_or_silent_means_no_inference() {
        let (s, _) = session(tone(0.2), Duration::from_secs(600));
        wait_for_replay(0.2);
        let t = s.finish().unwrap();
        assert_eq!((t.text.as_str(), t.segments), ("", 0));

        let (s, _) = session(silence(2.0), Duration::from_secs(600));
        wait_for_replay(2.0);
        let t = s.finish().unwrap();
        assert_eq!((t.text.as_str(), t.segments), ("", 0));
        assert!((t.audio_secs - 2.0).abs() < 1e-3);
    }

    #[test]
    fn max_length_stops_listening_and_says_so() {
        let (s, events) = session(tone(3.0), Duration::from_secs(1));
        wait_for_replay(3.0);
        let id = s.id();
        let t = s.finish().unwrap();
        assert!((t.audio_secs - 1.0).abs() < 1e-3);
        let evs = events.lock().unwrap();
        assert_eq!(
            evs.iter()
                .filter(|e| **e == EngineEvent::DictationTooLong { session: id })
                .count(),
            1
        );
    }

    #[test]
    fn cancel_releases_the_busy_flag() {
        let flag = Arc::new(AtomicBool::new(false));
        let (r, calls) = recognizer();
        let busy = BusyGuard::acquire(&flag, "a dictation").unwrap();
        let s = DictationSession::start(
            r,
            Input::Replay {
                samples: tone(5.0),
                speed: 1.0,
            },
            Duration::from_secs(600),
            None,
            Arc::new(|_| {}),
            busy,
        );
        assert!(BusyGuard::acquire(&flag, "a dictation").is_err());
        s.cancel();
        assert!(BusyGuard::acquire(&flag, "a dictation").is_ok());
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn keep_audio_writes_a_wav() {
        let dir = tempfile::tempdir().unwrap();
        let (r, _) = recognizer();
        let busy = BusyGuard::acquire(&Arc::new(AtomicBool::new(false)), "x").unwrap();
        let s = DictationSession::start(
            r,
            Input::Replay {
                samples: tone(1.0),
                speed: 50.0,
            },
            Duration::from_secs(600),
            Some(dir.path().to_path_buf()),
            Arc::new(|_| {}),
            busy,
        );
        wait_for_replay(1.0);
        let t = s.finish().unwrap();
        let path = t.audio_path.expect("kept");
        assert_eq!(audio::load_file(&path).unwrap().len(), 16_000);
    }

    #[test]
    fn join_mends_seams() {
        let j = |parts: &[&str]| {
            join_segments(&parts.iter().map(|p| p.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(
            j(&["the segmenter cuts at pauses.", "And each segment is sent."]),
            "the segmenter cuts at pauses, and each segment is sent."
        );
        assert_eq!(
            j(&["Open the file", "The one in src."]),
            "Open the file the one in src."
        );
        // Real sentence starts and names stay.
        assert_eq!(
            j(&["That works.", "Now run it."]),
            "That works. Now run it."
        );
        assert_eq!(
            j(&["Push it to", "GitHub today."]),
            "Push it to GitHub today."
        );
        assert_eq!(j(&["It works.", "I think."]), "It works. I think.");
        assert_eq!(j(&["Wait...", "And then?"]), "Wait... And then?");
        assert_eq!(
            j(&["Is it done?", "And tested?"]),
            "Is it done? And tested?"
        );
    }

    #[test]
    fn early_transcription_stands_in_for_the_tail() {
        let (r, calls) = recognizer();
        let busy = BusyGuard::acquire(&Arc::new(AtomicBool::new(false)), "x").unwrap();
        // One short utterance and a pause: speculated once, then reused.
        let s = DictationSession::start(
            r,
            Input::Replay {
                samples: [tone(1.5), silence(0.6)].concat(),
                speed: 50.0,
            },
            Duration::from_secs(600),
            None,
            Arc::new(|_| {}),
            busy,
        );
        wait_for_replay(2.1);
        let t = s.finish().unwrap();
        assert_eq!(t.segments, 1);
        // The speculative job saw the speech plus 0.2 s of silence.
        assert_eq!(t.text, format!("[{}]", 16_000 * 17 / 10));
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "the tail was not run again"
        );
    }

    #[test]
    fn a_cut_right_after_an_early_run_reuses_it() {
        let (r, calls) = recognizer();
        let busy = BusyGuard::acquire(&Arc::new(AtomicBool::new(false)), "x").unwrap();
        let s = DictationSession::start(
            r,
            Input::Replay {
                samples: [tone(5.3), silence(0.3)].concat(),
                speed: 50.0,
            },
            Duration::from_secs(600),
            None,
            Arc::new(|_| {}),
            busy,
        );
        wait_for_replay(5.6);
        let t = s.finish().unwrap();
        assert_eq!(t.segments, 1);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn stale_early_transcription_is_not_used() {
        let (r, _) = recognizer();
        let busy = BusyGuard::acquire(&Arc::new(AtomicBool::new(false)), "x").unwrap();
        let audio = [tone(1.0), silence(0.25), tone(0.5)].concat();
        let n = audio.len();
        let s = DictationSession::start(
            r,
            Input::Replay {
                samples: audio,
                speed: 50.0,
            },
            Duration::from_secs(600),
            None,
            Arc::new(|_| {}),
            busy,
        );
        wait_for_replay(1.75);
        let t = s.finish().unwrap();
        assert_eq!(t.text, format!("[{n}]"));
    }

    #[test]
    fn join_skips_empty_parts() {
        let parts = vec!["Hello there.".into(), " ".into(), " How are you? ".into()];
        assert_eq!(join_segments(&parts), "Hello there. How are you?");
        assert_eq!(join_segments(&[]), "");
    }
}
