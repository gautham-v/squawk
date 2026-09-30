//! The model half of cleanup: "S1-mini" by "Superwhisper", a 0.6B text
//! normalizer for speech-to-text output (Apache 2.0 with a naming term; see
//! the README). It rewrites a raw transcript as written text — fillers out,
//! false starts and self-corrections resolved, sentences mended where
//! squawk's cuts broke them, numbers written as numbers.
//!
//! The engine runs it (llama.cpp, `squawk_engine::normalizer`); this module
//! holds everything about it that needs no model, so it is tested here:
//! - [`chat_prompt`]: the exact input the model was trained on. The system
//!   prompt, the control line and the empty think block are part of the
//!   format; change any of them and the model can garble its output.
//! - [`accept`]: whether an output is safe to paste instead of the rules
//!   alone (not implausibly long, not emptied a real dictation).
//! - [`Normalizer`]: what `pipeline::finish_with` calls, so the pipeline is
//!   tested with a fake.
//!
//! Anything that goes wrong falls back to the rule-based pipeline: a
//! dictation is never lost to the model.

use std::time::Duration;

use crate::cleanup::{self, CleanupOptions};

/// The model's name, exactly as its license requires it to be shown.
pub const MODEL_NAME: &str = "S1-mini";
/// Its maker, likewise.
pub const MODEL_MAKER: &str = "Superwhisper";

/// The system prompt S1-mini was trained with. Byte for byte.
pub const SYSTEM_PROMPT: &str = "You are a text normalizer for speech-to-text transcripts. The input begins with a control line specifying the styling, structure, and context settings; clean the transcript to match those settings and output only the cleaned text.";

/// Standard written English, contractions kept; no bullet lists; no email
/// layout. Every value must be one the model was trained on.
pub const CONTROL_LINE: &str = "[Styling: semi-formal] [Structure: prose] [Context: general]";

/// How long a dictation waits for the model before pasting the rules-only
/// text instead. Measured on an M4: ~0.1–0.25 s for a sentence, ~1.4 s for
/// a 90-second dictation, so 4 s only trips on something unusual (a cold
/// GPU, a machine under load).
pub const BUDGET: Duration = Duration::from_secs(4);

/// Inputs longer than this many tokens (about 450 words, three minutes of
/// talking) skip the model rather than wait out [`BUDGET`] for nothing: the
/// answer is about as long as the input and comes at ~170 tokens a second.
pub const MAX_INPUT_TOKENS: usize = 600;

/// Room for the answer: a cleaned text is shorter than its transcript, so
/// twice the input plus a little is a cap that only a runaway output hits.
pub fn max_new_tokens(input_tokens: usize) -> usize {
    input_tokens * 2 + 32
}

/// The full ChatML text for `transcript`, ending where the model's answer
/// begins (after the empty think block S1-mini expects: thinking off).
pub fn chat_prompt(transcript: &str) -> String {
    format!(
        "<|im_start|>system\n{SYSTEM_PROMPT}<|im_end|>\n<|im_start|>user\n{CONTROL_LINE}\n{}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n",
        transcript.trim()
    )
}

/// The answer without template leftovers: a stray think block, an end
/// marker, surrounding whitespace.
pub fn tidy_output(output: &str) -> String {
    let mut text = output;
    if let Some(end) = text.find("</think>") {
        text = &text[end + "</think>".len()..];
    }
    if let Some(end) = text.find("<|im_end|>") {
        text = &text[..end];
    }
    text.trim().to_string()
}

/// Why an output was not used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    /// Far longer than what was said: the model is inventing.
    TooLong,
    /// Empty, though the rules alone leave several words: a filler-only
    /// input legitimately comes back empty, a real dictation must not.
    Emptied,
}

impl Rejected {
    pub fn label(self) -> &'static str {
        match self {
            Rejected::TooLong => "too_long",
            Rejected::Emptied => "emptied",
        }
    }
}

/// `output` (already [`tidy_output`]ed) if it may replace `input`.
pub fn accept(input: &str, output: &str) -> Result<String, Rejected> {
    let words = |s: &str| s.split_whitespace().count();
    let (have, got) = (words(input), words(output));
    if got as f32 > have as f32 * 1.5 + 10.0 {
        return Err(Rejected::TooLong);
    }
    if got == 0 {
        let by_rules = cleanup::strip(input, &CleanupOptions::default());
        if words(&by_rules) > 2 {
            return Err(Rejected::Emptied);
        }
    }
    Ok(output.to_string())
}

/// Why the model could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizeError {
    /// Not downloaded, still loading, or turned off.
    NotReady,
    /// More than [`MAX_INPUT_TOKENS`].
    TooLong,
    /// No answer within [`BUDGET`].
    Timeout,
    Failed(String),
}

impl NormalizeError {
    /// One token for the log line.
    pub fn label(&self) -> &'static str {
        match self {
            NormalizeError::NotReady => "not_ready",
            NormalizeError::TooLong => "input_too_long",
            NormalizeError::Timeout => "timeout",
            NormalizeError::Failed(_) => "error",
        }
    }
}

/// Something that turns a raw transcript into S1-mini's cleaned text,
/// blocking at most about [`BUDGET`]. The engine's model; a fake in tests.
pub trait Normalizer {
    fn normalize(&self, transcript: &str) -> Result<String, NormalizeError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_is_the_trained_format() {
        let p = chat_prompt("  so um i need to send it  ");
        assert_eq!(
            p,
            format!(
                "<|im_start|>system\n{SYSTEM_PROMPT}<|im_end|>\n\
                 <|im_start|>user\n[Styling: semi-formal] [Structure: prose] [Context: general]\n\
                 so um i need to send it<|im_end|>\n\
                 <|im_start|>assistant\n<think>\n\n</think>\n\n"
            )
        );
        assert!(p.ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n"));
    }

    #[test]
    fn output_leftovers_are_tidied() {
        assert_eq!(tidy_output("  Hello there.\n"), "Hello there.");
        assert_eq!(
            tidy_output("<think>\n\n</think>\n\nI need to send it.<|im_end|>"),
            "I need to send it."
        );
        assert_eq!(tidy_output(""), "");
    }

    #[test]
    fn a_runaway_answer_is_rejected() {
        let input = "what is seventeen times twenty three";
        assert_eq!(
            accept(input, "What is 17 times 23?").unwrap(),
            "What is 17 times 23?"
        );
        let essay = "word ".repeat(30);
        assert_eq!(accept(input, &essay), Err(Rejected::TooLong));
        // Short inputs get room: 4 words may come back as up to 16.
        assert!(accept(
            "send it by thursday",
            "Send it by Thursday, the 2nd of October, at 3 PM."
        )
        .is_ok());
    }

    #[test]
    fn empty_is_fine_for_fillers_only() {
        assert_eq!(accept("Um. Uh.", "").unwrap(), "");
        assert_eq!(accept("um, you know", "").unwrap(), "");
        assert_eq!(
            accept("ship the popover change today", ""),
            Err(Rejected::Emptied)
        );
    }

    #[test]
    fn token_caps() {
        assert_eq!(max_new_tokens(0), 32);
        assert_eq!(max_new_tokens(250), 532);
        assert_eq!(max_new_tokens(MAX_INPUT_TOKENS), 1232);
    }
}
