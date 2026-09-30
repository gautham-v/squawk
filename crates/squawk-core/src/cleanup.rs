//! Rule-based cleanup of a raw transcript. No model.
//!
//! Two halves, because Claude Code mode and the dictionary run between them:
//! [`strip`] takes out disfluencies (fillers, stutters) from the raw text, and
//! [`finalize`] fixes the edges (first letter, end punctuation, spacing) once
//! everything else has had its say.
//!
//! The rules are deliberately timid. A filler word is only removed when the
//! transcript itself marks it as one — set off by commas, or standing at the
//! start of a sentence with a comma after it. "I like Rust" keeps its "like";
//! "It was, like, fast" loses it. When in doubt the word stays: a stray "like"
//! costs nothing, a missing "like" changes what was said.

use crate::text::{self, Chunk};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupOptions {
    /// Remove fillers (um/uh always; like/you know/kind of/sort of when set
    /// off by commas; a leading so/basically).
    pub remove_fillers: bool,
    /// Collapse stutters: "the the" → "the", "we should we should" → "we should".
    pub fix_doubles: bool,
}

impl Default for CleanupOptions {
    fn default() -> Self {
        CleanupOptions {
            remove_fillers: true,
            fix_doubles: true,
        }
    }
}

/// Sounds that are never words. Removed wherever they stand.
const HESITATIONS: &[&str] = &[
    "um", "umm", "ummm", "uh", "uhh", "uhhh", "uhm", "erm", "ehm", "mmm",
];

/// Phrases that are fillers only when the transcript sets them off with
/// commas (or starts a sentence with them and a comma).
const SET_OFF_FILLERS: &[&[&str]] = &[
    &["you", "know"],
    &["like"],
    &["kind", "of"],
    &["sort", "of"],
    &["basically"],
];

/// Words that open a dictation without meaning anything.
const LEADING_FILLERS: &[&str] = &["so", "basically"];

/// Doubled on purpose often enough that a repeat is not a stutter.
const DOUBLE_OK: &[&str] = &[
    "that", "had", "is", "do", "very", "really", "no", "yes", "yeah", "bye", "ha", "haha", "so",
    "now", "well", "go", "blah", "knock", "night", "there", "far", "more", "many", "again", "over",
    "round", "on", "by", "out", "up", "down", "too", "hey", "hi", "oh", "wait", "please", "come",
    "run", "one", "two", "three", "four", "five",
];

/// Disfluency removal on raw model output.
pub fn strip(raw: &str, opts: &CleanupOptions) -> String {
    let mut chunks = text::chunks(raw);
    if opts.remove_fillers {
        remove_hesitations(&mut chunks);
        // Leading fillers before and after: "So, basically, it works" needs
        // "So," gone before "basically," is at the start, and "Um, so, like,
        // it works" needs "like," gone before the second pass sees "so".
        remove_leading_fillers(&mut chunks);
        remove_set_off_fillers(&mut chunks);
        remove_leading_fillers(&mut chunks);
    }
    if opts.fix_doubles {
        collapse_doubles(&mut chunks);
    }
    let out = text::join(&chunks);
    if out.chars().any(char::is_alphanumeric) {
        out
    } else {
        String::new()
    }
}

/// Edge fixes, run last: single spaces, no space before punctuation, a
/// capital first letter, and end punctuation — except after an @mention or a
/// path, where a period would read as part of it.
pub fn finalize(text: &str) -> String {
    let mut chunks = text::chunks(text);
    // " ," and " ." left behind by earlier passes fold into the word before.
    let mut i = 1;
    while i < chunks.len() {
        if chunks[i].core.is_empty() && chunks[i].lead.is_empty() && !chunks[i].trail.is_empty() {
            let trail = std::mem::take(&mut chunks[i].trail);
            chunks[i - 1].trail.push_str(&trail);
            chunks.remove(i);
        } else {
            i += 1;
        }
    }
    if chunks.is_empty()
        || !chunks
            .iter()
            .any(|c| c.core.chars().any(char::is_alphanumeric))
    {
        return String::new();
    }

    if let Some(first) = chunks.iter_mut().find(|c| !c.core.is_empty()) {
        if !first.is_protected() && !text::has_inner_case(&first.core) {
            first.core = text::capitalize(&first.core);
        }
    }

    let last = chunks.last_mut().expect("non-empty");
    if !last.is_protected() && !last.ends_sentence() {
        let closers: String = last
            .trail
            .chars()
            .rev()
            .take_while(|c| matches!(c, '"' | '\'' | ')' | ']' | '”' | '’'))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let body = &last.trail[..last.trail.len() - closers.len()];
        let body = body.trim_end_matches([',', ';', ':', '-', '—']);
        last.trail = format!("{body}.{closers}");
    }
    text::join(&chunks)
}

