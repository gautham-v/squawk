//! One [`RepoVocab`] per cwd, rebuilt when the repo changes.
//!
//! "Changed" is the git index's mtime (it moves on add, commit, checkout,
//! pull, and on the refresh `git status` does) — or, since a file Claude just
//! created is untracked and moves nothing, simply age: an entry older than a
//! minute is rebuilt too. A warm hit costs one `stat`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use super::RepoVocab;

/// Rebuild an entry this old even if the index did not move.
const MAX_AGE: Duration = Duration::from_secs(60);
/// Repos remembered at once; the least recently used goes first.
const MAX_ENTRIES: usize = 16;

/// Per-cwd vocab, rebuilt when the repo's `.git/index` mtime changes (or
/// after a minute). Owned by the app's dictation controller; not shared
/// between threads.
#[derive(Debug, Default)]
pub struct VocabCache {
    entries: HashMap<PathBuf, Entry>,
}

#[derive(Debug)]
struct Entry {
    vocab: Arc<RepoVocab>,
    /// The git index this cwd belongs to, found once.
    index_path: Option<PathBuf>,
    index_mtime: Option<SystemTime>,
    built: Instant,
    used: Instant,
}

impl VocabCache {
    pub fn new() -> VocabCache {
        VocabCache::default()
    }

    /// The vocab for `cwd`, building or rebuilding it if needed.
    pub fn get(&mut self, cwd: &Path) -> Arc<RepoVocab> {
        let now = Instant::now();
        if let Some(entry) = self.entries.get_mut(cwd) {
            let index_unchanged = match &entry.index_path {
                Some(path) => mtime(path) == entry.index_mtime,
                None => true,
            };
            if index_unchanged && now.duration_since(entry.built) < MAX_AGE {
                entry.used = now;
                return entry.vocab.clone();
            }
        }
        let index_path = git_index_path(cwd);
        // Stat before building: a change during the build means a rebuild
        // next time, not a stale entry forever.
        let index_mtime = index_path.as_deref().and_then(mtime);
        let vocab = Arc::new(RepoVocab::build(cwd));
        if self.entries.len() >= MAX_ENTRIES && !self.entries.contains_key(cwd) {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| k.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            cwd.to_path_buf(),
            Entry {
                vocab: vocab.clone(),
                index_path,
                index_mtime,
                built: now,
                used: now,
            },
        );
        vocab
    }

    /// Forget everything (tests; a settings change).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    fn age(&mut self, cwd: &Path, by: Duration) {
        if let Some(e) = self.entries.get_mut(cwd) {
            e.built -= by;
        }
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The work tree `cwd` is in: the nearest ancestor with a `.git` entry.
pub(crate) fn git_toplevel(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// The index file of the work tree `cwd` is in. `.git` is a directory in a
/// normal checkout and a `gitdir: <path>` file in a worktree or submodule.
pub(crate) fn git_index_path(cwd: &Path) -> Option<PathBuf> {
    let top = git_toplevel(cwd)?;
    let dot_git = top.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git.join("index"));
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let gitdir = PathBuf::from(gitdir);
    let gitdir = if gitdir.is_absolute() {
        gitdir
    } else {
        top.join(gitdir)
    };
    Some(gitdir.join("index"))
}

#[cfg(test)]
mod tests {
    use super::super::vocab::tests::{git, git_repo};
    use super::*;
    use std::fs;

    #[test]
    fn warm_hits_share_the_vocab() {
        let dir = git_repo(&[("src/audio.rs", "")]);
        let mut cache = VocabCache::new();
        let a = cache.get(dir.path());
        let b = cache.get(dir.path());
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn rebuilds_when_the_index_moves() {
        let dir = git_repo(&[("src/audio.rs", "")]);
        let mut cache = VocabCache::new();
        let a = cache.get(dir.path());
        fs::write(dir.path().join("src/meeting.rs"), "").unwrap();
        // mtime resolution: make sure the index's mtime really differs.
        std::thread::sleep(Duration::from_millis(20));
        git(dir.path(), &["add", "-A"]);
        let index = git_index_path(dir.path()).unwrap();
        let t = SystemTime::now() + Duration::from_secs(5);
        fs::File::options()
            .write(true)
            .open(&index)
            .unwrap()
            .set_modified(t)
            .unwrap();
        let b = cache.get(dir.path());
        assert!(!Arc::ptr_eq(&a, &b));
        assert!(b.files.contains(&"src/meeting.rs".to_string()));
    }

    #[test]
    fn rebuilds_when_old() {
        let dir = git_repo(&[("src/audio.rs", "")]);
        let mut cache = VocabCache::new();
        let a = cache.get(dir.path());
        // An untracked file moves nothing in git; age catches it.
        fs::write(dir.path().join("src/new.rs"), "").unwrap();
        cache.age(dir.path(), MAX_AGE);
        let b = cache.get(dir.path());
        assert!(!Arc::ptr_eq(&a, &b));
        assert!(b.files.contains(&"src/new.rs".to_string()));
    }

    #[test]
    fn outside_git_uses_age_only() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "").unwrap();
        let mut cache = VocabCache::new();
        let a = cache.get(dir.path());
        let b = cache.get(dir.path());
        assert!(Arc::ptr_eq(&a, &b));
        cache.age(dir.path(), MAX_AGE);
        assert!(!Arc::ptr_eq(&a, &cache.get(dir.path())));
    }

    #[test]
    fn evicts_the_least_recently_used() {
        let dirs: Vec<_> = (0..=MAX_ENTRIES)
            .map(|_| tempfile::tempdir().unwrap())
            .collect();
        let mut cache = VocabCache::new();
        for d in &dirs {
            cache.get(d.path());
        }
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert!(!cache.entries.contains_key(dirs[0].path()));
    }

    #[test]
    fn index_path_in_a_worktree_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(".git"),
            "gitdir: ../main/.git/worktrees/x\n",
        )
        .unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        assert_eq!(
            git_index_path(&dir.path().join("sub")),
            Some(dir.path().join("../main/.git/worktrees/x/index"))
        );
    }
}
