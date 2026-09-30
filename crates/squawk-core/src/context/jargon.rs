//! Repo jargon, spelled the way the repo spells it.
//!
//! Two fixes, both whole-word and case-insensitive:
//! - casing: "gpui" → `GPUI`, "tidewell" → `Tidewell` (only towards a
//!   spelling with capitals; never lowercases what the model capitalised);
//! - joins: up to three tokens the model split, when their join is a known
//!   word: "tide well" → `Tidewell`, "claude bar" → `claudebar`,
//!   "squawk core" → `squawk-core`.
//!
//! Protected chunks (@mentions, paths, URLs, code) are never touched, and
//! the stoplist keeps ordinary words ordinary (see index.rs).

use super::index::{squash, Index};
use crate::text::Chunk;

/// Most tokens one word can be split into ("chat g p t" is not worth it).
const MAX_JOIN: usize = 3;

pub(crate) fn apply(chunks: Vec<Chunk>, index: &Index) -> Vec<Chunk> {
    if !index.has_jargon() {
        return chunks;
    }
    let n = chunks.len();
    let mut out: Vec<Chunk> = Vec::with_capacity(n);
    let mut i = 0;
    'outer: while i < n {
        if !is_plain(&chunks[i]) {
            out.push(chunks[i].clone());
            i += 1;
            continue;
        }
        for len in (2..=MAX_JOIN.min(n - i)).rev() {
            let span = &chunks[i..i + len];
            let joinable = span.iter().all(is_plain)
                && span[..len - 1].iter().all(|c| c.trail.is_empty())
                && span[1..].iter().all(|c| c.lead.is_empty());
            if !joinable {
                continue;
            }
            let key: String = span.iter().map(|c| squash(&c.core)).collect();
            if let Some(word) = index.joined(&key) {
                out.push(Chunk {
                    lead: span[0].lead.clone(),
                    core: word.to_string(),
                    trail: span[len - 1].trail.clone(),
                });
                i += len;
                continue 'outer;
            }
        }
        let mut chunk = chunks[i].clone();
        if let Some(word) = index.exact(&chunk.core.to_lowercase()) {
            chunk.core = word.to_string();
        } else if chunk.core.contains('-') {
            // "tide-well": a split the model hyphenated.
            if let Some(word) = index.joined(&squash(&chunk.core)) {
                chunk.core = word.to_string();
            }
        }
        out.push(chunk);
        i += 1;
    }
    out
}

/// Letters, digits and inner hyphens; nothing protected.
fn is_plain(c: &Chunk) -> bool {
    !c.core.is_empty()
        && !c.is_protected()
        && c.core.chars().all(|ch| ch.is_alphanumeric() || ch == '-')
}

#[cfg(test)]
mod tests {
    use super::super::apply_vocab;
    use super::super::testutil::vocab;

    const WORDS: &[&str] = &[
        "Tidewell",
        "claudebar",
        "squawk-core",
        "GPUI",
        "Tokio",
        "ModelStatus",
        "gpt5",
        "keep_audio",
        "Settings",
        "ChatGPT",
    ];

    fn run(input: &str) -> String {
        apply_vocab(input, &vocab(&[], WORDS))
    }

    #[test]
    fn fixes_casing_and_splits() {
        let cases = [
            ("tide well is the agency", "Tidewell is the agency"),
            ("the Tide Well site", "the Tidewell site"),
            ("open claude bar", "open claudebar"),
            ("in squawk core, the text", "in squawk-core, the text"),
            ("the gpui popover", "the GPUI popover"),
            ("spawn a tokio task.", "spawn a Tokio task."),
            ("check model status", "check ModelStatus"),
            ("the tide-well repo", "the Tidewell repo"),
            ("ask gpt 5", "ask gpt5"),
            ("ask chat gpt", "ask ChatGPT"),
            ("(gpui)", "(GPUI)"),
        ];
        for (input, want) in cases {
            assert_eq!(run(input), want, "{input}");
        }
    }

    #[test]
    fn leaves_the_rest_alone() {
        let cases = [
            // Stopwords, snake_case targets, punctuation between the parts.
            "open settings",
            "keep audio off",
            "front, ward",
            // Protected chunks.
            "@src/gpui.rs and `tokio`",
            "see https://gpui.rs",
            // Already right; never lowercased.
            "Tidewell and GPUI",
            "Claudebar is running",
        ];
        for input in cases {
            assert_eq!(run(input), input, "{input}");
        }
    }

    #[test]
    fn no_words_no_work() {
        assert_eq!(apply_vocab("tide well", &vocab(&[], &[])), "tide well");
    }
}
