//! System audio for meetings ("Them"), via ScreenCaptureKit audio-only
//! capture (macOS 13+; we target 15).
//!
//! An `SCStream` on the main display with `captures_audio = true`, sample
//! rate 16 000 and one channel (SCK resamples for us), and the smallest
//! video size it accepts (2×2, 1 fps) since video cannot be switched off
//! entirely; the screen frames go to a no-op handler (without one SCK logs a
//! complaint per frame). SCK delivers buffers continuously, silent ones
//! included. Needs the "Screen & System Audio Recording" permission;
//! without it `start` fails with `SystemAudio`.
//!
//! (A Core Audio process tap, macOS 14.2+, is the alternative if SCK proves
//! unreliable; it would live behind this same API.)

use std::sync::Mutex;

use screencapturekit::prelude::*;

use crate::audio::{downmix, Resampler};
use crate::error::EngineError;
use crate::SAMPLE_RATE;

/// A running system-audio capture. `Send`. Dropping it stops capture.
pub struct SystemAudioCapture {
    stream: Option<SCStream>,
}

impl SystemAudioCapture {
    /// Start capturing; `sink` gets 16 kHz mono blocks on an SCK queue
    /// thread and must not block.
    pub fn start(
        sink: impl FnMut(&[f32]) + Send + 'static,
    ) -> Result<SystemAudioCapture, EngineError> {
        let fail = |what: &str, e: &dyn std::fmt::Display| {
            EngineError::SystemAudio(format!("{what}: {e}"))
        };
        if !has_permission() {
            return Err(EngineError::SystemAudio(
                "needs Screen & System Audio Recording permission".into(),
            ));
        }
        let content = SCShareableContent::get().map_err(|e| fail("shareable content", &e))?;
        let display = content
            .displays()
            .into_iter()
            .next()
            .ok_or_else(|| EngineError::SystemAudio("no display to attach to".into()))?;
        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build()
            .map_err(|e| fail("content filter", &e))?;
        let config = SCStreamConfiguration::new()
            .with_width(2)
            .with_height(2)
            .with_minimum_frame_interval(&CMTime::new(1, 1))
            .with_captures_audio(true)
            // Squawk makes no sound, and "current process" means the whole
            // responsible app: run from a terminal, `true` would also mute
            // every other program started from that terminal.
            .with_excludes_current_process_audio(false)
            .with_sample_rate(SAMPLE_RATE as i32)
            .with_channel_count(1);
        let mut stream = SCStream::new(&filter, &config).map_err(|e| fail("stream", &e))?;
        let converter = Mutex::new(Converter {
            sink: Box::new(sink),
            resampler: None,
        });
        stream
            .add_output_handler(
                move |buf: CMSampleBuffer, _: SCStreamOutputType| {
                    if let Ok(mut c) = converter.lock() {
                        c.push(&buf);
                    }
                },
                SCStreamOutputType::Audio,
            )
            .map_err(|e| fail("audio handler", &e))?;
        stream
            .add_output_handler(
                |_: CMSampleBuffer, _: SCStreamOutputType| {},
                SCStreamOutputType::Screen,
            )
            .map_err(|e| fail("screen handler", &e))?;
        stream.start_capture().map_err(|e| fail("start", &e))?;
        log::info!("system audio: capturing");
        Ok(SystemAudioCapture {
            stream: Some(stream),
        })
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(stream) = self.stream.take() {
            if let Err(e) = stream.stop_capture() {
                log::warn!("system audio: stop: {e}");
            }
        }
    }
}

impl Drop for SystemAudioCapture {
    fn drop(&mut self) {
        self.shutdown();
    }
}

type Sink = Box<dyn FnMut(&[f32]) + Send>;

/// Turns SCK's audio buffers into 16 kHz mono blocks.
struct Converter {
    sink: Sink,
    /// Only if SCK hands us a rate other than the one asked for.
    resampler: Option<Resampler>,
}

impl Converter {
    fn push(&mut self, buf: &CMSampleBuffer) {
        let Some(format) = buf.format_description() else {
            return;
        };
        let rate = format.audio_sample_rate().unwrap_or(SAMPLE_RATE as f64) as u32;
        let is_float = format.audio_is_float();
        let Ok(list) = buf.audio_buffer_list() else {
            return;
        };
        let mut channels: Vec<Vec<f32>> = Vec::new();
        for b in list.iter() {
            let samples = decode(b.data(), is_float);
            let n = b.number_channels.max(1) as usize;
            channels.push(downmix(&samples, n));
        }
        let mono = average(&channels);
        if mono.is_empty() {
            return;
        }
        let out = if rate == SAMPLE_RATE {
            mono
        } else {
            let r = self.resampler.get_or_insert_with(|| Resampler::new(rate));
            r.process(&mono)
        };
        if !out.is_empty() {
            (self.sink)(&out);
        }
    }
}

/// Native-endian f32 or i16 PCM bytes as f32.
fn decode(bytes: &[u8], is_float: bool) -> Vec<f32> {
    if is_float {
        let (words, _) = bytes.as_chunks::<4>();
        words.iter().map(|w| f32::from_ne_bytes(*w)).collect()
    } else {
        let (words, _) = bytes.as_chunks::<2>();
        words
            .iter()
            .map(|w| i16::from_ne_bytes(*w) as f32 / 32_768.0)
            .collect()
    }
}

/// Non-interleaved channels (one buffer each) averaged to one.
fn average(channels: &[Vec<f32>]) -> Vec<f32> {
    match channels {
        [] => Vec::new(),
        [one] => one.clone(),
        many => {
            let n = many.iter().map(Vec::len).min().unwrap_or(0);
            (0..n)
                .map(|i| many.iter().map(|c| c[i]).sum::<f32>() / many.len() as f32)
                .collect()
        }
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
}

/// Whether the Screen & System Audio Recording permission is granted
/// (CGPreflightScreenCaptureAccess). Does not prompt.
pub fn has_permission() -> bool {
    // SAFETY: a plain query with no arguments, callable from any thread.
    unsafe { CGPreflightScreenCaptureAccess() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_float_and_int_pcm() {
        let f: Vec<u8> = [0.5f32, -0.25]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        assert_eq!(decode(&f, true), vec![0.5, -0.25]);
        let i: Vec<u8> = [16_384i16, -32_768]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        assert_eq!(decode(&i, false), vec![0.5, -1.0]);
    }

    #[test]
    fn averages_planar_channels() {
        assert_eq!(average(&[]), Vec::<f32>::new());
        assert_eq!(average(&[vec![0.2, 0.4]]), vec![0.2, 0.4]);
        assert_eq!(
            average(&[vec![0.0, 1.0], vec![1.0, 0.0, 9.0]]),
            vec![0.5, 0.5]
        );
    }
}
