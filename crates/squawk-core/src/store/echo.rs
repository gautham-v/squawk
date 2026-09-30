//! The transcript-level safety net for echo: a "You" segment that mostly
//! repeats what "Them" said at the same moment is the other side leaking
//! into the mic (a call on speakers), not you.
//!
//! The mic's echo cancellation removes nearly all of it; this catches what
//! gets past (a loud laptop, the canceller still adapting in the first
//! seconds, or a mic that fell back to plain capture). It is tuned to keep
//! real speech: short answers ("Yes.", "Right, exactly.") are never
//! dropped, and a reply that reuses some of the question's words is kept.
//!
//! The test, per "You" segment:
//! 1. It has at least [`ECHO_MIN_WORDS`] words.
//! 2. Pool the words of every "Them" segment that overlaps it, each widened
//!    by [`ECHO_WINDOW_SECS`] on both sides, in time order.
//! 3. At least [`ECHO_MIN_SHARE`] of its words appear in that pool in the
//!    same order (longest common subsequence). Echo is their sentence again,
//!    give or take a misheard or dropped word.
//! 4. And at least [`ECHO_MIN_PHRASE_SHARE`] of its words sit in a word
//!    pair that also occurs side by side in the pool. Echo keeps whole
//!    phrases; this stops a long pool from matching scattered "the", "is",
//!    "we" in a reply made while they talk.
//!
//! Words are compared lowercased, without punctuation or apostrophes.

use std::collections::HashSet;

use super::meeting::{Segment, Speaker};

/// How far apart in time a "You" and a "Them" segment may be and still
/// count as the same moment.
pub const ECHO_WINDOW_SECS: f64 = 3.0;
/// "You" segments shorter than this are always kept: "Yes.", "Right.",
/// "Sounds good." are answers far more often than echo.
pub const ECHO_MIN_WORDS: usize = 4;
/// Share of a "You" segment's words that must appear, in order, in what
/// "Them" said around it.
pub const ECHO_MIN_SHARE: f64 = 0.75;
/// Share of its words that must be part of a word pair "Them" also said.
pub const ECHO_MIN_PHRASE_SHARE: f64 = 0.5;

/// `segments` without the "You" segments that repeat "Them". Order is kept;
/// "Them" segments are never touched.
pub fn drop_echoes(segments: Vec<Segment>) -> Vec<Segment> {
    let them: Vec<Heard> = segments
        .iter()
        .filter(|s| s.speaker == Speaker::Them)
        .map(|s| Heard {
            start: s.start_secs,
            end: s.end_secs.max(s.start_secs),
            words: words(&s.text),
        })
        .collect();
    if them.is_empty() {
        return segments;
    }
    let mut them = them;
    them.sort_by(|a, b| a.start.total_cmp(&b.start));
    segments
        .into_iter()
        .filter(|s| s.speaker != Speaker::You || !is_echo(s, &them))
        .collect()
}

struct Heard {
    start: f64,
    end: f64,
    words: Vec<String>,
}

fn is_echo(you: &Segment, them: &[Heard]) -> bool {
    let said = words(&you.text);
    if said.len() < ECHO_MIN_WORDS {
        return false;
    }
    let from = you.start_secs - ECHO_WINDOW_SECS;
    let to = you.end_secs.max(you.start_secs) + ECHO_WINDOW_SECS;
    let heard: Vec<&str> = them
        .iter()
        .filter(|t| t.start <= to && t.end >= from)
        .flat_map(|t| t.words.iter().map(String::as_str))
        .collect();
    let n = said.len() as f64;
    common_in_order(&said, &heard) as f64 >= ECHO_MIN_SHARE * n
        && phrase_share(&said, &heard) >= ECHO_MIN_PHRASE_SHARE
}

/// Length of the longest common subsequence of the two word lists.
fn common_in_order(said: &[String], heard: &[&str]) -> usize {
    let mut row = vec![0usize; heard.len() + 1];
    for w in said {
        let mut diagonal = 0;
        for (j, h) in heard.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if w == h {
                diagonal + 1
            } else {
                above.max(row[j])
            };
            diagonal = above;
        }
    }
    row[heard.len()]
}

