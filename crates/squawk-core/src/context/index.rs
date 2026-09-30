//! Lookup tables built once per [`RepoVocab`](super::RepoVocab), so `apply`
//! is hash lookups rather than scans of 20 000 paths.
//!
//! Names are compared *squashed*: lowercase, letters and digits only, dots
//! kept. "status item dot rs", "StatusItem.rs" and `status_item.rs` all
//! squash to `statusitem.rs`, which is how the model's spacing and the repo's
//! naming convention stop mattering.

use std::collections::{HashMap, HashSet};

use super::stopwords::is_stopword;
use crate::text::has_inner_case;

/// Stems that name a file on their own, no extension spoken ("the readme").
pub(crate) const STEM_ALIASES: &[&str] = &["readme", "changelog"];

/// Extensionless file names distinctive enough to match by themselves.
const BARE_NAMES: &[&str] = &[
    "makefile",
    "dockerfile",
    "containerfile",
    "justfile",
    "gemfile",
    "rakefile",
    "procfile",
    "brewfile",
    "podfile",
    "vagrantfile",
    "jenkinsfile",
];

#[derive(Debug, Clone, Default)]
pub(crate) struct Index {
    paths: Vec<String>,
    /// Squashed basename (`statusitem.rs`, `.gitignore`, `makefile`) → files.
    by_name: HashMap<String, Vec<u32>>,
    /// Squashed parent + "/" + basename (`src/statusitem.rs`) → files: the
    /// same, narrowed by one spoken directory without scanning a thousand
    /// `mod.rs`.
    by_parent: HashMap<String, Vec<u32>>,
    /// [`STEM_ALIASES`] stem → files (`readme` → README.md, docs/README.md).
    by_stem: HashMap<String, Vec<u32>>,
    /// Extensions present in the repo, lowercase.
    exts: HashSet<String>,
    /// Lowercase word → canonical spelling, for single tokens.
    exact: HashMap<String, String>,
    /// Squashed word → canonical spelling, for tokens the model split.
    joined: HashMap<String, String>,
}

impl Index {
    pub(crate) fn build(files: &[String], words: &[String]) -> Index {
        let mut index = Index {
            paths: files.to_vec(),
            ..Index::default()
        };
        for (i, path) in files.iter().enumerate() {
            let i = i as u32;
            // "@My Notes.md" would not survive as one mention.
            if path.contains(char::is_whitespace) {
                continue;
            }
            let base = path.rsplit('/').next().unwrap_or(path);
            let name = squash_name(base);
            if name.is_empty() {
                continue;
            }
            if let Some((stem, ext)) = name.rsplit_once('.') {
                if !stem.is_empty() && !ext.is_empty() {
                    index.exts.insert(ext.to_string());
                }
            }
            let stem = name.split('.').next().unwrap_or_default();
            if STEM_ALIASES.contains(&stem) {
                index.by_stem.entry(stem.to_string()).or_default().push(i);
            }
            if let Some(parent) = path.rsplit('/').nth(1) {
                let key = format!("{}/{name}", squash(parent));
                index.by_parent.entry(key).or_default().push(i);
            }
            index.by_name.entry(name).or_default().push(i);
        }
        let candidates = words
            .iter()
            .filter(|w| looks_english(w))
            .map(|w| w.to_lowercase())
            .collect();
        let english = super::english::common(&candidates);
        for word in words {
            index.add_word(word, english.contains(&word.to_lowercase()));
        }
        index
    }

    /// `english`: the word is an ordinary English word in lowercase ("Dock"),
    /// so only a split of it is fixed, never its casing.
    fn add_word(&mut self, word: &str, english: bool) {
        if word.is_empty() || word.chars().any(char::is_whitespace) || is_stopword(word) {
            return;
        }
        let lower = word.to_lowercase();
        let chars = word.chars().count();
        let distinctive = has_inner_case(word)
            || word.chars().any(|c| c.is_ascii_digit())
            || word.contains(['-', '_']);
        // Rewriting a single token only ever adds capitals the repo uses.
        if word != lower && (chars >= 4 || distinctive) && !english {
            self.exact.entry(lower).or_insert_with(|| word.to_string());
        }
        // Joining split tokens: never towards snake_case (people say "status
        // item" meaning the thing, not the identifier).
        let squashed = squash(word);
        if !word.contains('_') && (squashed.len() >= 5 || distinctive) {
            self.joined
                .entry(squashed)
                .or_insert_with(|| word.to_string());
        }
    }

