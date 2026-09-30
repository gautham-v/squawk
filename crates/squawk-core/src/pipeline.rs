//! Raw transcript to pasted text, in the one order that works:
//!
//! 1. `cleanup::strip` — fillers and stutters, on the model's own words;
//! 2. `context::apply` — @mentions and repo jargon (Claude Code mode only);
//! 3. `Dictionary::apply` — the user's words win over the repo's;
//! 4. `cleanup::finalize` — capital first letter, end punctuation, spacing.
//!
//! An empty result means "nothing to paste".

use crate::cleanup::{self, CleanupOptions};
use crate::context::{self, Context};
use crate::dictionary::Dictionary;

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

#[cfg(test)]
mod tests {
    use super::*;

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