/// Share of `said` covered by a word pair that also appears in `heard`.
fn phrase_share(said: &[String], heard: &[&str]) -> f64 {
    if said.is_empty() {
        return 0.0;
    }
    let pairs: HashSet<(&str, &str)> = heard.windows(2).map(|w| (w[0], w[1])).collect();
    let mut echoed = vec![false; said.len()];
    for (i, w) in said.windows(2).enumerate() {
        if pairs.contains(&(w[0].as_str(), w[1].as_str())) {
            echoed[i] = true;
            echoed[i + 1] = true;
        }
    }
    echoed.iter().filter(|&&e| e).count() as f64 / said.len() as f64
}

/// Lowercased words, punctuation gone ("I'll" → "ill", "follow-up" →
/// "follow", "up").
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '’'))
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(speaker: Speaker, start: f64, end: f64, text: &str) -> Segment {
        Segment {
            speaker,
            start_secs: start,
            end_secs: end,
            text: text.into(),
        }
    }

    fn you(start: f64, end: f64, text: &str) -> Segment {
        seg(Speaker::You, start, end, text)
    }

    fn them(start: f64, end: f64, text: &str) -> Segment {
        seg(Speaker::Them, start, end, text)
    }

    /// The "You" texts that survive.
    fn kept(segments: Vec<Segment>) -> Vec<String> {
        drop_echoes(segments)
            .into_iter()
            .filter(|s| s.speaker == Speaker::You)
            .map(|s| s.text)
            .collect()
    }

    #[test]
    fn words_are_normalised() {
        assert_eq!(
            words("I'll send the Q3 follow-up — OK?"),
            vec!["ill", "send", "the", "q3", "follow", "up", "ok"]
        );
        assert!(words(" ... ").is_empty());
    }

    fn owned(ws: &[&str]) -> Vec<String> {
        ws.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn common_in_order_is_the_lcs() {
        let said = owned(&["a", "b", "x", "c", "d"]);
        assert_eq!(common_in_order(&said, &["a", "c", "b", "c", "d"]), 4);
        assert_eq!(common_in_order(&said, &[]), 0);
        assert_eq!(common_in_order(&[], &["a"]), 0);
    }

    #[test]
    fn phrase_share_counts_words_in_shared_pairs() {
        let said = owned(&["we", "should", "go", "now"]);
        assert_eq!(phrase_share(&said, &["we", "should", "stay"]), 0.5);
        assert_eq!(phrase_share(&said, &["go", "we", "now"]), 0.0);
    }

    #[test]
    fn a_verbatim_echo_goes() {
        let segs = vec![
            them(
                12.0,
                17.5,
                "The quarterly numbers look strong, but we should revisit the hiring plan before Friday.",
            ),
            you(
                12.2,
                17.6,
                "The quarterly numbers look strong, but we should revisit the hiring plan before Friday.",
            ),
        ];
        assert!(kept(segs).is_empty());
    }

    #[test]
    fn a_misheard_echo_goes() {
        // Quieter and muffled through the speakers: words dropped and misheard.
        let segs = vec![
            them(
                40.0,
                46.0,
                "The quarterly numbers look strong, but we should revisit the hiring plan before Friday.",
            ),
            you(
                40.3,
                45.8,
                "quarterly numbers look strong we should revisit the higher plan for Friday",
            ),
        ];
        assert!(kept(segs).is_empty());
    }

    #[test]
    fn an_echo_spanning_two_of_their_segments_goes() {
        // Their track cut between two sentences; the mic's chunk did not.
        let segs =
            vec![
            them(100.0, 102.5, "Can everyone see my screen?"),
            them(103.0, 106.0, "This is the dashboard for the new onboarding flow."),
            you(
                100.4,
                106.2,
                "Can everyone see my screen? This is the dashboard for the new onboarding flow.",
            ),
        ];
        assert!(kept(segs).is_empty());
    }

    #[test]
    fn a_leftover_fragment_goes() {
        let segs = vec![
            them(
                7.0,
                12.0,
                "Let's push the launch to next Tuesday so design has time to finish.",
            ),
            you(9.5, 11.0, "so design has time"),
        ];
        assert!(kept(segs).is_empty());
    }

    #[test]
    fn short_answers_stay() {
        let segs = vec![
            them(20.0, 21.5, "Does that work for you?"),
            you(21.8, 22.2, "Yes."),
            them(23.0, 25.0, "Right, so the plan is set. Right?"),
            you(25.1, 25.6, "Right, exactly."),
            them(26.0, 27.0, "Sounds good."),
            you(26.5, 27.2, "Sounds good."),
        ];
        assert_eq!(kept(segs), vec!["Yes.", "Right, exactly.", "Sounds good."]);
    }

    #[test]
    fn a_reply_that_reuses_their_words_stays() {
        let segs = vec![
            them(50.0, 52.0, "Can you send the report by Friday?"),
            you(52.4, 55.0, "Sure, I'll send the report by Friday morning."),
            them(60.0, 62.0, "The code is four seven two nine."),
            you(62.3, 64.0, "Four seven two nine, got it."),
        ];
        assert_eq!(
            kept(segs),
            vec![
                "Sure, I'll send the report by Friday morning.",
                "Four seven two nine, got it."
            ]
        );
    }

    #[test]
    fn talking_over_them_stays() {
        // Both talking at once about the same thing: shared words, few
        // shared phrases.
        let segs = vec![
            them(
                30.0,
                38.0,
                "I think we should move the launch to next week because the migration is not done and support is not ready.",
            ),
            you(
                31.0,
                36.0,
                "No, I think the launch should stay, the migration is basically done.",
            ),
        ];
        assert_eq!(kept(segs).len(), 1);
    }

    #[test]
    fn half_echo_half_you_stays() {
        let segs = vec![
            them(70.0, 73.0, "We should revisit the hiring plan."),
            you(
                70.2,
                76.0,
                "We should revisit the hiring plan. Honestly I disagree, we are already behind on two roles.",
            ),
        ];
        assert_eq!(kept(segs).len(), 1);
    }

    #[test]
    fn repeating_them_later_stays() {
        // Quoting what they said a minute ago is not echo.
        let segs = vec![
            them(
                10.0,
                14.0,
                "We should revisit the hiring plan before Friday.",
            ),
            you(
                70.0,
                74.0,
                "Like you said, we should revisit the hiring plan before Friday.",
            ),
            you(
                80.0,
                83.0,
                "We should revisit the hiring plan before Friday.",
            ),
        ];
        assert_eq!(kept(segs).len(), 2);
    }

    #[test]
    fn the_window_is_three_seconds_either_side() {
        let text = "Let's take the rest of this offline.";
        let inside = vec![them(10.0, 12.0, text), you(14.9, 16.0, text)];
        assert!(kept(inside).is_empty());
        let outside = vec![them(10.0, 12.0, text), you(15.1, 16.0, text)];
        assert_eq!(kept(outside).len(), 1);
        // A segment without an end (a chunk with no timestamps) is a point.
        let point = vec![them(10.0, 10.0, text), you(12.5, 12.5, text)];
        assert!(kept(point).is_empty());
    }

    #[test]
    fn only_you_is_filtered_and_order_is_kept() {
        let text = "Can you hear me now, is this better?";
        let segs = vec![
            you(0.0, 2.0, "Morning everyone, give me one second."),
            them(3.0, 5.0, text),
            you(3.1, 5.1, text),
            you(6.0, 8.0, "Yes, much better, thanks for fixing it."),
        ];
        let out = drop_echoes(segs);
        let texts: Vec<(Speaker, &str)> =
            out.iter().map(|s| (s.speaker, s.text.as_str())).collect();
        assert_eq!(
            texts,
            vec![
                (Speaker::You, "Morning everyone, give me one second."),
                (Speaker::Them, text),
                (Speaker::You, "Yes, much better, thanks for fixing it."),
            ]
        );
    }

    #[test]
    fn nothing_from_them_means_nothing_dropped() {
        let segs = vec![you(0.0, 3.0, "Just me talking to myself here today.")];
        assert_eq!(drop_echoes(segs.clone()), segs);
    }
}
