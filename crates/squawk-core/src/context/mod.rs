//! Claude Code mode: when the front app is a terminal running `claude` (or
//! `codex`), find that session's working directory and use the repo to fix
//! what the model heard.
//!
//! - [`detect`] (detect.rs) walks the process table under the front terminal.
//! - [`RepoVocab`] (vocab.rs) lists the repo's files and jargon; [`VocabCache`]
//!   (cache.rs) keeps one per cwd and notices when the repo changes.
//! - [`apply`] turns spoken file names into `@mentions` (mentions.rs) and
//!   gives repo jargon its canonical spelling (jargon.rs), using lookup tables
//!   built once per vocab (index.rs).
//!
//! Timing contract: the app calls [`detect`] and [`VocabCache::get`] when a
//! recording *starts* (off the main thread, while the user is still talking),
//! and only [`apply`] after release. `apply` must take well under 5 ms;
//! `detect` + a warm `get` under 20 ms; a cold `get` (first time in a repo)
//! may take longer but must stay under ~200 ms on a large repo.

mod cache;
mod detect;
mod english;
mod index;
mod jargon;
mod mentions;
mod stopwords;
mod terms;
mod vocab;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use cache::VocabCache;
pub use detect::detect;
pub use vocab::RepoVocab;

/// Terminal apps squawk knows, by bundle id.
pub const TERMINAL_BUNDLE_IDS: &[&str] = &[
    "com.mitchellh.ghostty",
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "com.github.wez.wezterm",
    "dev.warp.Warp-Stable",
    "dev.warp.Warp",
    "net.kovidgoyal.kitty",
    "org.alacritty",
    "io.alacritty",
];

/// Process names that count as a coding agent session.
pub const AGENT_PROCESS_NAMES: &[(&str, Agent)] =
    &[("claude", Agent::Claude), ("codex", Agent::Codex)];

/// The frontmost app, as the app reads it from NSWorkspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontApp {
    pub bundle_id: String,
    /// Localized name: "Ghostty". Goes in the dictation heading.
    pub name: String,
    pub pid: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Agent {
    Claude,
    Codex,
}

/// A coding agent running under the front terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub agent: Agent,
    pub pid: i32,
    /// The agent process's current working directory.
    pub cwd: PathBuf,
    /// Its controlling terminal (`/dev/ttys004`), when it has one.
    pub tty: Option<PathBuf>,
}

impl Session {
    /// The cwd's last component: the "project" in a dictation heading.
    pub fn project(&self) -> String {
        project_name(&self.cwd)
    }
}

/// Last path component, or the whole path if there is none.
pub fn project_name(cwd: &Path) -> String {
    cwd.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.display().to_string())
}

/// Is this bundle id a terminal squawk looks inside?
pub fn is_terminal(bundle_id: &str) -> bool {
    TERMINAL_BUNDLE_IDS.contains(&bundle_id)
}

/// A detected session and its repo vocab: what the pipeline needs.
#[derive(Debug, Clone)]
pub struct Context {
    pub session: Session,
    pub vocab: Arc<RepoVocab>,
}

/// Rewrite `text` for the session:
/// 1. spoken file references become @mentions relative to the cwd, only when
///    the match is unique or clearly best ("audio dot rs" → `@src/audio.rs`,
///    "the claude md" → `@CLAUDE.md`, "cargo toml" → `@Cargo.toml`);
/// 2. repo jargon gets its canonical spelling and casing ("tide well" →
///    `Tidewell` when the repo writes it that way).
///
/// Runs after `cleanup::strip` and before the dictionary. Never touches
/// protected chunks it did not create (an existing `@mention`, a URL, a
/// path), and is conservative: an ambiguous reference stays as spoken.
pub fn apply(text: &str, context: &Context) -> String {
    apply_vocab(text, &context.vocab)
}

