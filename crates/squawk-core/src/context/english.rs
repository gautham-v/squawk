//! Is this capitalised doc term just an English word?
//!
//! A README that says "drag it to the Dock" or "System Settings > Privacy &
//! Security" makes "Dock" and "Privacy" look like names; recasing every
//! "dock" or "privacy" you say would be wrong. The stoplist catches the most
//! common words; for the rest, macOS ships a word list (`/usr/share/dict/
//! words`, Webster's 2nd) whose lowercase entries are ordinary words. Only
//! the handful of candidate terms are looked up, in one pass over the file,
//! so nothing stays in memory. Without the file, nothing is filtered.

use std::collections::HashSet;

const WORD_LIST: &str = "/usr/share/dict/words";

/// The candidates (lowercase) that are ordinary lowercase English words,
/// including simple plurals of one ("teams" → "team").
pub(crate) fn common(candidates: &HashSet<String>) -> HashSet<String> {
    if candidates.is_empty() {
        return HashSet::new();
    }
    let Ok(list) = std::fs::read_to_string(WORD_LIST) else {
        return HashSet::new();
    };
    common_in(&list, candidates)
}

fn common_in(list: &str, candidates: &HashSet<String>) -> HashSet<String> {
    let singular = |w: &str| -> Option<String> { w.strip_suffix('s').map(str::to_string) };
    let mut wanted: HashSet<String> = candidates.clone();
    wanted.extend(candidates.iter().filter_map(|w| singular(w)));
    let found: HashSet<&str> = list
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with(|c: char| c.is_lowercase()) && wanted.contains(*l))
        .collect();
    candidates
        .iter()
        .filter(|w| {
            found.contains(w.as_str()) || singular(w).is_some_and(|s| found.contains(s.as_str()))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn lowercase_entries_and_plurals() {
        let list = "Dock\ndock\nteam\nSaturday\nzoom\n";
        let got = common_in(list, &set(&["dock", "teams", "saturday", "tokio", "zoom"]));
        assert_eq!(got, set(&["dock", "teams", "zoom"]));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn system_list() {
        let got = common(&set(&["privacy", "tokio", "xcode"]));
        assert_eq!(got, set(&["privacy"]));
    }
}