/// Both halves with nothing in between: what a dictation gets outside a
/// terminal, minus the dictionary.
pub fn clean(raw: &str, opts: &CleanupOptions) -> String {
    finalize(&strip(raw, opts))
}

fn remove_hesitations(chunks: &mut Vec<Chunk>) {
    let mut i = 0;
    while i < chunks.len() {
        if !chunks[i].is_protected() && HESITATIONS.contains(&chunks[i].key().as_str()) {
            remove_span(chunks, i, i + 1);
        } else {
            i += 1;
        }
    }
}

fn remove_set_off_fillers(chunks: &mut Vec<Chunk>) {
    let mut i = 0;
    'outer: while i < chunks.len() {
        for phrase in SET_OFF_FILLERS {
            let end = i + phrase.len();
            if end > chunks.len() || !matches_phrase(&chunks[i..end], phrase) {
                continue;
            }
            let last = &chunks[end - 1];
            let at_sentence_start = i == 0 || chunks[i - 1].ends_sentence();
            let comma_before = i > 0 && chunks[i - 1].ends_with_comma();
            let comma_after = last.trail == ",";
            // ", you know." closes a sentence as a tag; nothing else does.
            let tag_end = *phrase == ["you", "know"] && comma_before && last.ends_sentence();
            if (comma_after && (comma_before || at_sentence_start)) || tag_end {
                remove_span(chunks, i, end);
                continue 'outer;
            }
        }
        i += 1;
    }
}

/// Inner words must be bare (no punctuation between "kind" and "of"); the
/// last word's trailing punctuation is judged by the caller.
fn matches_phrase(span: &[Chunk], phrase: &[&str]) -> bool {
    span.iter().zip(phrase).enumerate().all(|(k, (c, w))| {
        c.key() == *w && c.lead.is_empty() && (k + 1 == phrase.len() || c.trail.is_empty())
    })
}

fn remove_leading_fillers(chunks: &mut Vec<Chunk>) {
    loop {
        let Some(first) = chunks.first() else { return };
        if !first.lead.is_empty() || !LEADING_FILLERS.contains(&first.key().as_str()) {
            return;
        }
        // "So," always goes. A bare "So" goes only when enough follows that
        // it cannot be the point ("So what?" stays, "So I think we should" loses it).
        let words_after = chunks.len() - 1;
        let removable = first.trail == "," || (first.trail.is_empty() && words_after >= 3);
        if !removable {
            return;
        }
        remove_span(chunks, 0, 1);
    }
}

/// Remove `chunks[start..end]` and repair the punctuation around the hole:
/// a sentence end moves back onto the previous word, a comma that only
/// existed to set the removed words off goes with them, and a removed
/// capitalised sentence opener hands its capital to the next word.
fn remove_span(chunks: &mut Vec<Chunk>, start: usize, end: usize) {
    let removed: Vec<Chunk> = chunks.drain(start..end).collect();
    let first = &removed[0];
    let last = removed.last().expect("non-empty span");
    let terminal: String = last
        .trail
        .chars()
        .filter(|c| matches!(c, '.' | '?' | '!' | '…'))
        .collect();
    let had_comma = last.trail.starts_with(',');

    if start > 0 {
        let prev = &mut chunks[start - 1];
        if prev.ends_with_comma() && (had_comma || !terminal.is_empty()) {
            prev.trail.pop();
        }
        if !terminal.is_empty() && !prev.ends_sentence() {
            prev.trail.push_str(&terminal);
        }
    }

    let sentence_start = start == 0 || chunks[start - 1].ends_sentence();
    if sentence_start && start < chunks.len() {
        let next = &mut chunks[start];
        if !first.lead.is_empty() && next.lead.is_empty() {
            next.lead = first.lead.clone();
        }
        let opener_capital = first.core.chars().next().is_some_and(char::is_uppercase);
        if opener_capital && !next.is_protected() && !text::has_inner_case(&next.core) {
            next.core = text::capitalize(&next.core);
        }
    }
}

