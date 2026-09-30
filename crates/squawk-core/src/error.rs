//! The one error type for squawk-core.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// A config file that exists but does not parse. The message is phrased
    /// for a human: it names the file and what is wrong with it.
    #[error("{path}: {message}")]
    Config { path: PathBuf, message: String },

    #[error("bad JSON: {0}")]
    Json(#[from] serde_json::Error),

    /// The home directory could not be found (no `$HOME`).
    #[error("could not find the home directory")]
    NoHome,

    /// Nothing is listening on the socket: the app is not running.
    #[error("squawk is not running (no socket at {0})")]
    NotRunning(PathBuf),

    /// The app answered, but not with something we understood, or not in time.
    #[error("{0}")]
    Ipc(String),
}
