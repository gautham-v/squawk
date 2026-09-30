//! Mic capture (cpal), downmix, resampling to 16 kHz, and audio files.
//!
//! The mic is opened at the device's own rate and format (whatever
//! `default_input_config` says, usually 48 kHz f32), downmixed to mono, and
//! resampled to 16 kHz on the capture thread — never in the CoreAudio
//! callback, which only copies into a channel.

use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use crossbeam_channel::{select, Receiver, Sender};

use crate::error::EngineError;
use crate::voice_processing::VoiceInput;
use crate::SAMPLE_RATE;

/// How the mic is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MicMode {
    /// The device as it is, through cpal: lowest latency, nothing done to
    /// the sound, other audio left alone. Dictation.
    #[default]
    Plain,
    /// Apple's voice processing on the default input: echo cancellation
    /// (the other side of a call on speakers is taken out of the mic),
    /// noise suppression and gain control, at the cost of ducking other
    /// audio a little. Meetings. Falls back to `Plain` when it cannot start,
    /// or when a named device other than the default is asked for.
    EchoCancelled,
}

/// A running mic stream. The cpal stream lives on its own thread, which also
/// downmixes and resamples; this handle is `Send`. Dropping it stops capture.
pub struct MicCapture {
    name: String,
    echo_cancelled: bool,
    stop_tx: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl MicCapture {
    /// Open `device` (by name; `None` = system default) and deliver 16 kHz
    /// mono samples to `sink` in blocks of ~10–20 ms, on the capture thread.
    /// Returns once the stream is playing or has failed to open.
    pub fn start(
        device: Option<&str>,
        sink: impl FnMut(&[f32]) + Send + 'static,
    ) -> Result<MicCapture, EngineError> {
        MicCapture::start_with_errors(device, sink, |_| {})
    }

    /// [`MicCapture::start`], plus `on_lost` for a mic that is gone for
    /// good. When the device disappears or changes its sample rate (AirPods
    /// switching to their headset profile does this the moment their mic
    /// opens), the stream is reopened on the same (or the default) device and
    /// keeps feeding `sink`, with silence standing in for the gap so time
    /// stays on the wall clock. `on_lost` is called only when reopening
    /// fails; the sink then gets nothing more. Called on the capture thread.
    pub fn start_with_errors(
        device: Option<&str>,
        sink: impl FnMut(&[f32]) + Send + 'static,
        on_lost: impl FnOnce(String) + Send + 'static,
    ) -> Result<MicCapture, EngineError> {
        MicCapture::start_with_mode(device, MicMode::Plain, sink, on_lost)
    }

    /// [`MicCapture::start_with_errors`] in the given [`MicMode`]. A reopen
    /// after a device change uses the same mode.
    pub fn start_with_mode(
        device: Option<&str>,
        mode: MicMode,
        sink: impl FnMut(&[f32]) + Send + 'static,
        on_lost: impl FnOnce(String) + Send + 'static,
    ) -> Result<MicCapture, EngineError> {
        let wanted = device.map(str::to_string);
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
        let (stop_tx, stop_rx) = crossbeam_channel::bounded(1);
        let thread = std::thread::Builder::new()
            .name("squawk-mic".into())
            .spawn(move || capture_thread(wanted, mode, sink, on_lost, ready_tx, stop_rx))?;
        match ready_rx.recv() {
            Ok(Ok((name, echo_cancelled))) => Ok(MicCapture {
                name,
                echo_cancelled,
                stop_tx,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(EngineError::Mic("the capture thread died".into()))
            }
        }
    }

    /// The device actually opened.
    pub fn device_name(&self) -> &str {
        &self.name
    }

    /// Whether the stream opened at start is echo cancelled
    /// ([`MicMode::EchoCancelled`] that did not fall back).
    pub fn echo_cancelled(&self) -> bool {
        self.echo_cancelled
    }

    /// Stop and join the capture thread. Samples already delivered stay
    /// delivered, and whatever the resampler held back is flushed to the
    /// sink first; nothing arrives after this returns.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let _ = self.stop_tx.try_send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for MicCapture {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The opened device's name and whether it is echo cancelled.
type Ready = Sender<Result<(String, bool), EngineError>>;

type Listen = Box<dyn FnMut(&[f32]) + Send>;

/// The echo-cancelled meeting mic, lent to a dictation. While voice
/// processing runs, macOS hands every other client of that mic a signal
/// ~40 dB down (measured on a MacBook Pro), so a dictation opening its own
/// stream during such a meeting would hear almost nothing. It listens to
/// the meeting's stream instead, which also keeps the call out of it.
#[derive(Clone, Default)]
pub(crate) struct MicShare(Arc<Mutex<ShareState>>);

#[derive(Default)]
struct ShareState {
    live: bool,
    next_id: u64,
    listener: Option<(u64, Listen)>,
}

/// A dictation listening to a [`MicShare`]; dropping it stops listening.
pub(crate) struct MicListener {
    share: MicShare,
    id: u64,
}

impl MicShare {
    fn state(&self) -> std::sync::MutexGuard<'_, ShareState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Called by the meeting when its echo-cancelled mic starts (`true`)
    /// and before it stops or is lost (`false`, which also drops the
    /// listener).
    pub(crate) fn set_live(&self, live: bool) {
        let mut st = self.state();
        st.live = live;
        if !live {
            st.listener = None;
        }
    }

    /// Every 16 kHz block of the meeting mic.
    pub(crate) fn forward(&self, block: &[f32]) {
        if let Some((_, listen)) = self.state().listener.as_mut() {
            listen(block);
        }
    }

    /// Start sending the meeting mic's blocks to `listen`, replacing any
    /// earlier listener. `None` when no echo-cancelled meeting mic is live.
    pub(crate) fn listen(
        &self,
        listen: impl FnMut(&[f32]) + Send + 'static,
    ) -> Option<MicListener> {
        let mut st = self.state();
        if !st.live {
            return None;
        }
        st.next_id += 1;
        let id = st.next_id;
        st.listener = Some((id, Box::new(listen)));
        Some(MicListener {
            share: self.clone(),
            id,
        })
    }
}

impl Drop for MicListener {
    fn drop(&mut self) {
        let mut st = self.share.state();
        if st.listener.as_ref().is_some_and(|(id, _)| *id == self.id) {
            st.listener = None;
        }
    }
}

/// Waits before each attempt to reopen a lost mic. CoreAudio needs a
/// moment after a route change before the new format is readable.
const REOPEN_DELAYS_MS: [u64; 5] = [100, 250, 500, 1000, 2000];
/// A device that keeps dropping out is given up on after this many reopens
/// in one capture, rather than looping forever.
const MAX_REOPENS: u32 = 20;
/// Cap on the silence inserted for a reopen gap.
const MAX_GAP: Duration = Duration::from_secs(10);

/// One opened stream and what its buffers need to become 16 kHz mono.
struct Opened {
    stream: Stream,
    name: String,
    channels: usize,
    resampler: Resampler,
}

impl Opened {
    fn deliver(&mut self, raw: &[f32], sink: &mut impl FnMut(&[f32])) {
        let mono = downmix(raw, self.channels);
        let out = self.resampler.process(&mono);
        if !out.is_empty() {
            sink(&out);
        }
    }

    /// Stop the device, then pass on every buffer it had already sent and
    /// whatever the resampler held back. Nothing of this stream is left in
    /// `raw_rx` afterwards.
    fn close(self, raw_rx: &Receiver<Vec<f32>>, sink: &mut impl FnMut(&[f32])) {
        let Opened {
            stream,
            channels,
            mut resampler,
            ..
        } = self;
        drop(stream);
        for raw in raw_rx.try_iter() {
            let out = resampler.process(&downmix(&raw, channels));
            if !out.is_empty() {
                sink(&out);
            }
        }
        let tail = resampler.flush();
        if !tail.is_empty() {
            sink(&tail);
        }
    }
}

/// Either kind of stream; both stop when dropped, on the thread that opened
/// them.
enum Stream {
    Plain(#[allow(dead_code)] cpal::Stream),
    Voice(#[allow(dead_code)] VoiceInput),
}

fn capture_thread(
    wanted: Option<String>,
    mode: MicMode,
    mut sink: impl FnMut(&[f32]),
    on_lost: impl FnOnce(String),
    ready: Ready,
    stop: Receiver<()>,
) {
    let (raw_tx, raw_rx) = crossbeam_channel::unbounded::<Vec<f32>>();
    // Loss reports carry the generation of the stream that raised them, so
    // a late report from a stream already replaced is ignored.
    let (lost_tx, lost_rx) = crossbeam_channel::unbounded::<(u32, String)>();
    let mut generation = 0u32;
    let first = match open_stream(wanted.as_deref(), mode, &raw_tx, &lost_tx, generation) {
        Ok(opened) => opened,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let echo_cancelled = matches!(first.stream, Stream::Voice(_));
    let _ = ready.send(Ok((first.name.clone(), echo_cancelled)));
    let mut current = Some(first);
    let mut on_lost = Some(on_lost);

    loop {
        select! {
            recv(raw_rx) -> msg => {
                if let (Ok(raw), Some(mic)) = (msg, current.as_mut()) {
                    mic.deliver(&raw, &mut sink);
                }
            }
            recv(lost_rx) -> msg => {
                let Ok((gen, why)) = msg else { continue };
                if gen != generation {
                    continue;
                }
                let Some(old) = current.take() else { continue };
                let lost_at = Instant::now();
                let name = old.name.clone();
                old.close(&raw_rx, &mut sink);
                log::warn!("mic: {name}: {why}; reopening");
                generation += 1;
                let reopened = if generation > MAX_REOPENS {
                    Reopen::Failed("it keeps dropping out".into())
                } else {
                    reopen(wanted.as_deref(), mode, &raw_tx, &lost_tx, generation, &stop)
                };
                match reopened {
                    Reopen::Opened(mic) => {
                        let gap = silence_for(lost_at.elapsed());
                        log::info!(
                            "mic: reopened {} after {} ms",
                            mic.name,
                            lost_at.elapsed().as_millis()
                        );
                        for block in gap.chunks(SAMPLE_RATE as usize / 10) {
                            sink(block);
                        }
                        current = Some(mic);
                    }
                    Reopen::Failed(e) => {
                        log::error!("mic: could not reopen: {e}");
                        if let Some(f) = on_lost.take() {
                            f(format!("{why}; reopening failed: {e}"));
                        }
                    }
                    // Stopped while waiting to retry; the old stream is
                    // already closed and flushed.
                    Reopen::Stopped => return,
                }
            }
            recv(stop) -> _ => break,
        }
    }
    if let Some(mic) = current {
        mic.close(&raw_rx, &mut sink);
    }
}

enum Reopen {
    Opened(Opened),
    Failed(String),
    Stopped,
}

/// Try to open the mic again, waiting a little longer before each attempt.
/// A stop request during a wait ends it.
fn reopen(
    wanted: Option<&str>,
    mode: MicMode,
    raw_tx: &Sender<Vec<f32>>,
    lost_tx: &Sender<(u32, String)>,
    generation: u32,
    stop: &Receiver<()>,
) -> Reopen {
    let mut last = String::from("no attempt");
    for delay in REOPEN_DELAYS_MS {
        if stop.recv_timeout(Duration::from_millis(delay)).is_ok() {
            return Reopen::Stopped;
        }
        match open_stream(wanted, mode, raw_tx, lost_tx, generation) {
            Ok(mic) => return Reopen::Opened(mic),
            Err(e) => {
                log::debug!("mic: reopen attempt failed: {e}");
                last = e.to_string();
            }
        }
    }
    Reopen::Failed(last)
}

/// Zeros covering a reopen gap of `gap`, capped at [`MAX_GAP`].
fn silence_for(gap: Duration) -> Vec<f32> {
    let secs = gap.min(MAX_GAP).as_secs_f64();
    vec![0.0; (secs * SAMPLE_RATE as f64) as usize]
}

fn open_stream(
    wanted: Option<&str>,
    mode: MicMode,
    raw_tx: &Sender<Vec<f32>>,
    lost_tx: &Sender<(u32, String)>,
    generation: u32,
) -> Result<Opened, EngineError> {
    let host = cpal::default_host();
    let device = find_device(&host, wanted)?;
    let name = device_name(&device);
    if mode == MicMode::EchoCancelled {
        // Voice processing always uses the default input.
        let is_default = host
            .default_input_device()
            .is_some_and(|d| device_name(&d) == name);
        if !is_default {
            log::warn!("mic: echo cancellation needs the default input; {name} is recorded as is");
        } else {
            match open_voice(name.clone(), raw_tx, lost_tx, generation) {
                Ok(opened) => return Ok(opened),
                Err(e) => log::warn!("mic: {e}; recording without echo cancellation"),
            }
        }
    }
    let config = device
        .default_input_config()
        .map_err(|e| EngineError::Mic(format!("{name}: {e}")))?;
    let rate = config.sample_rate();
    let channels = config.channels() as usize;
    let format = config.sample_format();
    let stream_config = config.config();
    let lost_tx = lost_tx.clone();
    // cpal pauses the stream itself on DeviceNotAvailable and
    // StreamInvalidated; those need a reopen. Xruns (a late callback) and
    // the rest leave the stream running and are only logged.
    let err_fn = move |e: cpal::Error| match e.kind() {
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated => {
            let _ = lost_tx.send((generation, e.to_string()));
        }
        cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged => log::debug!("mic: {e}"),
        _ => log::warn!("mic: {e}"),
    };
    let raw_tx = raw_tx.clone();
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, stream_config, raw_tx, err_fn),
        SampleFormat::I16 => build::<i16>(&device, stream_config, raw_tx, err_fn),
        SampleFormat::I32 => build::<i32>(&device, stream_config, raw_tx, err_fn),
        SampleFormat::U16 => build::<u16>(&device, stream_config, raw_tx, err_fn),
        SampleFormat::I8 => build::<i8>(&device, stream_config, raw_tx, err_fn),
        SampleFormat::U8 => build::<u8>(&device, stream_config, raw_tx, err_fn),
        SampleFormat::F64 => build::<f64>(&device, stream_config, raw_tx, err_fn),
        other => {
            return Err(EngineError::Mic(format!(
                "{name}: unsupported sample format {other:?}"
            )))
        }
    }
    .map_err(|e| EngineError::Mic(format!("{name}: {e}")))?;
    stream
        .play()
        .map_err(|e| EngineError::Mic(format!("{name}: {e}")))?;
    log::info!("mic: {name} at {rate} Hz, {channels} ch");
    Ok(Opened {
        stream: Stream::Plain(stream),
        name,
        channels,
        resampler: Resampler::new(rate),
    })
}

/// The default input through voice processing; a hardware change is
/// reported like a lost cpal stream, so the capture thread reopens it.
fn open_voice(
    name: String,
    raw_tx: &Sender<Vec<f32>>,
    lost_tx: &Sender<(u32, String)>,
    generation: u32,
) -> Result<Opened, EngineError> {
    let lost_tx = lost_tx.clone();
    let opening = Instant::now();
    let voice = VoiceInput::open(raw_tx.clone(), move || {
        let _ = lost_tx.send((generation, "the audio hardware changed".into()));
    })?;
    let rate = voice.rate();
    log::info!(
        "mic: {name} at {rate} Hz, echo cancelled (opened in {} ms)",
        opening.elapsed().as_millis()
    );
    Ok(Opened {
        stream: Stream::Voice(voice),
        name,
        channels: 1,
        resampler: Resampler::new(rate),
    })
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    raw_tx: Sender<Vec<f32>>,
    err_fn: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _| {
            let block: Vec<f32> = data.iter().map(|&s| f32::from_sample_(s)).collect();
            let _ = raw_tx.send(block);
        },
        err_fn,
        None,
    )
}

/// The named input device, else the default. A named device that is gone
/// falls back to the default (with a log line) rather than failing: the
/// user unplugged a headset and still wants to dictate.
fn find_device(host: &cpal::Host, wanted: Option<&str>) -> Result<cpal::Device, EngineError> {
    if let Some(wanted) = wanted {
        if let Ok(devices) = host.input_devices() {
            for d in devices {
                if device_name(&d) == wanted {
                    return Ok(d);
                }
            }
        }
        log::warn!("mic: input device {wanted:?} not found, using the default");
    }
    host.default_input_device()
        .ok_or_else(|| EngineError::Mic("no input device".into()))
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "unknown device".into())
}

/// Names of the available input devices, default first.
pub fn input_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut out: Vec<String> = Vec::new();
    if let Some(d) = host.default_input_device() {
        out.push(device_name(&d));
    }
    if let Ok(devices) = host.input_devices() {
        for d in devices {
            let name = device_name(&d);
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// Zero crossings of the windowed sinc on each side of the centre tap.
const ZERO_CROSSINGS: f64 = 16.0;
/// Cutoff as a fraction of the lower Nyquist: a little below it, so the
/// window's transition band does not fold back into speech.
const CUTOFF: f64 = 0.92;
/// Cap on the polyphase table for odd rate pairs (44 100 → 16 000 needs
/// 160 phases; a pair with no common factor would need 16 000).
const MAX_PHASES: u64 = 4096;

/// Streaming resampler from any rate to 16 kHz mono: a windowed-sinc
/// (Blackman) interpolator with a precomputed polyphase table. Positions are
/// kept as exact integer ratios, so there is no drift over an hour-long
/// meeting. Zero-phase: output sample n is input time n / 16 000 s.
pub struct Resampler {
    from_rate: u32,
    half: usize,
    phases: u64,
    table: Vec<f32>,
    /// Input history; `buf[0]` is absolute input index `buf_start`.
    buf: Vec<f32>,
    buf_start: i64,
    next_out: u64,
    total_in: u64,
}

impl Resampler {
    pub fn new(from_rate: u32) -> Resampler {
        let from = from_rate.max(1) as u64;
        let to = SAMPLE_RATE as u64;
        let fc = CUTOFF * (to as f64 / from as f64).min(1.0);
        let half = (ZERO_CROSSINGS / fc).ceil() as usize;
        let phases = (to / gcd(from, to)).min(MAX_PHASES);
        let taps = 2 * half;
        let mut table = vec![0f32; phases as usize * taps];
        for p in 0..phases as usize {
            let frac = p as f64 / phases as f64;
            let row = &mut table[p * taps..(p + 1) * taps];
            let mut sum = 0.0;
            let mut vals = vec![0f64; taps];
            for (j, v) in vals.iter_mut().enumerate() {
                // Distance from the output position to input tap j.
                let t = (j as f64 - half as f64 + 1.0) - frac;
                *v = fc * sinc(fc * t) * blackman(t / half as f64);
                sum += *v;
            }
            for (r, v) in row.iter_mut().zip(vals) {
                *r = (v / sum) as f32;
            }
        }
        Resampler {
            from_rate,
            half,
            phases,
            table,
            buf: vec![0.0; half],
            buf_start: -(half as i64),
            next_out: 0,
            total_in: 0,
        }
    }

    pub fn from_rate(&self) -> u32 {
        self.from_rate
    }

    fn passthrough(&self) -> bool {
        self.from_rate == SAMPLE_RATE
    }

    /// Resample a block; may return fewer samples than a full ratio would
    /// suggest (the rest is held for the next block).
    pub fn process(&mut self, mono: &[f32]) -> Vec<f32> {
        self.total_in += mono.len() as u64;
        if self.passthrough() {
            self.next_out += mono.len() as u64;
            return mono.to_vec();
        }
        self.buf.extend_from_slice(mono);
        self.emit(u64::MAX)
    }

    /// Whatever is held back. The stream is over after this.
    pub fn flush(&mut self) -> Vec<f32> {
        if self.passthrough() {
            return Vec::new();
        }
        let from = self.from_rate.max(1) as u64;
        let expected = (self.total_in * SAMPLE_RATE as u64).div_ceil(from);
        self.buf.extend(std::iter::repeat_n(0.0, self.half + 1));
        self.emit(expected)
    }

    fn emit(&mut self, limit: u64) -> Vec<f32> {
        let from = self.from_rate.max(1) as u64;
        let to = SAMPLE_RATE as u64;
        let taps = 2 * self.half;
        let end = self.buf_start + self.buf.len() as i64;
        let mut out = Vec::new();
        while self.next_out < limit {
            let num = self.next_out * from;
            let i0 = (num / to) as i64;
            if i0 + self.half as i64 >= end {
                break;
            }
            let phase = ((num % to) * self.phases / to) as usize;
            let coeffs = &self.table[phase * taps..(phase + 1) * taps];
            let first = (i0 - self.half as i64 + 1 - self.buf_start) as usize;
            let window = &self.buf[first..first + taps];
            let v: f32 = window.iter().zip(coeffs).map(|(x, c)| x * c).sum();
            out.push(v);
            self.next_out += 1;
        }
        // Keep only the history the next output needs.
        let next_i0 = ((self.next_out * from) / to) as i64;
        let keep_from = next_i0 - self.half as i64 + 1;
        let drop_n = (keep_from - self.buf_start).clamp(0, self.buf.len() as i64) as usize;
        if drop_n > 0 {
            self.buf.drain(..drop_n);
            self.buf_start += drop_n as i64;
        }
        out
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// Blackman window over x in [-1, 1].
fn blackman(x: f64) -> f64 {
    if x.abs() > 1.0 {
        0.0
    } else {
        0.42 + 0.5 * (PI * x).cos() + 0.08 * (2.0 * PI * x).cos()
    }
}

/// Average interleaved channels to mono.
pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Resample a whole clip to 16 kHz.
pub fn resample_all(mono: &[f32], from_rate: u32) -> Vec<f32> {
    let mut r = Resampler::new(from_rate);
    let mut out = r.process(mono);
    out.extend(r.flush());
    out
}

/// Any audio file macOS can read, as 16 kHz mono. WAV is read directly
/// (hound); anything else goes through `/usr/bin/afconvert` to a temp WAV.
pub fn load_file(path: &Path) -> Result<Vec<f32>, EngineError> {
    let fail = |message: String| EngineError::AudioFile {
        path: path.to_path_buf(),
        message,
    };
    if !path.is_file() {
        return Err(fail("no such file".into()));
    }
    if let Ok(samples) = read_wav(path) {
        return Ok(samples);
    }
    let tmp = temp_wav_path();
    let status = std::process::Command::new("/usr/bin/afconvert")
        .args(["-f", "WAVE", "-d", "LEF32@16000", "-c", "1"])
        .arg(path)
        .arg(&tmp)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| fail(format!("afconvert: {e}")))?;
    if !status.status.success() {
        let _ = std::fs::remove_file(&tmp);
        let msg = String::from_utf8_lossy(&status.stderr).trim().to_string();
        return Err(fail(if msg.is_empty() {
            "not an audio file macOS can read".into()
        } else {
            msg
        }));
    }
    let out = read_wav(&tmp).map_err(fail);
    let _ = std::fs::remove_file(&tmp);
    out
}

fn temp_wav_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("squawk-{}-{nanos}.wav", std::process::id()))
}

/// A WAV of any rate, depth and channel count, as 16 kHz mono.
fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?
        }
    };
    let mono = downmix(&interleaved, spec.channels as usize);
    Ok(resample_all(&mono, spec.sample_rate))
}

