//! squawk-core: everything the app, the engine and the CLI agree on.
//!
//! No audio, no AppKit, no model here — only the pure pieces: where files
//! live, what they look like, how a raw transcript becomes the text that gets
//! pasted, how fn presses become actions, and what goes over the socket. That
//! keeps all of it unit-testable without a microphone or a menu bar.

pub mod cleanup;
pub mod config;
pub mod config_edit;
pub mod context;
pub mod dictionary;
pub mod error;
pub mod hotkey;
pub mod ipc;
pub mod notetaker;
pub mod paths;
pub mod pipeline;
pub mod status;
pub mod store;
pub mod text;

pub use config::Config;
pub use dictionary::Dictionary;
pub use error::{Error, Result};
pub use paths::Paths;
pub use status::{AppState, ModelStatus};
pub use store::Store;

/// The version every binary reports, and what `Pong` carries.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
