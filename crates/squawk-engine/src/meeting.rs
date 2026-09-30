//! Meeting recording: mic as "You", system audio as "Them".
//!
//! Each track is cut into ~`chunk_secs` pieces (at a pause near the target
//! length when there is one) and transcribed at meeting priority with
//! segment timestamps. Segments are offset to meeting time, merged with
//! `squawk_core::store::merge_segments`, and the whole file is rewritten via
//! `Store::write_meeting` after every chunk (front matter `status:
//! recording` until the end), so a crash loses at most one chunk.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, SecondsFormat};
use crossbeam_channel::{Receiver, Sender};
use squawk_core::status::MeetingInfo;
use squawk_core::store::{drop_echoes, merge_segments, Meeting, Segment, Speaker};
use squawk_core::Store;

use crate::audio::{self, MicCapture, MicMode, MicShare};
use crate::engine::{BusyGuard, Emit, EngineConfig, EngineEvent};
use crate::error::EngineError;
use crate::model::Recognized;
use crate::recognizer::{Priority, Recognizer};
use crate::segmenter::{Segmenter, SegmenterConfig};
use crate::system_audio::SystemAudioCapture;
use crate::SAMPLE_RATE;

#[derive(Debug, Clone, PartialEq)]
pub struct MeetingOptions {
    pub title: String,
    /// Where to write; the app gets it from `Store::new_meeting_path`.
    pub path: PathBuf,
    pub started_at: DateTime<Local>,
    pub chunk_secs: u64,
    /// Capture system audio as "Them". If ScreenCaptureKit fails the meeting
    /// continues mic-only and an `EngineEvent::MeetingWarning` is sent.
    pub system_audio: bool,
}

/// A meeting in progress. Dropping it stops it (as `stop`, result
/// discarded, on a background thread).
pub struct MeetingHandle {
    opts: MeetingOptions,
    started: Instant,
    had_system_audio: bool,
    running: Option<Running>,
}

/// Everything that has to be shut down, in order.
struct Running {
    mic: MicCapture,
    share: MicShare,
    system: Option<SystemAudioCapture>,
    tracks: Vec<Arc<Track>>,
    writer_tx: Sender<WriterMsg>,
    writer: JoinHandle<Result<(), EngineError>>,
    started: Instant,
    _busy: BusyGuard,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeetingResult {
    pub path: PathBuf,
    pub title: String,
    pub length_secs: u64,
    /// Whether the "Them" track was actually captured.
    pub had_system_audio: bool,
}

impl MeetingHandle {
    pub(crate) fn start(
        opts: MeetingOptions,
        config: EngineConfig,
        recognizer: Recognizer,
        emit: Emit,
        share: MicShare,
        busy: BusyGuard,
    ) -> Result<MeetingHandle, EngineError> {
        let started = Instant::now();
        let store = Store::new(&config.paths);
        let head = Meeting {
            title: opts.title.clone(),
            started_at: opts.started_at,
            length_secs: 0,
            in_progress: true,
            utterances: Vec::new(),
        };
        // Written at once so the meeting shows up in lists while it runs.
        store.write_meeting(&opts.path, &head)?;

        let echo_cancellation = config.meeting.echo_cancellation;
        let (writer_tx, writer_rx) = crossbeam_channel::unbounded();
        let writer = {
            let (opts, emit) = (opts.clone(), emit.clone());
            let filter = echo_cancellation.then_some(drop_echoes as EchoFilter);
            std::thread::Builder::new()
                .name("squawk-meeting-writer".into())
                .spawn(move || {
                    write_loop(store, opts, started, recognizer, writer_rx, filter, emit)
                })?
        };
        let seg_config = SegmenterConfig::for_meeting(opts.chunk_secs);
        let wav = |label: &str| {
            config.keep_audio.then(|| {
                let stamp = opts.started_at.format("%Y-%m-%d %H%M%S");
                config
                    .paths
                    .audio_dir
                    .join(format!("{stamp} meeting {label}.wav"))
            })
        };
        let you = Track::new(
            Speaker::You,
            started,
            seg_config,
            false,
            wav("you"),
            writer_tx.clone(),
        );
        let them = Track::new(
            Speaker::Them,
            started,
            seg_config,
            true,
            wav("them"),
            writer_tx.clone(),
        );

        // MicCapture reopens a device that drops out (AirPods disconnecting,
        // or flipping to their headset profile when a call app opens their
        // mic) and fills the gap with silence, so "You" stays on the meeting
        // clock. Only a mic that cannot be reopened ends up here. With echo
        // cancellation the call playing on the speakers is taken out of it.
        let feeder = you.clone();
        let lent = share.clone();
        let lost = emit.clone();
        let unlend = share.clone();
        let mode = if echo_cancellation {
            MicMode::EchoCancelled
        } else {
            MicMode::Plain
        };
        let mic = MicCapture::start_with_mode(
            config.input_device.as_deref(),
            mode,
            move |b| {
                feeder.feed(b);
                lent.forward(b);
            },
            move |msg| {
                unlend.set_live(false);
                lost(EngineEvent::MeetingMicLost(msg));
            },
        );
        let mic = match mic {
            Ok(m) => m,
            Err(e) => {
                let _ = writer_tx.send(WriterMsg::Abandon);
                let _ = writer.join();
                let _ = std::fs::remove_file(&opts.path);
                return Err(e);
            }
        };

        // A dictation during this meeting listens to this mic rather than
        // opening its own (see `MicShare`).
        share.set_live(mic.echo_cancelled());
        let mut tracks = vec![you];
        let mut system = None;
        if opts.system_audio {
            let feeder = them.clone();
            match SystemAudioCapture::start(move |b| feeder.feed(b)) {
                Ok(s) => {
                    system = Some(s);
                    tracks.push(them);
                }
                Err(e) => {
                    log::warn!("meeting: {e}; recording the mic only");
                    emit(EngineEvent::MeetingWarning(format!(
                        "{e}. Recording your side only."
                    )));
                }
            }
        }
        let on_off = |b: bool| if b { "on" } else { "off" };
        log::info!(
            "meeting: started, system audio {}, echo cancellation {}",
            on_off(system.is_some()),
            on_off(mic.echo_cancelled()),
        );
        Ok(MeetingHandle {
            had_system_audio: system.is_some(),
            opts,
            started,
            running: Some(Running {
                mic,
                share,
                system,
                tracks,
                writer_tx,
                writer,
                started,
                _busy: busy,
            }),
        })
    }