    pub(crate) fn has_files(&self) -> bool {
        !self.paths.is_empty()
    }

    pub(crate) fn has_jargon(&self) -> bool {
        !self.exact.is_empty() || !self.joined.is_empty()
    }

    pub(crate) fn path(&self, i: u32) -> &str {
        &self.paths[i as usize]
    }

    pub(crate) fn by_name(&self, name: &str) -> &[u32] {
        self.by_name.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Files named `name` directly inside a directory squashing to `parent`.
    pub(crate) fn by_parent(&self, parent: &str, name: &str) -> &[u32] {
        self.by_parent
            .get(&format!("{parent}/{name}"))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn by_stem(&self, stem: &str) -> &[u32] {
        self.by_stem.get(stem).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(crate) fn has_ext(&self, ext: &str) -> bool {
        self.exts.contains(ext)
    }

    pub(crate) fn is_bare_name(name: &str) -> bool {
        BARE_NAMES.contains(&name)
    }

    /// Canonical spelling of a single token, if it differs in case only.
    pub(crate) fn exact(&self, lower: &str) -> Option<&str> {
        self.exact.get(lower).map(String::as_str)
    }

    /// Canonical spelling of split tokens, keyed by their squashed join.
    pub(crate) fn joined(&self, squashed: &str) -> Option<&str> {
        self.joined.get(squashed).map(String::as_str)
    }
}

/// `Dock`, `NOTE`: capitalised or shouted, but maybe just a word. (Short
/// acronyms like `ID`, `UI`, `TUI` are taken at face value.)
fn looks_english(word: &str) -> bool {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let rest: Vec<char> = chars.collect();
    let capitalised = first.is_uppercase() && rest.iter().all(|c| c.is_lowercase());
    let shouted = word.chars().count() >= 4 && word.chars().all(|c| c.is_uppercase());
    word.chars().all(char::is_alphabetic) && (capitalised || shouted)
}

/// Lowercase letters and digits only.
pub(crate) fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// [`squash`], keeping dots: `status_item.rs` → `statusitem.rs`.
pub(crate) fn squash_name(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '.')
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn names_are_squashed() {
        let idx = Index::build(
            &strings(&[
                "src/status_item.rs",
                "README.md",
                "docs/README.md",
                ".gitignore",
                "Makefile",
            ]),
            &[],
        );
        assert_eq!(idx.by_name("statusitem.rs"), &[0]);
        assert_eq!(idx.by_parent("src", "statusitem.rs"), &[0]);
        assert_eq!(idx.by_parent("docs", "readme.md"), &[2]);
        assert!(idx.by_parent("", "readme.md").is_empty());
        assert_eq!(idx.by_name(".gitignore"), &[3]);
        assert_eq!(idx.by_name("makefile"), &[4]);
        assert_eq!(idx.by_stem("readme"), &[1, 2]);
        assert!(idx.has_ext("rs") && idx.has_ext("md"));
        assert!(!idx.has_ext("gitignore"));
    }

    #[test]
    fn words_feed_the_right_tables() {
        let idx = Index::build(
            &[],
            &strings(&[
                "Tokio",
                "claudebar",
                "squawk-core",
                "keep_audio",
                "TUI",
                "Settings",
                "Rx",
                "Dock",
            ]),
        );
        assert_eq!(idx.exact("tokio"), Some("Tokio"));
        assert_eq!(idx.joined("tokio"), Some("Tokio"));
        assert_eq!(idx.exact("tui"), Some("TUI"));
        assert_eq!(idx.exact("claudebar"), None);
        assert_eq!(idx.exact("settings"), None);
        assert_eq!(idx.exact("rx"), None);
        assert_eq!(idx.joined("claudebar"), Some("claudebar"));
        assert_eq!(idx.joined("squawkcore"), Some("squawk-core"));
        assert_eq!(idx.joined("keepaudio"), None);
        assert_eq!(idx.joined("tui"), Some("TUI"));
        // An English word: joins only, never recased.
        if cfg!(target_os = "macos") {
            assert_eq!(idx.exact("dock"), None);
        }
    }
}
