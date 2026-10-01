//! Raw transcript to pasted text, in the one order that works:
//!
//! 0. `normalize` — S1-mini rewrites the raw transcript as written text,
//!    when it is on and answers in time ([`finish_with`]). It sees the
//!    model's own words, before anything below touches them: it does far
//!    better on Parakeet's raw output than on already-stripped text;
//! 1. `cleanup::strip` — fillers and stutters (after S1-mini, it catches a
//!    ", like," the model kept);
//! 2. `context::apply` — @mentions and repo jargon (Claude Code mode only);
//! 3. `Dictionary::apply` — the user's words win over the repo's and the
//!    model's;
//! 4. `cleanup::finalize` — capital first letter, end punctuation, spacing.
//!
//! An empty result means "nothing to paste".
//!
//! A meeting segment gets less ([`meeting_segment`]): hesitations and
//! stutters out, then the dictionary.

use std::time::{Duration, Instant};

use crate::cleanup::{self, CleanupOptions};
use crate::context::{self, Context};
use crate::dictionary::Dictionary;
use crate::normalize::{self, NormalizeError, Normalizer};

/// Which way a dictation was cleaned, for the log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cleanup {
    /// S1-mini is off (or not downloaded yet): the rules alone.
    Rules,
    /// S1-mini's text went on through the rest of the pipeline.
    Model,
    /// S1-mini was asked but its answer was not used: why.
    Fallback(&'static str),
}

impl Cleanup {
    /// `rules`, `s1`, `fallback:timeout`.
    pub fn label(self) -> String {
        match self {
            Cleanup::Rules => "rules".into(),
            Cleanup::Model => "s1".into(),
            Cleanup::Fallback(why) => format!("fallback:{why}"),
        }
    }
}

/// The pasted text and how it got that way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub text: String,
    pub cleanup: Cleanup,
    /// Time spent waiting on S1-mini (zero without it).
    pub model_time: Duration,
    /// What S1-mini answered, tidied, when it answered (used or not).
    pub model_text: Option<String>,
}

/// [`finish`], with S1-mini first when `normalizer` is given. Whatever the
/// model does wrong (not ready, too slow, an implausible answer) the text is
/// what [`finish`] alone would give.
pub fn finish_with(
    raw: &str,
    normalizer: Option<&dyn Normalizer>,
    dictionary: &Dictionary,
    context: Option<&Context>,
    opts: &CleanupOptions,
) -> Finished {
    let rules = |cleanup, model_time, model_text| Finished {
        text: finish(raw, dictionary, context, opts),
        cleanup,
        model_time,
        model_text,
    };
    let Some(normalizer) = normalizer else {
        return rules(Cleanup::Rules, Duration::ZERO, None);
    };
    if raw.trim().is_empty() {
        return rules(Cleanup::Rules, Duration::ZERO, None);
    }
    let asked = Instant::now();
    let answer = normalizer.normalize(raw);
    let model_time = asked.elapsed();
    let out = match answer {
        Ok(out) => normalize::tidy_output(&out),
        Err(NormalizeError::NotReady) => return rules(Cleanup::Rules, model_time, None),
        Err(e) => return rules(Cleanup::Fallback(e.label()), model_time, None),
    };
    match normalize::accept(raw, &out) {
        Ok(text) => Finished {
            text: finish(&text, dictionary, context, opts),
            cleanup: Cleanup::Model,
            model_time,
            model_text: Some(out),
        },
        Err(why) => rules(Cleanup::Fallback(why.label()), model_time, Some(out)),
    }
}

pub fn finish(
    raw: &str,
    dictionary: &Dictionary,
    context: Option<&Context>,
    opts: &CleanupOptions,
) -> String {
    let mut text = cleanup::strip(raw, opts);
    if text.is_empty() {
        return text;
    }
    if let Some(ctx) = context {
        text = context::apply(&text, ctx);
    }
    text = dictionary.apply(&text);
    cleanup::finalize(&text)
}

