//! `~/squawk/dictionary.txt`: the user's words.
//!
//! One entry per line:
//! - `Kubernetes` — a term. Whenever it is heard (any case) it is written
//!   exactly like this. A term with inner capitals or digits (`ChatGPT`,
//!   `GitHub`, `gpt5`) also matches when the model splits it: "chat GPT",
//!   "git hub".
//! - `cloud code -> Claude Code` — a replacement: the spoken words on the
//!   left, what to write on the right.
//! - `# ...` and blank lines are ignored.
//!
//! Matching is whole-word and case-insensitive, longest entry first, and
//! never reaches inside an @mention or a path.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use regex::{NoExpand, Regex, RegexBuilder};

use crate::error::Result;
use crate::text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Term(String),
    Replace { from: String, to: String },
}

impl Entry {
    /// Parse one line. `None` for blanks, comments and lines with an empty side.
    pub fn parse(line: &str) -> Option<Entry> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        if let Some((from, to)) = line.split_once("->") {
            let from = collapse_ws(from);
            let to = collapse_ws(to);
            if from.is_empty() || to.is_empty() {
                return None;
            }
            return Some(Entry::Replace { from, to });
        }
        Some(Entry::Term(collapse_ws(line)))
    }

    /// The line as written back to the file.
    pub fn to_line(&self) -> String {
        match self {
            Entry::Term(t) => t.clone(),
            Entry::Replace { from, to } => format!("{from} -> {to}"),
        }
    }

    /// What is heard (the key two entries clash on).
    pub fn spoken(&self) -> &str {
        match self {
            Entry::Term(t) => t,
            Entry::Replace { from, .. } => from,
        }
    }

    /// What is written.
    pub fn written(&self) -> &str {
        match self {
            Entry::Term(t) => t,
            Entry::Replace { to, .. } => to,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Dictionary {
    entries: Vec<Entry>,
    /// Longest spoken form first, so "claude code" wins over "claude".
    rules: Vec<(Regex, String)>,
}

impl Dictionary {
    pub fn parse(text: &str) -> Dictionary {
        let entries: Vec<Entry> = text.lines().filter_map(Entry::parse).collect();
        let mut sorted: Vec<&Entry> = entries.iter().collect();
        sorted.sort_by_key(|e| std::cmp::Reverse(e.spoken().len()));
        let rules = sorted
            .into_iter()
            .filter_map(|e| Some((pattern_for(e)?, e.written().to_string())))
            .collect();
        Dictionary { entries, rules }
    }

    /// A missing file is an empty dictionary.
    pub fn load(path: &Path) -> Result<Dictionary> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Dictionary::parse(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Dictionary::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The written forms of all terms (not replacements): extra vocabulary
    /// for Claude Code mode.
    pub fn terms(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().filter_map(|e| match e {
            Entry::Term(t) => Some(t.as_str()),
            Entry::Replace { .. } => None,
        })
    }

    /// Apply every entry to `text`, outside protected chunks.
    pub fn apply(&self, text: &str) -> String {
        if self.rules.is_empty() {
            return text.to_string();
        }
        text::map_prose(text, |prose| {
            let mut out = prose.to_string();
            for (re, written) in &self.rules {
                if re.is_match(&out) {
                    out = re.replace_all(&out, NoExpand(written)).into_owned();
                }
            }
            out
        })
    }
}

/// Build the regex for one entry, or `None` if it has no letters to match.
fn pattern_for(entry: &Entry) -> Option<Regex> {
    let spoken = entry.spoken();
    let words: Vec<String> = spoken
        .split_whitespace()
        .map(|w| match entry {
            Entry::Term(_) if is_jargon(w) => split_jargon(w)
                .iter()
                .map(|p| regex::escape(p))
                .collect::<Vec<_>>()
                .join(r"[\s\-]?"),
            _ => regex::escape(w),
        })
        .collect();
    if words.is_empty() {
        return None;
    }
    let mut pattern = words.join(r"[\s\-]+");
    if spoken.chars().next().is_some_and(is_word_char) {
        pattern = format!(r"\b{pattern}");
    }
    if spoken.chars().last().is_some_and(is_word_char) {
        pattern = format!(r"{pattern}\b");
    }
    RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
        .ok()
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Inner capitals or a letter/digit mix: a word the model may split.
fn is_jargon(word: &str) -> bool {
    let has_digit = word.chars().any(|c| c.is_ascii_digit());
    let has_alpha = word.chars().any(char::is_alphabetic);
    (text::has_inner_case(word) && word.chars().any(char::is_lowercase)) || (has_digit && has_alpha)
}

/// `ChatGPT` → `Chat`, `GPT`; `GitHub` → `Git`, `Hub`; `gpt5` → `gpt`, `5`;
/// `SQLite` → `SQ`, `Lite` (an uppercase run hands its last letter to a
/// following lowercase run, the usual camel-case rule).
fn split_jargon(word: &str) -> Vec<String> {
    #[derive(PartialEq, Clone, Copy)]
    enum K {
        Upper,
        Lower,
        Digit,
        Other,
    }
    let kind = |c: char| {
        if c.is_uppercase() {
            K::Upper
        } else if c.is_lowercase() {
            K::Lower
        } else if c.is_ascii_digit() {
            K::Digit
        } else {
            K::Other
        }
    };
    let chars: Vec<char> = word.chars().collect();
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let k = kind(c);
        if let Some(&p) = i.checked_sub(1).and_then(|j| chars.get(j)) {
            let pk = kind(p);
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let boundary = match (pk, k) {
                (K::Lower, K::Upper) => true,
                (K::Upper, K::Upper) => next_lower,
                (a, b) if (a == K::Digit) != (b == K::Digit) => true,
                _ => false,
            };
            if boundary && !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Header written when `add` creates the file.
pub const DICTIONARY_HEADER: &str = "# squawk dictionary: one entry per line.\n\
# A term is written exactly as it appears here:     Kubernetes\n\
# A replacement maps what you say to what is typed: cloud code -> Claude Code\n";

/// Append an entry (`phrase` or `spoken -> written`) unless an entry with
/// the same spoken form is already there. Creates the file with a short
/// header. Returns whether a line was added.
pub fn add(path: &Path, line: &str) -> Result<bool> {
    let Some(entry) = Entry::parse(line) else {
        return Ok(false);
    };
    let existing = Dictionary::load(path)?;
    let spoken = entry.spoken().to_lowercase();
    if existing
        .entries()
        .iter()
        .any(|e| e.spoken().to_lowercase() == spoken)
    {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut body = std::fs::read_to_string(path).unwrap_or_else(|_| DICTIONARY_HEADER.to_string());
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&entry.to_line());
    body.push('\n');
    std::fs::write(path, body)?;
    Ok(true)
}

/// A dictionary that re-reads its file when the file's mtime changes, so an
/// edit (by hand, by `squawk dict add`, by Claude) applies to the next
/// dictation without a restart. One `stat` per call.
#[derive(Debug)]
pub struct DictionaryCache {
    path: PathBuf,
    mtime: Option<SystemTime>,
    dict: Dictionary,
}

impl DictionaryCache {
    pub fn new(path: PathBuf) -> DictionaryCache {
        DictionaryCache {
            path,
            mtime: None,
            dict: Dictionary::default(),
        }
    }

    pub fn get(&mut self) -> &Dictionary {
        let mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        if mtime != self.mtime {
            self.dict = Dictionary::load(&self.path).unwrap_or_default();
            self.mtime = mtime;
        }
        &self.dict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(lines: &str) -> Dictionary {
        Dictionary::parse(lines)
    }

    #[test]
    fn parses_lines() {
        let dict =
            d("# comment\n\nKubernetes\n  cloud   code ->  Claude Code \n -> nothing\nbad ->\n");
        assert_eq!(
            dict.entries(),
            &[
                Entry::Term("Kubernetes".into()),
                Entry::Replace {
                    from: "cloud code".into(),
                    to: "Claude Code".into()
                }
            ]
        );
    }

    #[test]
    fn term_fixes_case() {
        let dict = d("Kubernetes\nkubectl\n");
        assert_eq!(
            dict.apply("deploy to kubernetes now"),
            "deploy to Kubernetes now"
        );
        assert_eq!(dict.apply("Kubectl get pods."), "kubectl get pods.");
    }

    #[test]
    fn whole_words_only() {
        let dict = d("Rust\n");
        assert_eq!(dict.apply("trust the rust code"), "trust the Rust code");
    }

    #[test]
    fn replacement_spans_words_and_punctuation_case() {
        let dict = d("cloud code -> Claude Code\n");
        assert_eq!(
            dict.apply("ask Cloud Code to fix it"),
            "ask Claude Code to fix it"
        );
        assert_eq!(dict.apply("ask cloud-code."), "ask Claude Code.");
        assert_eq!(dict.apply("the cloud is code"), "the cloud is code");
    }

    #[test]
    fn jargon_matches_when_split() {
        let dict = d("ChatGPT\nGitHub\ngpt5\n");
        assert_eq!(dict.apply("ask chat GPT"), "ask ChatGPT");
        assert_eq!(dict.apply("push to git hub"), "push to GitHub");
        assert_eq!(dict.apply("push to github"), "push to GitHub");
        assert_eq!(dict.apply("use GPT 5 here"), "use gpt5 here");
    }

    #[test]
    fn plain_words_do_not_match_when_split() {
        // "together" is not jargon, so "to get her" stays three words.
        let dict = d("together\n");
        assert_eq!(dict.apply("to get her"), "to get her");
    }

    #[test]
    fn longest_first() {
        let dict = d("claude -> Claude\nclaude code -> Claude Code\n");
        assert_eq!(
            dict.apply("claude code and claude"),
            "Claude Code and Claude"
        );
    }

    #[test]
    fn never_inside_protected() {
        let dict = d("audio -> Audio\n");
        assert_eq!(
            dict.apply("audio in @src/audio.rs"),
            "Audio in @src/audio.rs"
        );
    }

    #[test]
    fn symbols_in_terms() {
        let dict = d("C++\n");
        assert_eq!(dict.apply("write it in c++ today"), "write it in C++ today");
    }

    #[test]
    fn replacement_text_is_literal() {
        let dict = d("dollar sign -> $1\n");
        assert_eq!(dict.apply("a dollar sign"), "a $1");
    }

    #[test]
    fn split_jargon_rules() {
        assert_eq!(split_jargon("ChatGPT"), ["Chat", "GPT"]);
        assert_eq!(split_jargon("GitHub"), ["Git", "Hub"]);
        assert_eq!(split_jargon("SQLite"), ["SQ", "Lite"]);
        assert_eq!(split_jargon("gpt5"), ["gpt", "5"]);
        assert_eq!(split_jargon("iPhone"), ["i", "Phone"]);
    }

    #[test]
    fn add_creates_appends_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/dictionary.txt");
        assert!(add(&path, "Kubernetes").unwrap());
        assert!(!add(&path, "kubernetes").unwrap());
        assert!(add(&path, "cloud code -> Claude Code").unwrap());
        assert!(!add(&path, "Cloud  Code -> something else").unwrap());
        assert!(!add(&path, "  # just a comment").unwrap());
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("# squawk dictionary"));
        assert!(body.ends_with("Kubernetes\ncloud code -> Claude Code\n"));
        let dict = Dictionary::load(&path).unwrap();
        assert_eq!(dict.entries().len(), 2);
        assert_eq!(dict.terms().collect::<Vec<_>>(), ["Kubernetes"]);
    }

    #[test]
    fn add_to_file_without_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.txt");
        std::fs::write(&path, "Rust").unwrap();
        assert!(add(&path, "Tokio").unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Rust\nTokio\n");
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let dict = Dictionary::load(&dir.path().join("nope.txt")).unwrap();
        assert!(dict.is_empty());
        assert_eq!(dict.apply("unchanged"), "unchanged");
    }

    #[test]
    fn cache_reloads_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.txt");
        let mut cache = DictionaryCache::new(path.clone());
        assert!(cache.get().is_empty());
        std::fs::write(&path, "Rust\n").unwrap();
        assert_eq!(cache.get().entries().len(), 1);
        // Force a different mtime even on coarse filesystems.
        std::fs::write(&path, "Rust\nTokio\n").unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(later).unwrap();
        assert_eq!(cache.get().entries().len(), 2);
    }
}