    pub fn options(&self) -> &MeetingOptions {
        &self.opts
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// For `Response::MeetingStarted` and the status response.
    pub fn info(&self) -> MeetingInfo {
        MeetingInfo {
            title: self.opts.title.clone(),
            path: self.opts.path.display().to_string(),
            started_at: self
                .opts
                .started_at
                .to_rfc3339_opts(SecondsFormat::Secs, false),
            elapsed_secs: self.elapsed().as_secs(),
        }
    }

    /// Stop both captures, transcribe the last chunks, write the final file
    /// (no `status: recording`). Blocks until done: call it off the main
    /// thread.
    pub fn stop(mut self) -> Result<MeetingResult, EngineError> {
        let running = self.running.take().expect("running until stopped");
        let length_secs = running.shutdown()?;
        Ok(MeetingResult {
            path: self.opts.path.clone(),
            title: self.opts.title.clone(),
            length_secs,
            had_system_audio: self.had_system_audio,
        })
    }
}

impl Drop for MeetingHandle {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            let _ = std::thread::Builder::new()
                .name("squawk-meeting-stop".into())
                .spawn(move || {
                    if let Err(e) = running.shutdown() {
                        log::warn!("meeting: stop on drop: {e}");
                    }
                });
        }
    }
}

impl Running {
    /// Returns the meeting length in seconds.
    fn shutdown(self) -> Result<u64, EngineError> {
        let length_secs = self.started.elapsed().as_secs();
        self.share.set_live(false);
        self.mic.stop();
        if let Some(s) = self.system {
            s.stop();
        }
        for t in &self.tracks {
            t.finish();
        }
        let _ = self.writer_tx.send(WriterMsg::Finish { length_secs });
        self.writer
            .join()
            .map_err(|_| EngineError::Transcribe("the meeting writer panicked".into()))??;
        log::info!("meeting: stopped after {length_secs} s");
        Ok(length_secs)
    }
}

enum WriterMsg {
    Chunk {
        speaker: Speaker,
        /// Meeting time of the chunk's first sample.
        start_secs: f64,
        samples: Vec<f32>,
    },
    Finish {
        length_secs: u64,
    },
    /// The meeting never really started; exit without writing.
    Abandon,
}