/// Write 16 kHz mono samples as a 16-bit WAV (for `keep_audio`).
pub fn write_wav(path: &Path, samples: &[f32]) -> Result<(), EngineError> {
    let fail = |e: hound::Error| EngineError::AudioFile {
        path: path.to_path_buf(),
        message: e.to_string(),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut w = hound::WavWriter::create(path, wav_spec()).map_err(fail)?;
    for &s in samples {
        w.write_sample(to_i16(s)).map_err(fail)?;
    }
    w.finalize().map_err(fail)
}

pub(crate) fn wav_spec() -> hound::WavSpec {
    hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

pub(crate) fn to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

/// RMS of a block, 0 for an empty one.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shared_mic_reaches_one_listener_while_live() {
        let share = MicShare::default();
        let got = Arc::new(Mutex::new(Vec::new()));
        let sink = |got: &Arc<Mutex<Vec<f32>>>| {
            let got = got.clone();
            move |b: &[f32]| got.lock().unwrap().extend_from_slice(b)
        };
        assert!(share.listen(sink(&got)).is_none(), "nothing to lend yet");

        share.set_live(true);
        share.forward(&[0.1]);
        let first = share.listen(sink(&got)).unwrap();
        share.forward(&[0.2]);
        // A second listener replaces the first; dropping the stale handle
        // does not detach the new one.
        let other = Arc::new(Mutex::new(Vec::new()));
        let second = share.listen(sink(&other)).unwrap();
        share.forward(&[0.3]);
        drop(first);
        share.forward(&[0.4]);
        drop(second);
        share.forward(&[0.5]);
        assert_eq!(*got.lock().unwrap(), vec![0.2]);
        assert_eq!(*other.lock().unwrap(), vec![0.3, 0.4]);

        let _third = share.listen(sink(&got)).unwrap();
        share.set_live(false);
        share.forward(&[0.6]);
        assert_eq!(*got.lock().unwrap(), vec![0.2]);
        assert!(share.listen(sink(&got)).is_none());
    }

    fn sine(freq: f32, rate: u32, secs: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin() * 0.5)
            .collect()
    }

    /// Amplitude of `freq` in `x` by correlation (a one-bin DFT).
    fn tone_amplitude(x: &[f32], freq: f32, rate: u32) -> f32 {
        let (mut re, mut im) = (0f64, 0f64);
        for (i, &v) in x.iter().enumerate() {
            let a = 2.0 * std::f64::consts::PI * freq as f64 * i as f64 / rate as f64;
            re += v as f64 * a.cos();
            im += v as f64 * a.sin();
        }
        (2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32
    }

    /// Streaming in odd block sizes, then flushing.
    fn stream(r: &mut Resampler, x: &[f32], block: usize) -> Vec<f32> {
        let mut out = Vec::new();
        for chunk in x.chunks(block) {
            out.extend(r.process(chunk));
        }
        out.extend(r.flush());
        out
    }

    #[test]
    fn length_follows_the_rate_ratio() {
        for (rate, n_in) in [(48_000u32, 48_000usize), (44_100, 44_100), (8_000, 8_000)] {
            let x = vec![0.1f32; n_in];
            let out = stream(&mut Resampler::new(rate), &x, 479);
            assert_eq!(out.len(), 16_000, "from {rate}");
        }
        // A partial second rounds up.
        let out = stream(&mut Resampler::new(48_000), &vec![0.0; 100], 7);
        assert_eq!(out.len(), 34);
    }

    #[test]
    fn a_sine_keeps_its_frequency_and_level() {
        for rate in [48_000u32, 44_100, 22_050, 96_000] {
            let x = sine(440.0, rate, 1.0);
            let out = stream(&mut Resampler::new(rate), &x, 512);
            // Skip the edges where the kernel sees the implicit zeros.
            let mid = &out[1000..15_000];
            let at = tone_amplitude(mid, 440.0, 16_000);
            let off = tone_amplitude(mid, 523.0, 16_000);
            assert!((at - 0.5).abs() < 0.02, "{rate}: 440 Hz at {at}");
            assert!(off < 0.01, "{rate}: leak {off}");
        }
    }

    #[test]
    fn frequencies_above_8k_are_filtered_out() {
        // 11 kHz would alias to 5 kHz without the low-pass.
        let x = sine(11_000.0, 48_000, 1.0);
        let out = stream(&mut Resampler::new(48_000), &x, 480);
        let alias = tone_amplitude(&out[1000..15_000], 5_000.0, 16_000);
        assert!(alias < 0.005, "alias {alias}");
    }

    #[test]
    fn block_size_does_not_change_the_output() {
        let x = sine(300.0, 44_100, 0.5);
        let a = stream(&mut Resampler::new(44_100), &x, 1);
        let b = stream(&mut Resampler::new(44_100), &x, 4410);
        assert_eq!(a.len(), b.len());
        for (p, q) in a.iter().zip(&b) {
            assert!((p - q).abs() < 1e-6);
        }
    }

    #[test]
    fn sixteen_k_passes_through() {
        let x = sine(300.0, 16_000, 0.1);
        let out = stream(&mut Resampler::new(16_000), &x, 100);
        assert_eq!(out, x);
    }

    #[test]
    fn a_reopen_gap_becomes_capped_silence() {
        assert_eq!(silence_for(Duration::from_millis(250)).len(), 4_000);
        assert!(silence_for(Duration::from_millis(250))
            .iter()
            .all(|&s| s == 0.0));
        assert_eq!(
            silence_for(Duration::from_secs(60)).len(),
            MAX_GAP.as_secs() as usize * SAMPLE_RATE as usize
        );
    }

    #[test]
    fn downmix_averages_frames() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert!((downmix(&[0.3, 0.6, 0.9], 3)[0] - 0.6).abs() < 1e-6);
        assert_eq!(downmix(&[0.1, 0.2], 1), vec![0.1, 0.2]);
    }

    #[test]
    fn wav_roundtrip_through_load_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.wav");
        let x = sine(440.0, 16_000, 0.5);
        write_wav(&path, &x).unwrap();
        let back = load_file(&path).unwrap();
        assert_eq!(back.len(), x.len());
        assert!(back.iter().zip(&x).all(|(a, b)| (a - b).abs() < 1e-3));
    }

    #[test]
    fn stereo_48k_wav_is_converted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for s in sine(440.0, 48_000, 1.0) {
            w.write_sample(s).unwrap();
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
        let back = load_file(&path).unwrap();
        assert_eq!(back.len(), 16_000);
        assert!((tone_amplitude(&back[1000..15_000], 440.0, 16_000) - 0.5).abs() < 0.02);
    }

    #[test]
    fn load_file_reports_missing_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            load_file(&dir.path().join("nope.wav")),
            Err(EngineError::AudioFile { .. })
        ));
        let junk = dir.path().join("junk.mp3");
        std::fs::write(&junk, b"not audio at all").unwrap();
        assert!(matches!(
            load_file(&junk),
            Err(EngineError::AudioFile { .. })
        ));
    }
}
