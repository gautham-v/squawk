//! Where every file lives.
//!
//! Three roots, each for a different reader:
//! - `~/squawk` (the data dir) holds what the user and Claude read: dictations,
//!   meetings, the dictionary. Plain markdown and text, safe to grep.
//! - `~/.config/squawk/config.toml` is the one settings file.
//! - `~/Library/Application Support/squawk` holds what only squawk reads: the
//!   model, the socket, the log, kept audio.
//!
//! `SQUAWK_DATA_DIR` and `SQUAWK_SUPPORT_DIR` override the first and last
//! roots, so tests and a second copy of the app never touch the real ones.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub home: PathBuf,
    /// `~/.config/squawk/config.toml`
    pub config_file: PathBuf,
    /// `~/squawk`, or `data_dir` from the config.
    pub data_dir: PathBuf,
    /// `<data>/dictations`, one `YYYY-MM-DD.md` per day.
    pub dictations_dir: PathBuf,
    /// `<data>/meetings`, one `YYYY-MM-DD HHMM <title>.md` per meeting.
    pub meetings_dir: PathBuf,
    /// `<data>/dictionary.txt`
    pub dictionary_file: PathBuf,
    /// `~/Library/Application Support/squawk`
    pub support_dir: PathBuf,
    /// `<support>/models`; each model is a directory under it.
    pub models_dir: PathBuf,
    /// `<support>/squawk.sock`, the app's IPC socket.
    pub socket: PathBuf,
    /// `<support>/squawk.log`, one line per dictation with its latency.
    pub log_file: PathBuf,
    /// `<support>/audio`, WAVs kept only when `keep_audio = true`.
    pub audio_dir: PathBuf,
}

impl Paths {
    /// The real paths for this user, honouring the two env overrides. Does
    /// not read the config; apply its `data_dir` with [`Paths::with_data_dir`].
    pub fn detect() -> Result<Paths> {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or(Error::NoHome)?;
        let mut paths = Paths::from_home(&home);
        if let Some(dir) = env_dir("SQUAWK_DATA_DIR") {
            paths = paths.with_data_dir(dir);
        }
        if let Some(dir) = env_dir("SQUAWK_SUPPORT_DIR") {
            paths = paths.with_support_dir(dir);
        }
        Ok(paths)
    }

    /// The default layout under `home`, with no overrides.
    pub fn from_home(home: &Path) -> Paths {
        let config_file = home.join(".config").join("squawk").join("config.toml");
        let paths = Paths {
            home: home.to_path_buf(),
            config_file,
            data_dir: PathBuf::new(),
            dictations_dir: PathBuf::new(),
            meetings_dir: PathBuf::new(),
            dictionary_file: PathBuf::new(),
            support_dir: PathBuf::new(),
            models_dir: PathBuf::new(),
            socket: PathBuf::new(),
            log_file: PathBuf::new(),
            audio_dir: PathBuf::new(),
        };
        paths
            .with_data_dir(home.join("squawk"))
            .with_support_dir(home.join("Library/Application Support/squawk"))
    }

    /// Every file and dir under one root, for tests: data in `<root>/data`,
    /// support in `<root>/support`, config at `<root>/config.toml`.
    pub fn under(root: &Path) -> Paths {
        let mut paths = Paths::from_home(root)
            .with_data_dir(root.join("data"))
            .with_support_dir(root.join("support"));
        paths.config_file = root.join("config.toml");
        paths
    }

    /// Move the data root (and everything under it).
    pub fn with_data_dir(mut self, dir: PathBuf) -> Paths {
        self.dictations_dir = dir.join("dictations");
        self.meetings_dir = dir.join("meetings");
        self.dictionary_file = dir.join("dictionary.txt");
        self.data_dir = dir;
        self
    }

    /// Move the support root (and everything under it).
    pub fn with_support_dir(mut self, dir: PathBuf) -> Paths {
        self.models_dir = dir.join("models");
        self.socket = dir.join("squawk.sock");
        self.log_file = dir.join("squawk.log");
        self.audio_dir = dir.join("audio");
        self.support_dir = dir;
        self
    }

    /// Apply the config's `data_dir`, if it sets one. `~` expands to `home`.
    /// An env override wins over the config, so tests stay contained.
    pub fn with_config(self, config: &crate::Config) -> Paths {
        if std::env::var_os("SQUAWK_DATA_DIR").is_some() {
            return self;
        }
        match config.data_dir.as_deref().map(str::trim) {
            Some(dir) if !dir.is_empty() => {
                let dir = expand_tilde(dir, &self.home);
                self.with_data_dir(dir)
            }
            _ => self,
        }
    }

    /// Create the data dirs and the support dir. Idempotent.
    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [
            &self.dictations_dir,
            &self.meetings_dir,
            &self.support_dir,
            &self.models_dir,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

/// `~` or `~/x` becomes a path under `home`; anything else is taken as is.
pub fn expand_tilde(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        home.to_path_buf()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(path)
    }
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout() {
        let p = Paths::from_home(Path::new("/Users/you"));
        assert_eq!(p.data_dir, Path::new("/Users/you/squawk"));
        assert_eq!(p.dictations_dir, Path::new("/Users/you/squawk/dictations"));
        assert_eq!(p.meetings_dir, Path::new("/Users/you/squawk/meetings"));
        assert_eq!(
            p.dictionary_file,
            Path::new("/Users/you/squawk/dictionary.txt")
        );
        assert_eq!(
            p.config_file,
            Path::new("/Users/you/.config/squawk/config.toml")
        );
        assert_eq!(
            p.socket,
            Path::new("/Users/you/Library/Application Support/squawk/squawk.sock")
        );
        assert_eq!(
            p.models_dir,
            Path::new("/Users/you/Library/Application Support/squawk/models")
        );
    }

    #[test]
    fn data_dir_moves_everything_under_it() {
        let p = Paths::from_home(Path::new("/h")).with_data_dir("/d".into());
        assert_eq!(p.dictations_dir, Path::new("/d/dictations"));
        assert_eq!(p.dictionary_file, Path::new("/d/dictionary.txt"));
        // Support files stay put.
        assert_eq!(
            p.log_file,
            Path::new("/h/Library/Application Support/squawk/squawk.log")
        );
    }

    #[test]
    fn tilde_expands() {
        let home = Path::new("/Users/you");
        assert_eq!(expand_tilde("~", home), home);
        assert_eq!(expand_tilde("~/notes/sq", home), home.join("notes/sq"));
        assert_eq!(expand_tilde("/abs", home), Path::new("/abs"));
        assert_eq!(expand_tilde("~other", home), Path::new("~other"));
    }

    #[test]
    fn under_keeps_tests_contained() {
        let p = Paths::under(Path::new("/tmp/x"));
        assert!(p.config_file.starts_with("/tmp/x"));
        assert!(p.socket.starts_with("/tmp/x"));
        assert!(p.meetings_dir.starts_with("/tmp/x"));
    }
}