fn collapse_doubles(chunks: &mut Vec<Chunk>) {
    // Single words: "the the" → "the".
    let mut i = 0;
    while i + 1 < chunks.len() {
        let (a, b) = (&chunks[i], &chunks[i + 1]);
        if a.trail.is_empty()
            && b.lead.is_empty()
            && a.is_word()
            && a.key() == b.key()
            && !DOUBLE_OK.contains(&a.key().as_str())
        {
            let trail = chunks.remove(i + 1).trail;
            chunks[i].trail = trail;
        } else {
            i += 1;
        }
    }
    // Pairs: "we should we should" → "we should".
    let mut i = 0;
    while i + 3 < chunks.len() {
        let bare = |c: &Chunk| c.is_word() && c.lead.is_empty();
        let span = &chunks[i..i + 4];
        if span.iter().all(bare)
            && span[..3].iter().all(|c| c.trail.is_empty())
            && span[0].key() == span[2].key()
            && span[1].key() == span[3].key()
            && span[0].key() != span[1].key()
        {
            let trail = chunks[i + 3].trail.clone();
            chunks.drain(i + 2..i + 4);
            chunks[i + 1].trail = trail;
        } else {
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(raw: &str) -> String {
        clean(raw, &CleanupOptions::default())
    }

    #[test]
    fn hesitations_go() {
        assert_eq!(
            c("Um, I think we should ship it."),
            "I think we should ship it."
        );
        assert_eq!(
            c("I think, uh, we should ship it."),
            "I think we should ship it."
        );
        assert_eq!(c("We should uh ship it"), "We should ship it.");
        assert_eq!(c("That's it, um."), "That's it.");
        assert_eq!(c("Uh."), "");
        assert_eq!(c("um uh"), "");
        assert_eq!(c("Ummm, okay."), "Okay.");
    }

    #[test]
    fn hesitation_mid_sentence_keeps_sentence_boundary() {
        assert_eq!(
            c("First thing. Uh, second thing."),
            "First thing. Second thing."
        );
        assert_eq!(c("Done. um, next one"), "Done. next one.");
    }

    #[test]
    fn like_as_a_verb_stays() {
        assert_eq!(c("I like Rust."), "I like Rust.");
        assert_eq!(c("I like, really like Rust."), "I like, really like Rust.");
        assert_eq!(c("Do you like it?"), "Do you like it?");
        assert_eq!(c("What do you like, though?"), "What do you like, though?");
        assert_eq!(
            c("Things I like, like pizza."),
            "Things I like, like pizza."
        );
    }

    #[test]
    fn like_as_comparison_or_approximation_stays() {
        assert_eq!(c("It looks like a bug."), "It looks like a bug.");
        assert_eq!(
            c("It takes like five minutes."),
            "It takes like five minutes."
        );
        assert_eq!(c("Make it look like this"), "Make it look like this.");
        assert_eq!(c("I was like, what?"), "I was like, what?");
    }

    #[test]
    fn like_set_off_by_commas_goes() {
        assert_eq!(c("It was, like, really fast."), "It was really fast.");
        assert_eq!(c("Like, I don't know."), "I don't know.");
        assert_eq!(
            c("Do you know, like, anyone there?"),
            "Do you know anyone there?"
        );
    }

    #[test]
    fn like_like_stutter_collapses() {
        assert_eq!(
            c("It was like like really fast"),
            "It was like really fast."
        );
    }

    #[test]
    fn you_know() {
        assert_eq!(c("It's, you know, broken."), "It's broken.");
        assert_eq!(c("You know, it's broken."), "It's broken.");
        assert_eq!(c("It's broken, you know."), "It's broken.");
        assert_eq!(c("Do you know the answer?"), "Do you know the answer?");
        assert_eq!(c("You know the answer."), "You know the answer.");
        assert_eq!(c("I want you know to see"), "I want you know to see.");
    }

    #[test]
    fn kind_of_and_sort_of() {
        assert_eq!(c("It's, kind of, slow."), "It's slow.");
        assert_eq!(c("It's kind of slow."), "It's kind of slow.");
        assert_eq!(c("What kind of error is it?"), "What kind of error is it?");
        assert_eq!(c("Sort of, yes."), "Yes.");
        assert_eq!(c("A sort of queue."), "A sort of queue.");
    }

    #[test]
    fn leading_so_and_basically() {
        assert_eq!(
            c("So I think we should refactor this."),
            "I think we should refactor this."
        );
        assert_eq!(c("So, what now?"), "What now?");
        assert_eq!(c("So what?"), "So what?");
        assert_eq!(c("Basically, the cache is stale."), "The cache is stale.");
        assert_eq!(c("Basically the cache is stale."), "The cache is stale.");
        assert_eq!(c("Um, so, basically, it works."), "It works.");
        // Only at the start.
        assert_eq!(c("I did so much."), "I did so much.");
        assert_eq!(c("It is basically done."), "It is basically done.");
    }

    #[test]
    fn basically_set_off_mid_sentence() {
        assert_eq!(c("The cache is, basically, stale."), "The cache is stale.");
    }

    #[test]
    fn doubled_words() {
        assert_eq!(c("The the cache is stale."), "The cache is stale.");
        assert_eq!(c("I I think so."), "I think so.");
        assert_eq!(c("fix the the the bug"), "Fix the bug.");
        assert_eq!(c("We should we should ship."), "We should ship.");
    }

    #[test]
    fn meaningful_doubles_stay() {
        assert_eq!(
            c("I think that that is fine."),
            "I think that that is fine."
        );
        assert_eq!(c("He had had enough."), "He had had enough.");
        assert_eq!(c("It is very very slow."), "It is very very slow.");
        assert_eq!(c("No no no."), "No no no.");
        // Punctuation between them means it was said twice on purpose.
        assert_eq!(c("Stop. Stop."), "Stop. Stop.");
    }

    #[test]
    fn capitalize_and_punctuate() {
        assert_eq!(c("hello world"), "Hello world.");
        assert_eq!(c("is it done?"), "Is it done?");
        assert_eq!(c("wow!"), "Wow!");
        assert_eq!(c("trailing comma,"), "Trailing comma.");
        assert_eq!(c("he said \"hi\""), "He said \"hi.\"");
        assert_eq!(c("  lots   of    space  "), "Lots of space.");
    }

    #[test]
    fn inner_case_first_words_keep_their_case() {
        assert_eq!(c("iPhone is great"), "iPhone is great.");
        assert_eq!(c("macOS update"), "macOS update.");
    }

    #[test]
    fn no_period_after_a_mention_or_path() {
        assert_eq!(finalize("look at @src/audio.rs"), "Look at @src/audio.rs");
        assert_eq!(finalize("open src/main.rs"), "Open src/main.rs");
        assert_eq!(finalize("@CLAUDE.md is stale"), "@CLAUDE.md is stale.");
    }

    #[test]
    fn fillers_never_touch_protected_chunks() {
        assert_eq!(c("check @like/um.rs, like, now"), "Check @like/um.rs now.");
    }

    #[test]
    fn options_turn_rules_off() {
        let off = CleanupOptions {
            remove_fillers: false,
            fix_doubles: false,
        };
        assert_eq!(clean("um the the thing", &off), "Um the the thing.");
    }

    #[test]
    fn empty_and_punctuation_only() {
        assert_eq!(c(""), "");
        assert_eq!(c("   "), "");
        assert_eq!(c("..."), "");
        assert_eq!(c(", um."), "");
    }

    #[test]
    fn numbers_and_code_words_survive() {
        assert_eq!(c("set the timeout to 250 ms"), "Set the timeout to 250 ms.");
        assert_eq!(c("run cargo test"), "Run cargo test.");
    }
}