/// One capture track: segments its audio and hands chunks to the writer.
struct Track {
    speaker: Speaker,
    started: Instant,
    /// Insert silence for gaps in delivery (SCK sends nothing while no app
    /// plays audio), so chunk times stay on the meeting clock.
    fill_gaps: bool,
    tx: Sender<WriterMsg>,
    state: Mutex<TrackState>,
}

struct TrackState {
    segmenter: Option<Segmenter>,
    /// Meeting time of this track's first sample, set on the first block.
    origin: Option<f64>,
    received: usize,
    wav: Option<hound::WavWriter<BufWriter<File>>>,
}

/// Gaps shorter than this are jitter, not silence.
const GAP_FILL_MIN: usize = SAMPLE_RATE as usize / 2;

impl Track {
    fn new(
        speaker: Speaker,
        started: Instant,
        config: SegmenterConfig,
        fill_gaps: bool,
        wav: Option<PathBuf>,
        tx: Sender<WriterMsg>,
    ) -> Arc<Track> {
        let wav = wav.and_then(|p| match open_wav(&p) {
            Ok(w) => Some(w),
            Err(e) => {
                log::warn!("meeting: cannot keep audio at {}: {e}", p.display());
                None
            }
        });
        Arc::new(Track {
            speaker,
            started,
            fill_gaps,
            tx,
            state: Mutex::new(TrackState {
                segmenter: Some(Segmenter::new(config)),
                origin: None,
                received: 0,
                wav,
            }),
        })
    }

    fn feed(&self, block: &[f32]) {
        let now = self.started.elapsed().as_secs_f64();
        let mut st = self.state.lock().expect("track lock");
        if st.segmenter.is_none() {
            return;
        }
        let block_secs = block.len() as f64 / SAMPLE_RATE as f64;
        let origin = *st.origin.get_or_insert((now - block_secs).max(0.0));
        if self.fill_gaps {
            let gap = gap_to_fill(origin, now, st.received, block.len());
            if gap > 0 {
                self.push(&mut st, &vec![0.0; gap], origin);
            }
        }
        self.push(&mut st, block, origin);
    }

    fn push(&self, st: &mut TrackState, samples: &[f32], origin: f64) {
        st.received += samples.len();
        if let Some(w) = st.wav.as_mut() {
            for &s in samples {
                let _ = w.write_sample(audio::to_i16(s));
            }
        }
        let Some(seg) = st.segmenter.as_mut() else {
            return;
        };
        for cut in seg.push(samples) {
            self.send(cut.start, cut.samples, origin);
        }
    }

    fn send(&self, start: usize, samples: Vec<f32>, origin: f64) {
        let _ = self.tx.send(WriterMsg::Chunk {
            speaker: self.speaker,
            start_secs: origin + start as f64 / SAMPLE_RATE as f64,
            samples,
        });
    }

    /// Flush the tail and close the WAV. Nothing is accepted afterwards.
    fn finish(&self) {
        let mut st = self.state.lock().expect("track lock");
        let origin = st.origin.unwrap_or(0.0);
        if let Some(tail) = st.segmenter.take().and_then(Segmenter::finish) {
            if tail.has_speech {
                self.send(tail.start, tail.samples, origin);
            }
        }
        if let Some(w) = st.wav.take() {
            if let Err(e) = w.finalize() {
                log::warn!("meeting: closing kept audio: {e}");
            }
        }
    }
}