/// One segment of a meeting transcript: hesitations and stutters go
/// (`cleanup::strip_hesitations`), then the dictionary. No S1-mini (it is
/// made for a dictation, and a meeting is hours of them), no Claude Code
/// mode, and no `finalize`: segments are joined into one block, so a
/// segment's edges are not a sentence's. Empty means "drop the segment".
pub fn meeting_segment(raw: &str, dictionary: &Dictionary) -> String {
    let text = cleanup::strip_hesitations(raw);
    if text.is_empty() {
        return text;
    }
    dictionary.apply(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Answers with a fixed result and remembers what it was given.
    struct Fake {
        answer: Result<String, NormalizeError>,
        seen: RefCell<Vec<String>>,
    }

    impl Fake {
        fn says(answer: Result<&str, NormalizeError>) -> Fake {
            Fake {
                answer: answer.map(str::to_string),
                seen: RefCell::default(),
            }
        }
    }

    impl Normalizer for Fake {
        fn normalize(&self, transcript: &str) -> Result<String, NormalizeError> {
            self.seen.borrow_mut().push(transcript.to_string());
            self.answer.clone()
        }
    }

    const RAW: &str = "so um i need to like send the the report to cloud code by uh friday no wait make that thursday";

    #[test]
    fn the_model_sees_the_raw_text_and_the_rest_runs_after_it() {
        let dict = Dictionary::parse("cloud code -> Claude Code\n");
        let fake = Fake::says(Ok(
            "<think>\n\n</think>\n\nI need to send the report, like, to cloud code by Thursday",
        ));
        let out = finish_with(RAW, Some(&fake), &dict, None, &CleanupOptions::default());
        // Raw, not stripped: the model gets the ums and the stutter.
        assert_eq!(*fake.seen.borrow(), vec![RAW.to_string()]);
        // Then strip (the comma-set-off "like"), the dictionary, finalize.
        assert_eq!(
            out.text,
            "I need to send the report to Claude Code by Thursday."
        );
        assert_eq!(out.cleanup, Cleanup::Model);
    }

    #[test]
    fn a_meeting_segment_gets_the_dictionary_and_keeps_its_edges() {
        let dict = Dictionary::parse("front word -> Frontward\nGotham -> Gautham\n");
        assert_eq!(
            meeting_segment("um, I'm Gotham, I I work at front word and", &dict),
            "I'm Gautham, I work at Frontward and"
        );
        assert_eq!(meeting_segment("Uh, um.", &dict), "");
    }

    #[test]
    fn without_a_model_it_is_the_rules() {
        let dict = Dictionary::default();
        let out = finish_with(RAW, None, &dict, None, &CleanupOptions::default());
        assert_eq!(
            out.text,
            finish(RAW, &dict, None, &CleanupOptions::default())
        );
        assert_eq!(out.cleanup, Cleanup::Rules);
        assert_eq!(out.model_time, Duration::ZERO);
        let not_ready = Fake::says(Err(NormalizeError::NotReady));
        let out = finish_with(
            RAW,
            Some(&not_ready),
            &dict,
            None,
            &CleanupOptions::default(),
        );
        assert_eq!(out.cleanup, Cleanup::Rules);
    }

    #[test]
    fn every_failure_falls_back_to_the_rules_text() {
        let dict = Dictionary::default();
        let opts = CleanupOptions::default();
        let rules = finish(RAW, &dict, None, &opts);
        let cases = [
            (Err(NormalizeError::Timeout), "fallback:timeout"),
            (Err(NormalizeError::TooLong), "fallback:input_too_long"),
            (Err(NormalizeError::Failed("gpu".into())), "fallback:error"),
            (Ok(""), "fallback:emptied"),
        ];
        for (answer, label) in cases {
            let fake = Fake::says(answer);
            let out = finish_with(RAW, Some(&fake), &dict, None, &opts);
            assert_eq!(out.text, rules, "{label}");
            assert_eq!(out.cleanup.label(), label);
        }
        let essay = "blah ".repeat(80);
        let fake = Fake::says(Ok(&essay));
        let out = finish_with(RAW, Some(&fake), &dict, None, &opts);
        assert_eq!(out.text, rules);
        assert_eq!(out.cleanup.label(), "fallback:too_long");
    }

    #[test]
    fn a_fillers_only_dictation_may_come_back_empty() {
        let fake = Fake::says(Ok(""));
        let out = finish_with(
            "Um. Uh.",
            Some(&fake),
            &Dictionary::default(),
            None,
            &CleanupOptions::default(),
        );
        assert_eq!(out.text, "");
        assert_eq!(out.cleanup, Cleanup::Model);
    }

    #[test]
    fn nothing_said_never_asks_the_model() {
        let fake = Fake::says(Ok("Invented."));
        let out = finish_with(
            "  ",
            Some(&fake),
            &Dictionary::default(),
            None,
            &CleanupOptions::default(),
        );
        assert_eq!(out.text, "");
        assert!(fake.seen.borrow().is_empty());
    }

    #[test]
    fn cleans_then_applies_dictionary_then_finalizes() {
        let dict = Dictionary::parse("cloud code -> Claude Code\nkubectl\n");
        let out = finish(
            "um, so ask cloud code to run kubectl apply",
            &dict,
            None,
            &CleanupOptions::default(),
        );
        assert_eq!(out, "Ask Claude Code to run kubectl apply.");
    }

    #[test]
    fn dictionary_can_lowercase_the_first_word_but_finalize_capitalizes_it() {
        // The user's casing applies mid-sentence; the first letter of a
        // dictation is always a capital unless the word has inner case.
        let dict = Dictionary::parse("kubectl\n");
        let out = finish("Kubectl get pods", &dict, None, &CleanupOptions::default());
        assert_eq!(out, "Kubectl get pods.");
    }

    #[test]
    fn nothing_said_is_nothing_pasted() {
        let out = finish(
            "Um.",
            &Dictionary::default(),
            None,
            &CleanupOptions::default(),
        );
        assert_eq!(out, "");
    }
}
