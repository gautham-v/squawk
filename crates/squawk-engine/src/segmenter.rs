//! Cutting a live stream at pauses. Pure: samples in, segments out, so the
//! cut rules are unit-tested without a mic.
//!
//! Frames of 20 ms are classed as speech or silence by RMS against an
//! adaptive threshold (a running noise floor × 3, never below
//! `min_speech_rms`). Once at least `min_segment` of audio is buffered, the
//! first run of silence at least `pause` long is a cut point: everything up
//! to the middle of that pause is committed. If `max_segment` is reached
//! with no pause, cut at the quietest frame in its last 2 s. What is left at
//! `finish` is the tail.
//!
//! A committed segment that is all silence is dropped (not sent to the
//! model), which also keeps Parakeet from hallucinating on empty input.

/// Tunables. The defaults are what dictation uses; meetings use
/// `min_segment = chunk_secs - 5`, `max_segment = chunk_secs + 5`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmenterConfig {
    pub sample_rate: u32,
    /// Do not cut before this much audio (seconds).
    pub min_segment: f32,
    /// Always cut by this much (seconds).
    pub max_segment: f32,
    /// A silence at least this long is a cut point (seconds).
    pub pause: f32,
    /// Floor for the speech threshold (RMS, 0..1).
    pub min_speech_rms: f32,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        SegmenterConfig {
            sample_rate: crate::SAMPLE_RATE,
            min_segment: 3.0,
            max_segment: 20.0,
            pause: 0.35,
            min_speech_rms: 0.004,
        }
    }
}

/// A committed piece of audio and where it starts in the stream.
#[derive(Debug, Clone, PartialEq)]
pub struct Cut {
    /// Offset of the first sample from the stream start, in samples.
    pub start: usize,
    pub samples: Vec<f32>,
    /// Whether any frame in it was speech.
    pub has_speech: bool,
}

pub struct Segmenter {
    config: SegmenterConfig,
}

impl Segmenter {
    pub fn new(config: SegmenterConfig) -> Segmenter {
        Segmenter { config }
    }

    pub fn config(&self) -> &SegmenterConfig {
        &self.config
    }

    /// Feed samples; returns any segments that became final.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Cut> {
        let _ = samples;
        todo!("engine agent")
    }

    /// Everything not yet committed.
    pub fn finish(self) -> Option<Cut> {
        todo!("engine agent")
    }
}