/// [`apply`] without a session: all it needs is the vocab.
pub(crate) fn apply_vocab(text: &str, vocab: &RepoVocab) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }
    let index = vocab.index();
    let chunks = mentions::apply(crate::text::chunks(text), index);
    let chunks = jargon::apply(chunks, index);
    crate::text::join(&chunks)
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    /// A vocab rooted at `/repo` with these files and words.
    pub fn vocab(files: &[&str], words: &[&str]) -> RepoVocab {
        RepoVocab::new(
            PathBuf::from("/repo"),
            files.iter().map(|s| s.to_string()).collect(),
            words.iter().map(|s| s.to_string()).collect(),
        )
    }

    pub fn context(files: &[&str], words: &[&str]) -> Context {
        Context {
            session: Session {
                agent: Agent::Claude,
                pid: 0,
                cwd: PathBuf::from("/repo"),
                tty: None,
            },
            vocab: Arc::new(vocab(files, words)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::context;
    use super::*;
    use crate::cleanup::CleanupOptions;
    use crate::dictionary::Dictionary;
    use crate::pipeline;

    #[test]
    fn terminals() {
        assert!(is_terminal("com.mitchellh.ghostty"));
        assert!(is_terminal("com.apple.Terminal"));
        assert!(!is_terminal("com.apple.Safari"));
    }

    #[test]
    fn project_is_last_component() {
        let s = Session {
            agent: Agent::Claude,
            pid: 1,
            cwd: PathBuf::from("/Users/you/code/squawk"),
            tty: None,
        };
        assert_eq!(s.project(), "squawk");
        assert_eq!(project_name(Path::new("/")), "/");
    }

    #[test]
    fn mentions_and_jargon_together() {
        let ctx = context(
            &["src/audio.rs", "Cargo.toml", "CLAUDE.md"],
            &["Tidewell", "GPUI"],
        );
        assert_eq!(
            apply("so the tide well gpui code in audio dot rs", &ctx),
            "so the Tidewell GPUI code in @src/audio.rs"
        );
    }

    #[test]
    fn empty_text_passes_through() {
        let ctx = context(&["src/audio.rs"], &[]);
        assert_eq!(apply("", &ctx), "");
    }

    #[test]
    fn through_the_whole_pipeline() {
        let ctx = context(&["src/audio.rs", "Cargo.toml", "CLAUDE.md"], &["Tokio"]);
        let opts = CleanupOptions::default();
        let dict = Dictionary::default();
        let cases = [
            ("Um, look at the cargo toml.", "Look at @Cargo.toml"),
            (
                "Read the claude md and then fix audio dot rs, okay?",
                "Read @CLAUDE.md and then fix @src/audio.rs, okay?",
            ),
            (
                "The audio dot rs file uses tokio.",
                "@src/audio.rs uses Tokio.",
            ),
            (
                "check audio dot rs. Then run it",
                "Check @src/audio.rs Then run it.",
            ),
        ];
        for (raw, want) in cases {
            assert_eq!(
                pipeline::finish(raw, &dict, Some(&ctx), &opts),
                want,
                "{raw}"
            );
        }
    }
}

/// Timing: `apply` on a 20k-file repo, and a cold/warm cache on a real one.
#[cfg(test)]
mod perf {
    use super::*;
    use std::time::{Duration, Instant};

    fn big_vocab() -> RepoVocab {
        let mut files = Vec::with_capacity(20_000);
        for c in 0..40 {
            for m in 0..25 {
                for f in 0..20 {
                    files.push(format!("crates/crate_{c}/src/module_{m}/file_{f}.rs"));
                }
            }
        }
        files.extend(["src/audio.rs".to_string(), "Cargo.toml".to_string()]);
        let words = (0..2_000)
            .map(|i| format!("Term{i}x"))
            .chain(["Tidewell".into()])
            .collect();
        RepoVocab::new(PathBuf::from("/repo"), files, words)
    }

    /// Debug builds are ~10x slower than release; the budget is for release
    /// (< 5 ms), so debug gets 20 ms.
    #[test]
    fn apply_is_fast_on_a_20k_file_repo() {
        let vocab = big_vocab();
        let t = Instant::now();
        vocab.index();
        let build = t.elapsed();
        let text = "so um look at the audio dot rs file and the cargo toml, then check \
                    file underscore seven dot rs in module underscore three and tell tide well \
                    that the crate underscore twelve slash source slash module underscore one \
                    slash file underscore two dot rs is wrong because it never reads the config "
            .repeat(3);
        let mut worst = Duration::ZERO;
        let t = Instant::now();
        for _ in 0..20 {
            let one = Instant::now();
            let out = apply_vocab(&text, &vocab);
            worst = worst.max(one.elapsed());
            assert!(out.contains("@src/audio.rs"), "{out}");
            assert!(
                out.contains("@crates/crate_12/src/module_1/file_2.rs"),
                "{out}"
            );
            assert!(out.contains("Tidewell"), "{out}");
        }
        let avg = t.elapsed() / 20;
        eprintln!(
            "index build {build:?}, apply avg {avg:?}, worst {worst:?} ({} words)",
            text.split_whitespace().count()
        );
        assert!(worst < Duration::from_millis(20), "worst {worst:?}");
    }

    /// `SQUAWK_BIG_REPO=1 cargo test -p squawk-core --release big_repo -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn big_repo_cold_and_warm() {
        let dir = tempfile::tempdir().unwrap();
        for c in 0..40 {
            for m in 0..25 {
                let d = dir.path().join(format!("crates/c{c}/src/m{m}"));
                std::fs::create_dir_all(&d).unwrap();
                for f in 0..20 {
                    std::fs::write(d.join(format!("file_{f}.rs")), "").unwrap();
                }
            }
        }
        vocab::tests::git(dir.path(), &["init", "-q"]);
        vocab::tests::git(dir.path(), &["add", "-A"]);
        let mut cache = VocabCache::new();
        let t = Instant::now();
        let v = cache.get(dir.path());
        let cold = t.elapsed();
        let t = Instant::now();
        let _ = cache.get(dir.path());
        let warm = t.elapsed();
        eprintln!("{} files: cold {cold:?}, warm {warm:?}", v.files.len());
        assert_eq!(v.files.len(), 20_000);
        assert!(warm < Duration::from_millis(1));
    }

    /// Try apply against a real checkout, read-only:
    /// `SQUAWK_TRY_REPO=~/code/x SQUAWK_TRY_TEXT='look at the cargo toml|…' cargo test -p squawk-core try_repo -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn try_repo() {
        let Ok(repo) = std::env::var("SQUAWK_TRY_REPO") else {
            return;
        };
        let t = Instant::now();
        let vocab = RepoVocab::build(Path::new(&repo));
        eprintln!(
            "built in {:?}: {} files, words {:?}",
            t.elapsed(),
            vocab.files.len(),
            vocab.words
        );
        let text = std::env::var("SQUAWK_TRY_TEXT").unwrap_or_default();
        for line in text.split('|') {
            let t = Instant::now();
            let out = apply_vocab(line.trim(), &vocab);
            eprintln!("{:>8.2?}  {line:?}\n          -> {out:?}", t.elapsed());
        }
    }
}