fn open_wav(path: &Path) -> Result<hound::WavWriter<BufWriter<File>>, EngineError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    hound::WavWriter::create(path, audio::wav_spec()).map_err(|e| EngineError::AudioFile {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

/// Samples of silence to insert before a block that arrived at meeting time
/// `now` (its end), given the track started at `origin` and `received`
/// samples came before it. Zero unless the gap is at least half a second.
fn gap_to_fill(origin: f64, now: f64, received: usize, block: usize) -> usize {
    let expected = ((now - origin).max(0.0) * SAMPLE_RATE as f64) as usize;
    let have = received + block;
    let gap = expected.saturating_sub(have);
    if gap >= GAP_FILL_MIN {
        gap
    } else {
        0
    }
}

/// A chunk's model output as meeting segments: timestamps moved from the
/// chunk's clock to the meeting's. A result with text but no segments
/// becomes one segment at the chunk start.
fn chunk_segments(rec: Recognized, speaker: Speaker, start_secs: f64) -> Vec<Segment> {
    if rec.segments.is_empty() {
        let text = rec.text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        return vec![Segment {
            speaker,
            start_secs,
            end_secs: start_secs,
            text: text.to_string(),
        }];
    }
    rec.segments
        .into_iter()
        .map(|(s, e, text)| Segment {
            speaker,
            start_secs: start_secs + s as f64,
            end_secs: start_secs + e as f64,
            text,
        })
        .collect()
}

/// Takes echo out of the segments before they are merged
/// (`store::drop_echoes`).
type EchoFilter = fn(Vec<Segment>) -> Vec<Segment>;

fn write_loop(
    store: Store,
    opts: MeetingOptions,
    started: Instant,
    recognizer: Recognizer,
    rx: Receiver<WriterMsg>,
    echo_filter: Option<EchoFilter>,
    emit: Emit,
) -> Result<(), EngineError> {
    let mut segments: Vec<Segment> = Vec::new();
    // Every write filters the whole meeting again: a "Them" chunk can land
    // after the "You" chunk its echo is in.
    let mut dropped = 0;
    let mut write = |segments: &[Segment], length_secs: u64, in_progress: bool| {
        let mut kept = segments.to_vec();
        if let Some(filter) = echo_filter {
            kept = filter(kept);
            let now = segments.len() - kept.len();
            if now != dropped {
                log::info!("meeting: {now} echoed segments of \"You\" dropped");
                dropped = now;
            }
        }
        let meeting = Meeting {
            title: opts.title.clone(),
            started_at: opts.started_at,
            length_secs,
            in_progress,
            utterances: merge_segments(kept),
        };
        store.write_meeting(&opts.path, &meeting)
    };
    for msg in rx {
        match msg {
            WriterMsg::Chunk {
                speaker,
                start_secs,
                samples,
            } => {
                let got = recognizer
                    .submit(samples, Priority::Meeting)
                    .recv()
                    .map_err(|_| EngineError::Transcribe("the recognizer stopped".into()))
                    .and_then(|r| r);
                match got {
                    Ok(rec) => segments.extend(chunk_segments(rec, speaker, start_secs)),
                    Err(e) => {
                        log::warn!("meeting: chunk failed: {e}");
                        emit(EngineEvent::MeetingWarning(format!(
                            "part of the meeting could not be transcribed: {e}"
                        )));
                    }
                }
                let elapsed = started.elapsed().as_secs();
                match write(&segments, elapsed, true) {
                    Ok(()) => emit(EngineEvent::MeetingProgress {
                        path: opts.path.clone(),
                        elapsed_secs: elapsed,
                    }),
                    Err(e) => log::warn!("meeting: write failed: {e}"),
                }
            }
            WriterMsg::Finish { length_secs } => {
                write(&segments, length_secs, false)?;
                return Ok(());
            }
            WriterMsg::Abandon => return Ok(()),
        }
    }
    // Every sender dropped without a Finish: write what we have as final.
    write(&segments, started.elapsed().as_secs(), false)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recognizer::Backend;
    use squawk_core::Paths;

    #[test]
    fn chunk_segments_move_to_meeting_time() {
        let rec = Recognized {
            text: "Hi there. Next point.".into(),
            segments: vec![
                (0.5, 1.2, "Hi there.".into()),
                (2.0, 3.0, "Next point.".into()),
            ],
        };
        let segs = chunk_segments(rec, Speaker::Them, 60.0);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].speaker, Speaker::Them);
        assert!((segs[0].start_secs - 60.5).abs() < 1e-6);
        assert!((segs[1].end_secs - 63.0).abs() < 1e-6);
        assert_eq!(segs[1].text, "Next point.");
    }

    #[test]
    fn chunk_without_segments_is_one_block() {
        let rec = Recognized {
            text: " Okay. ".into(),
            segments: vec![],
        };
        let segs = chunk_segments(rec, Speaker::You, 12.0);
        assert_eq!(segs.len(), 1);
        assert_eq!((segs[0].start_secs, segs[0].text.as_str()), (12.0, "Okay."));
        assert!(chunk_segments(Recognized::default(), Speaker::You, 0.0).is_empty());
    }

    #[test]
    fn gaps_are_filled_only_when_real() {
        // 10 s in, 9.9 s received: jitter.
        assert_eq!(gap_to_fill(0.0, 10.0, 158_400, 1600), 0);
        // 10 s in, 5 s received: fill the missing 5 s minus this block.
        assert_eq!(gap_to_fill(0.0, 10.0, 80_000, 1600), 78_400);
        // Late start is measured from the origin.
        assert_eq!(gap_to_fill(2.0, 3.0, 0, 16_000), 0);
        assert_eq!(gap_to_fill(5.0, 4.0, 0, 100), 0);
    }

    /// Echoes the number of samples as text with one segment at 0.
    struct Echo;

    impl Backend for Echo {
        fn transcribe(&mut self, samples: &[f32]) -> Result<Recognized, EngineError> {
            let t = format!("{} samples.", samples.len());
            Ok(Recognized {
                text: t.clone(),
                segments: vec![(0.0, 1.0, t)],
            })
        }
    }

    #[test]
    fn writer_merges_tracks_and_finalizes_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path());
        let store = Store::new(&paths);
        let path = paths.meetings_dir.join("2026-09-29 1400 Sync.md");
        let started_at = Local::now();
        let opts = MeetingOptions {
            title: "Sync".into(),
            path: path.clone(),
            started_at,
            chunk_secs: 30,
            system_audio: true,
        };
        let r = Recognizer::spawn_with(
            PathBuf::from("/fake"),
            || Ok(Box::new(Echo) as Box<dyn Backend>),
            |_| {},
        );
        let (tx, rx) = crossbeam_channel::unbounded();
        let events = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        let emit: Emit = Arc::new(move |e| ev.lock().unwrap().push(e));
        let writer = std::thread::spawn({
            let opts = opts.clone();
            move || write_loop(store, opts, Instant::now(), r, rx, Some(drop_echoes), emit)
        });
        let chunk = |speaker, start_secs, n| WriterMsg::Chunk {
            speaker,
            start_secs,
            samples: vec![0.0; n],
        };
        tx.send(chunk(Speaker::You, 0.0, 10)).unwrap();
        tx.send(chunk(Speaker::Them, 31.0, 20)).unwrap();
        tx.send(chunk(Speaker::You, 30.0, 30)).unwrap();
        tx.send(WriterMsg::Finish { length_secs: 65 }).unwrap();
        writer.join().unwrap().unwrap();

        let m = Store::new(&paths).read_meeting(&path).unwrap();
        assert!(!m.in_progress);
        assert_eq!(m.length_secs, 65);
        assert_eq!(m.title, "Sync");
        let blocks: Vec<(Speaker, u64, &str)> = m
            .utterances
            .iter()
            .map(|u| (u.speaker, u.start_secs, u.text.as_str()))
            .collect();
        assert_eq!(
            blocks,
            vec![
                (Speaker::You, 0, "10 samples. 30 samples."),
                (Speaker::Them, 31, "20 samples."),
            ]
        );
        let progress = events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, EngineEvent::MeetingProgress { .. }))
            .count();
        assert_eq!(progress, 3);
    }

    #[test]
    fn track_offsets_chunks_by_its_origin() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let config = SegmenterConfig {
            min_segment: 1.0,
            max_segment: 2.0,
            ..SegmenterConfig::default()
        };
        let track = Track::new(Speaker::Them, Instant::now(), config, false, None, tx);
        let tone: Vec<f32> = (0..16_000 * 5)
            .map(|i| (i as f32 * 0.07).sin() * 0.3)
            .collect();
        for b in tone.chunks(1600) {
            track.feed(b);
        }
        track.finish();
        track.feed(&tone[..1600]);
        let mut chunks = Vec::new();
        for msg in rx.try_iter() {
            if let WriterMsg::Chunk {
                start_secs,
                samples,
                speaker,
            } = msg
            {
                assert_eq!(speaker, Speaker::Them);
                chunks.push((start_secs, samples.len()));
            }
        }
        let total: usize = chunks.iter().map(|c| c.1).sum();
        assert_eq!(total, tone.len(), "nothing accepted after finish");
        assert!(chunks.len() >= 3);
        // The origin is the first block's start (~0 here); each chunk starts
        // where the previous one ended.
        assert!(chunks[0].0 < 0.1);
        for w in chunks.windows(2) {
            let expected = w[0].0 + w[0].1 as f64 / 16_000.0;
            assert!((w[1].0 - expected).abs() < 1e-9);
        }
    }
}
