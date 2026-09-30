//! Word-level plumbing shared by cleanup, the dictionary and context.
//!
//! Text is handled as whitespace-separated chunks, each split into leading
//! punctuation, a core, and trailing punctuation — `"(um,"` is `(` + `um` +
//! `,`. That is enough to reason about "a filler set off by commas" without a
//! tokenizer, and joining the chunks back with single spaces is the only
//! normalisation it imposes.
//!
//! Some chunks are *protected*: an `@mention`, a path, a URL, inline code, a
//! file name. Nothing in squawk rewrites inside one — a dictionary entry for
//! "audio" must not reach into `@src/audio.rs`.

/// One whitespace-separated piece of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub lead: String,
    pub core: String,
    pub trail: String,
}

impl Chunk {
    pub fn parse(raw: &str) -> Chunk {
        if is_protected(raw) {
            // Opaque: a trailing comma or period still counts as punctuation
            // (so sentence logic works), but nothing else is split off.
            let body = raw.trim_end_matches([',', '.', '?', '!', ';', ':']);
            if body.is_empty() || !is_protected(body) {
                return Chunk {
                    lead: String::new(),
                    core: raw.to_string(),
                    trail: String::new(),
                };
            }
            return Chunk {
                lead: String::new(),
                core: body.to_string(),
                trail: raw[body.len()..].to_string(),
            };
        }
        let start = raw.find(is_core_char).unwrap_or(raw.len());
        let end = raw
            .char_indices()
            .rev()
            .find(|(_, c)| is_core_char(*c))
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(start);
        Chunk {
            lead: raw[..start].to_string(),
            core: raw[start..end.max(start)].to_string(),
            trail: raw[end.max(start)..].to_string(),
        }
    }

    pub fn render(&self) -> String {
        format!("{}{}{}", self.lead, self.core, self.trail)
    }

    /// The core, lowercased, for matching.
    pub fn key(&self) -> String {
        self.core.to_lowercase()
    }

    pub fn is_protected(&self) -> bool {
        is_protected(&self.core)
    }

    /// Trailing punctuation that ends a sentence.
    pub fn ends_sentence(&self) -> bool {
        self.trail
            .trim_end_matches(['"', '\'', ')', ']', '”', '’'])
            .ends_with(['.', '?', '!', '…'])
    }

    pub fn ends_with_comma(&self) -> bool {
        self.trail.ends_with(',')
    }

    /// Only letters (and inner apostrophes/hyphens): a plain word.
    pub fn is_word(&self) -> bool {
        !self.core.is_empty()
            && self.core.chars().any(char::is_alphabetic)
            && self
                .core
                .chars()
                .all(|c| c.is_alphabetic() || c == '\'' || c == '’' || c == '-')
    }
}

fn is_core_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// Split into chunks on any whitespace.
pub fn chunks(text: &str) -> Vec<Chunk> {
    text.split_whitespace().map(Chunk::parse).collect()
}

/// Join chunks with single spaces, dropping any that ended up empty.
pub fn join(chunks: &[Chunk]) -> String {
    let mut out = String::new();
    for c in chunks {
        let s = c.render();
        if s.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&s);
    }
    out
}

/// A chunk nothing may rewrite inside: `@mention`, path, URL, `code`, a file
/// name with an extension, `a::b`.
pub fn is_protected(word: &str) -> bool {
    if word.starts_with('@') || word.contains('`') || word.contains("://") || word.contains("::") {
        return true;
    }
    if word.contains('/') {
        let segments = word
            .split('/')
            .filter(|s| s.chars().any(char::is_alphanumeric))
            .count();
        let rooted = word.starts_with('/') || word.starts_with("~/");
        if segments >= 2 || (rooted && segments == 1) {
            return true;
        }
    }
    looks_like_file_name(word.trim_end_matches(['.', ',', '?', '!', ';', ':']))
}

/// `main.rs`, `Cargo.toml`, `.gitignore`: a dot with a letter on the right
/// and something on the left, or a leading dot. `e.g` and `3.5` are not.
fn looks_like_file_name(word: &str) -> bool {
    if let Some(rest) = word.strip_prefix('.') {
        return rest.len() > 1
            && rest
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
    }
    let Some((stem, ext)) = word.rsplit_once('.') else {
        return false;
    };
    stem.len() > 1
        && (1..=6).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && ext.chars().any(|c| c.is_ascii_alphabetic())
        && stem
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Run `f` over the stretches of text between protected chunks, leaving the
/// protected ones exactly as they were. Whitespace is normalised to single
/// spaces.
pub fn map_prose(text: &str, mut f: impl FnMut(&str) -> String) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut run: Vec<&str> = Vec::new();
    let flush = |run: &mut Vec<&str>, out: &mut Vec<String>, f: &mut dyn FnMut(&str) -> String| {
        if !run.is_empty() {
            let mapped = f(&run.join(" "));
            if !mapped.is_empty() {
                out.push(mapped);
            }
            run.clear();
        }
    };
    for word in text.split_whitespace() {
        if is_protected(word) {
            flush(&mut run, &mut out, &mut f);
            out.push(word.to_string());
        } else {
            run.push(word);
        }
    }
    flush(&mut run, &mut out, &mut f);
    out.join(" ")
}

/// Uppercase the first character of `s`.
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A word whose casing carries meaning beyond the first letter: `iPhone`,
/// `macOS`, `GPT`. Capitalising or lowercasing it would be wrong.
pub fn has_inner_case(word: &str) -> bool {
    word.chars().skip(1).any(char::is_uppercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_splits_punctuation() {
        let c = Chunk::parse("(um,");
        assert_eq!(
            (c.lead.as_str(), c.core.as_str(), c.trail.as_str()),
            ("(", "um", ",")
        );
        let c = Chunk::parse("don't.");
        assert_eq!(c.core, "don't");
        assert_eq!(c.trail, ".");
        let c = Chunk::parse("...");
        assert_eq!(c.core, "");
        assert_eq!(c.render(), "...");
    }

    #[test]
    fn protected_chunks_stay_whole() {
        let c = Chunk::parse("@src/audio.rs,");
        assert_eq!(c.core, "@src/audio.rs");
        assert_eq!(c.trail, ",");
        assert!(c.is_protected());
        let c = Chunk::parse("Cargo.toml.");
        assert_eq!(c.core, "Cargo.toml");
        assert!(c.ends_sentence());
    }

    #[test]
    fn protection_rules() {
        for w in [
            "@CLAUDE.md",
            "src/main.rs",
            "https://x.io",
            "`ls`",
            "std::fs",
            "main.rs",
            ".gitignore",
            "a/b",
        ] {
            assert!(is_protected(w), "{w}");
        }
        for w in ["hello", "e.g.", "3.5", "U.S.", "and/", "Mr.", "/"] {
            assert!(!is_protected(w), "{w}");
        }
    }

    #[test]
    fn map_prose_skips_protected() {
        let out = map_prose("look at  @src/audio.rs and audio", |s| s.to_uppercase());
        assert_eq!(out, "LOOK AT @src/audio.rs AND AUDIO");
    }

    #[test]
    fn sentence_end_sees_through_quotes() {
        assert!(Chunk::parse("done.\"").ends_sentence());
        assert!(Chunk::parse("why?)").ends_sentence());
        assert!(!Chunk::parse("so,").ends_sentence());
    }

    #[test]
    fn inner_case() {
        assert!(has_inner_case("iPhone"));
        assert!(has_inner_case("GPT"));
        assert!(!has_inner_case("Hello"));
    }
}
