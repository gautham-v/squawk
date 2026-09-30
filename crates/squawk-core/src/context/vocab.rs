//! What a repo offers for fixing a transcript: its files (for @mentions) and
//! its jargon (for spelling).
//!
//! Files come from `git ls-files` (tracked plus untracked-but-not-ignored,
//! because the file Claude created a minute ago is the one you are most
//! likely to talk about), or from a bounded walk outside git.
//!
//! `words` holds only what the jargon pass may rewrite *towards*: package
//! names, the project's own name, distinctive file and directory stems
//! (`ContentView`, `gpt5`), and terms from the root docs (see terms.rs).
//! Plain stems like `audio` or `CLAUDE` are left out on purpose: they are
//! reachable as @mentions, and as words they would only do harm ("claude" is
//! not "CLAUDE").

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use super::index::Index;
use super::terms;

/// `git ls-files` stops here: enough for any repo you talk to by name.
pub(crate) const MAX_GIT_FILES: usize = 20_000;
/// The walk outside git stops at this many files, this deep.
const MAX_WALK_FILES: usize = 5_000;
const MAX_WALK_DEPTH: usize = 4;
/// And after looking at this many directory entries, whatever it found.
const MAX_WALK_ENTRIES: usize = 50_000;
/// Directories never worth listing.
const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    "build",
    "dist",
    "Library",
    "Pods",
    "DerivedData",
    "__pycache__",
    "venv",
];
/// Docs at the root whose terms count as jargon.
const DOC_FILES: &[&str] = &["README.md", "CLAUDE.md", "AGENTS.md", "CONTEXT.md"];
/// Big docs are read only this far.
const MAX_DOC_BYTES: usize = 256 * 1024;
/// Manifests read for package names.
const MAX_MANIFESTS: usize = 64;

/// What a repo offers for fixing a transcript.
#[derive(Clone, Default)]
pub struct RepoVocab {
    /// The directory mentions are relative to (the session cwd).
    pub root: PathBuf,
    /// Tracked files, relative to `root` (`src/audio.rs`), from `git ls-files`
    /// (or a bounded walk outside git).
    pub files: Vec<String>,
    /// Jargon in its canonical casing: crate/package names, distinctive
    /// stems, headings and capitalised terms from README.md / CLAUDE.md /
    /// CONTEXT.md / AGENTS.md.
    pub words: Vec<String>,
    /// Lookup tables for `apply`, built on first use from `files` + `words`
    /// (so they must not change after that).
    index: OnceLock<Index>,
}

impl fmt::Debug for RepoVocab {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RepoVocab")
            .field("root", &self.root)
            .field("files", &self.files.len())
            .field("words", &self.words)
            .finish()
    }
}

impl PartialEq for RepoVocab {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root && self.files == other.files && self.words == other.words
    }
}

impl Eq for RepoVocab {}

impl RepoVocab {
    /// A vocab from parts (tests, or a caller with its own file list).
    pub fn new(root: PathBuf, files: Vec<String>, words: Vec<String>) -> RepoVocab {
        RepoVocab {
            root,
            files,
            words,
            index: OnceLock::new(),
        }
    }

    /// Scan the repo at `cwd`. Never fails: an unreadable directory is an
    /// empty vocab. The lookup tables are built here too, so the first
    /// `apply` after a build is as fast as the rest.
    pub fn build(cwd: &Path) -> RepoVocab {
        let files = list_files(cwd);
        let words = collect_words(cwd, &files);
        let vocab = RepoVocab::new(cwd.to_path_buf(), files, words);
        vocab.index();
        vocab
    }

    pub(crate) fn index(&self) -> &Index {
        self.index
            .get_or_init(|| Index::build(&self.files, &self.words))
    }
}

/// Agent docs people keep out of git (the owner's own repos ignore
/// CLAUDE.md) but still talk about: listed whenever they exist.
const AGENT_DOCS: &[&str] = &["CLAUDE.md", "CLAUDE.local.md", "AGENTS.md", "CONTEXT.md"];

/// The repo's files relative to `cwd`: git when `cwd` is in a work tree,
/// else a bounded walk. Plus the agent docs, ignored or not.
fn list_files(cwd: &Path) -> Vec<String> {
    let mut files = super::cache::git_index_path(cwd)
        .and_then(|_| git_ls_files(cwd))
        .unwrap_or_else(|| walk_files(cwd));
    for doc in AGENT_DOCS {
        if !files.iter().any(|f| f == doc) && cwd.join(doc).is_file() {
            files.push(doc.to_string());
        }
    }
    files
}

/// `git ls-files`, tracked + untracked-not-ignored, relative to `cwd`.
/// `None` if git is missing or fails.
fn git_ls_files(cwd: &Path) -> Option<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "-c",
            "core.quotepath=off",
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut seen = HashSet::new();
    let files = out
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .filter_map(|p| std::str::from_utf8(p).ok())
        .filter(|p| seen.insert(*p))
        .take(MAX_GIT_FILES)
        .map(str::to_string)
        .collect();
    Some(files)
}

