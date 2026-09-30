//! Claude Code mode: when the front app is a terminal running `claude` (or
//! `codex`), find that session's working directory and use the repo to fix
//! what the model heard.
//!
//! OWNED BY THE CONTEXT AGENT. This file holds the agreed public API with
//! stub bodies (detection finds nothing, `apply` passes text through) so the
//! rest of the workspace builds against it. See SPEC.md, "squawk-core::context".
//! The implementation may split into submodules (detect.rs, vocab.rs,
//! mentions.rs); the items below must stay exported from `context`.
//!
//! Timing contract: the app calls [`detect`] and [`VocabCache::get`] when a
//! recording *starts* (off the main thread, while the user is still talking),
//! and only [`apply`] after release. `apply` must take well under 5 ms;
//! `detect` + a warm `get` under 20 ms; a cold `get` (first time in a repo)
//! may take longer but must stay under ~200 ms on a large repo.

use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// Find the agent session behind the front terminal: a `claude`/`codex`
/// process descended from `front.pid`; if several, the one whose controlling
/// tty was used most recently (tty device atime/mtime). `None` if the front
/// app is not a terminal or runs no agent. Uses libproc (proc_listpids,
/// proc_pidinfo PROC_PIDTBSDINFO / PROC_PIDVNODEPATHINFO); no subprocesses.
///
/// STUB: always `None`.
pub fn detect(front: &FrontApp) -> Option<Session> {
    let _ = front;
    None
}

/// What a repo offers for fixing a transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoVocab {
    /// The directory mentions are relative to (the session cwd).
    pub root: PathBuf,
    /// Tracked files, relative to `root` (`src/audio.rs`), from `git ls-files`
    /// (or a bounded walk outside git).
    pub files: Vec<String>,
    /// Jargon in its canonical casing: file stems, crate/package names,
    /// headings and capitalised terms from README.md / CLAUDE.md / CONTEXT.md.
    pub words: Vec<String>,
}

impl RepoVocab {
    /// Scan the repo at `cwd`. STUB: empty vocab rooted at `cwd`.
    pub fn build(cwd: &Path) -> RepoVocab {
        RepoVocab {
            root: cwd.to_path_buf(),
            ..RepoVocab::default()
        }
    }
}

/// Per-cwd vocab, rebuilt when the repo's `.git/index` mtime changes (or,
/// outside git, after a short TTL). Owned by the app's dictation controller;
/// not shared between threads.
#[derive(Debug, Default)]
pub struct VocabCache {
    entries: std::collections::HashMap<PathBuf, Arc<RepoVocab>>,
}

impl VocabCache {
    pub fn new() -> VocabCache {
        VocabCache::default()
    }

    /// The vocab for `cwd`, building or rebuilding it if needed.
    /// STUB: builds every time, no invalidation.
    pub fn get(&mut self, cwd: &Path) -> Arc<RepoVocab> {
        let vocab = Arc::new(RepoVocab::build(cwd));
        self.entries.insert(cwd.to_path_buf(), vocab.clone());
        vocab
    }
}

/// A detected session and its repo vocab: what the pipeline needs.
#[derive(Debug, Clone)]
pub struct Context {
    pub session: Session,
    pub vocab: Arc<RepoVocab>,
}

/// Rewrite `text` for the session:
/// 1. spoken file references become @mentions relative to the cwd, only when
///    the match is unique ("audio dot rs" → `@src/audio.rs`, "the claude md"
///    → `@CLAUDE.md`, "cargo toml" → `@Cargo.toml`);
/// 2. repo jargon gets its canonical spelling and casing.
///
/// Runs after `cleanup::strip` and before the dictionary. Must never touch
/// protected chunks (`text::is_protected`) it did not create, and must be
/// conservative: an ambiguous reference stays as spoken.
///
/// STUB: returns `text` unchanged.
pub fn apply(text: &str, context: &Context) -> String {
    let _ = context;
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