/// Breadth-first, depth ≤ 4, skipping build output and dependency trees.
fn walk_files(cwd: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut queue = std::collections::VecDeque::from([(cwd.to_path_buf(), 0usize)]);
    let mut entries = 0usize;
    while let Some((dir, depth)) = queue.pop_front() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<_> = read.flatten().collect();
        children.sort_by_key(|e| e.file_name());
        for entry in children {
            entries += 1;
            if files.len() >= MAX_WALK_FILES || entries >= MAX_WALK_ENTRIES {
                return files;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                if depth + 1 < MAX_WALK_DEPTH
                    && !name.starts_with('.')
                    && !SKIP_DIRS.contains(&name.as_str())
                {
                    queue.push_back((path, depth + 1));
                }
            } else if kind.is_file() {
                if let Ok(rel) = path.strip_prefix(cwd) {
                    files.push(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
    files
}

/// Jargon for `words`, deduped case-sensitively, first spelling wins.
fn collect_words(cwd: &Path, files: &[String]) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();

    // The project's own name ("claudebar"), and every package name.
    if let Some(name) = cwd.file_name().and_then(|n| n.to_str()) {
        words.push(name.to_string());
    }
    let manifests = files
        .iter()
        .filter(|f| {
            let base = f.rsplit('/').next().unwrap_or(f);
            base == "Cargo.toml" || base == "package.json"
        })
        .take(MAX_MANIFESTS);
    for rel in manifests {
        let Ok(text) = std::fs::read_to_string(cwd.join(rel)) else {
            continue;
        };
        if rel.ends_with(".toml") {
            words.extend(cargo_package_name(&text));
            words.extend(cargo_hyphenated_deps(&text));
        } else {
            words.extend(npm_package_name(&text));
        }
    }

    // Distinctive stems of files and directories.
    let mut dirs = HashSet::new();
    for f in files {
        let mut parts: Vec<&str> = f.split('/').collect();
        let base = parts.pop().unwrap_or_default();
        for d in parts {
            if dirs.insert(d) && is_distinctive_stem(d) {
                words.push(d.to_string());
            }
        }
        let stem = base.split('.').next().unwrap_or_default();
        if is_distinctive_stem(stem) {
            words.push(stem.to_string());
        }
    }

    // Terms from the docs, at the cwd and (if different) the git root.
    let mut roots = vec![cwd.to_path_buf()];
    if let Some(top) = super::cache::git_toplevel(cwd) {
        if top != cwd {
            roots.push(top);
        }
    }
    let mut docs = String::new();
    for root in &roots {
        for name in DOC_FILES {
            if let Ok(text) = std::fs::read(root.join(name)) {
                let text = &text[..text.len().min(MAX_DOC_BYTES)];
                docs.push_str(&String::from_utf8_lossy(text));
                docs.push('\n');
            }
        }
    }
    words.extend(terms::extract(&docs));

    let mut seen = HashSet::new();
    words.retain(|w| !w.is_empty() && seen.insert(w.clone()));
    words
}

/// Inner capitals with some lowercase (`ContentView`, `macOS`) or a
/// letter/digit mix (`gpt5`): a stem that is jargon, not an English word.
fn is_distinctive_stem(stem: &str) -> bool {
    if stem.is_empty() || !stem.chars().all(|c| c.is_alphanumeric()) {
        return false;
    }
    let camel = crate::text::has_inner_case(stem) && stem.chars().any(char::is_lowercase);
    let mixed = stem.chars().any(|c| c.is_ascii_digit()) && stem.chars().any(char::is_alphabetic);
    camel || mixed
}

/// `[package] name` from a Cargo.toml.
fn cargo_package_name(text: &str) -> Option<String> {
    let value: toml::Value = toml::from_str(text).ok()?;
    let name = value.get("package")?.get("name")?.as_str()?;
    Some(name.to_string())
}

/// Dependencies with a hyphen in the name (`transcribe-rs`): what "transcribe
/// rs" should join into. Unhyphenated ones are left out on purpose — joining
/// "this error" into `thiserror` would be wrong far more often than right.
fn cargo_hyphenated_deps(text: &str) -> Vec<String> {
    let Ok(value) = toml::from_str::<toml::Value>(text) else {
        return Vec::new();
    };
    let tables = [
        value.get("dependencies"),
        value.get("dev-dependencies"),
        value.get("workspace").and_then(|w| w.get("dependencies")),
    ];
    tables
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_table)
        .flat_map(|t| t.keys())
        .filter(|k| k.contains('-'))
        .cloned()
        .collect()
}

/// `name` from a package.json, without an npm scope (`@acme/widgets` →
/// `widgets`).
fn npm_package_name(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let name = value.get("name")?.as_str()?;
    let name = name.rsplit('/').next().unwrap_or(name);
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;

    /// A temp git repo with these files (path, contents), committed.
    pub(crate) fn git_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (path, body) in files {
            let p = dir.path().join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["add", "-A"]);
        git(
            dir.path(),
            &[
                "-c",
                "user.name=You",
                "-c",
                "user.email=you@example.com",
                "commit",
                "-qm",
                "init",
            ],
        );
        dir
    }

    pub(crate) fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git")
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn builds_from_a_git_repo() {
        let dir = git_repo(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n"),
            (
                "crates/widget-core/Cargo.toml",
                "[package]\nname = \"widget-core\"\nversion = \"0.1.0\"\n",
            ),
            ("crates/widget-core/src/lib.rs", ""),
            ("crates/widget-core/src/audio.rs", ""),
            ("web/package.json", r#"{"name": "@acme/widget-web"}"#),
            ("web/src/ContentView.tsx", ""),
            (
                "README.md",
                "# Widget\n\nWidget talks to Tidewell over GPUI. It uses tokio.\n",
            ),
            ("target/debug/junk.rs", ""),
            (".gitignore", "target/\nCLAUDE.md\n"),
            ("CLAUDE.md", "Ignored, but still a file you talk about.\n"),
        ]);
        // Untracked but not ignored: listed. Ignored: not.
        fs::write(dir.path().join("notes.md"), "").unwrap();
        let v = RepoVocab::build(dir.path());
        assert_eq!(v.root, dir.path());
        for f in [
            "Cargo.toml",
            "crates/widget-core/src/audio.rs",
            "web/src/ContentView.tsx",
            "notes.md",
            ".gitignore",
            "CLAUDE.md",
        ] {
            assert!(v.files.contains(&f.to_string()), "{f} in {:?}", v.files);
        }
        assert!(!v.files.iter().any(|f| f.starts_with("target/")));
        for w in [
            "widget-core",
            "widget-web",
            "ContentView",
            "Tidewell",
            "GPUI",
        ] {
            assert!(v.words.contains(&w.to_string()), "{w} in {:?}", v.words);
        }
        for w in ["audio", "lib", "tokio", "Cargo", "README"] {
            assert!(!v.words.contains(&w.to_string()), "{w} in {:?}", v.words);
        }
    }

    #[test]
    fn files_are_relative_to_a_subdirectory_cwd() {
        let dir = git_repo(&[
            ("app/src/main.rs", ""),
            ("lib/src/lib.rs", ""),
            ("CLAUDE.md", "Uses the Tidewell engine.\n"),
        ]);
        let v = RepoVocab::build(&dir.path().join("app"));
        assert_eq!(v.files, vec!["src/main.rs".to_string()]);
        // Docs at the git root still count.
        assert!(v.words.contains(&"Tidewell".to_string()));
    }

    #[test]
    fn walks_outside_git() {
        let dir = tempfile::tempdir().unwrap();
        for p in [
            "src/audio.rs",
            "node_modules/x/index.js",
            "target/a.rs",
            ".hidden/x.rs",
            "a/b/c/d/e/deep.rs",
        ] {
            let p = dir.path().join(p);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "").unwrap();
        }
        let v = RepoVocab::build(dir.path());
        assert_eq!(v.files, vec!["src/audio.rs".to_string()]);
    }

    #[test]
    fn missing_directory_is_empty() {
        let v = RepoVocab::build(Path::new("/nonexistent/squawk/test"));
        assert!(v.files.is_empty());
    }

    #[test]
    fn package_names() {
        assert_eq!(
            cargo_package_name("[package]\nname = \"squawk-core\"\n").as_deref(),
            Some("squawk-core")
        );
        assert_eq!(cargo_package_name("[workspace]\n"), None);
        assert_eq!(
            cargo_hyphenated_deps(
                "[dependencies]\ntranscribe-rs = \"1\"\nthiserror = \"2\"\n\
                 [workspace.dependencies]\nsquawk-core = { path = \"x\" }\n"
            ),
            vec!["transcribe-rs".to_string(), "squawk-core".to_string()]
        );
        assert_eq!(
            npm_package_name(r#"{"name":"@scope/thing"}"#).as_deref(),
            Some("thing")
        );
        assert_eq!(npm_package_name("not json"), None);
    }

    #[test]
    fn distinctive_stems() {
        for s in ["ContentView", "gpt5", "macOS"] {
            assert!(is_distinctive_stem(s), "{s}");
        }
        for s in ["audio", "CLAUDE", "README", "Cargo", "menu_bar", ""] {
            assert!(!is_distinctive_stem(s), "{s}");
        }
    }

    #[test]
    fn equality_ignores_the_index() {
        let a = RepoVocab::new("/r".into(), vec!["a.rs".into()], vec![]);
        let b = a.clone();
        a.index();
        assert_eq!(a, b);
    }
}
