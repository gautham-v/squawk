//! `~/.config/squawk/config.toml`.
//!
//! Every key has a default, so a missing file, an empty file and a partial
//! file all work, and unknown keys are ignored. Values that would break the
//! app are clamped on load rather than rejected: a bad setting should never
//! stop dictation from working.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Where the default model comes from: Handy's mirror of the int8 ONNX export
/// of NVIDIA Parakeet TDT 0.6B v3. One tar.gz holding one directory.
pub const DEFAULT_MODEL_URL: &str = "https://blob.handy.computer/parakeet-v3-int8.tar.gz";
/// The directory the tarball unpacks to, under `Paths::models_dir`.
pub const DEFAULT_MODEL_DIR: &str = "parakeet-tdt-0.6b-v3-int8";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where dictations, meetings and the dictionary live. `~` expands.
    /// `None` means `~/squawk`.
    pub data_dir: Option<String>,
    /// Keep a WAV of every dictation and meeting under the support dir.
    pub keep_audio: bool,
    pub hotkey: HotkeyConfig,
    pub dictation: DictationConfig,
    pub meeting: MeetingConfig,
    pub model: ModelConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    /// A fn press released sooner than this is a tap, not push-to-talk.
    pub tap_max_ms: u64,
    /// How long after a tap's release a second press still counts as a
    /// double tap.
    pub double_tap_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DictationConfig {
    /// Drop um/uh and comma-delimited filler phrases.
    pub remove_fillers: bool,
    /// Resolve file names to @mentions and fix repo jargon when the front
    /// app is a terminal running claude or codex.
    pub claude_code_mode: bool,
    /// Add one space after the pasted text, so two dictations in a row do
    /// not run together. Never a newline: squawk never submits.
    pub trailing_space: bool,
    /// How long after the synthetic cmd+V the old clipboard is put back.
    pub paste_restore_ms: u64,
    /// Input device name as CoreAudio reports it; empty is the system default.
    pub input_device: String,
    /// A dictation longer than this is stopped and pasted, in case a fn
    /// release was missed.
    pub max_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MeetingConfig {
    /// Transcribe each track in chunks of about this many seconds.
    pub chunk_secs: u64,
    /// Record the other side (system audio) as "Them". Off records the mic only.
    pub system_audio: bool,
    /// Take the title from the calendar event happening now.
    pub calendar_titles: bool,
    /// Keep the other side out of "You" when the call plays on speakers:
    /// the mic goes through Apple's voice processing (echo cancellation),
    /// and a "You" line that repeats what "Them" said at the same moment is
    /// dropped. Off records the mic as it is (fine with headphones).
    pub echo_cancellation: bool,
    /// Offer to record this many seconds before a calendar event with other
    /// people or a call link starts. 0 = at the start, negative = off.
    pub heads_up_secs: i64,
    /// Offer to take notes when a call app starts using the mic.
    pub detect_calls: bool,
    /// Stop and save a meeting after this many minutes, with a warning two
    /// minutes before.
    pub max_minutes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    /// Directory name under the models dir.
    pub dir: String,
    /// Where to download it from when it is missing.
    pub url: String,
    /// ONNX Runtime intra-op threads; 0 lets ORT decide.
    pub threads: usize,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        HotkeyConfig {
            tap_max_ms: crate::hotkey::DEFAULT_TAP_MAX_MS,
            double_tap_ms: crate::hotkey::DEFAULT_DOUBLE_TAP_MS,
        }
    }
}

impl Default for DictationConfig {
    fn default() -> Self {
        DictationConfig {
            remove_fillers: true,
            claude_code_mode: true,
            trailing_space: true,
            paste_restore_ms: 300,
            input_device: String::new(),
            max_secs: 600,
        }
    }
}

impl Default for MeetingConfig {
    fn default() -> Self {
        MeetingConfig {
            chunk_secs: 30,
            system_audio: true,
            calendar_titles: true,
            echo_cancellation: true,
            heads_up_secs: 15,
            detect_calls: true,
            max_minutes: 120,
        }
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        ModelConfig {
            dir: DEFAULT_MODEL_DIR.to_string(),
            url: DEFAULT_MODEL_URL.to_string(),
            threads: 0,
        }
    }
}

impl Config {
    /// Read the config. A missing file is the defaults with no note; a file
    /// that does not parse is the defaults plus a human-readable note the
    /// popover and `squawk status` show.
    pub fn load(path: &Path) -> (Config, Option<String>) {
        match Config::try_load(path) {
            Ok(config) => (config, None),
            Err(err) => (Config::default(), Some(err.to_string())),
        }
    }

    /// Like [`Config::load`] but a parse error is an error.
    pub fn try_load(path: &Path) -> Result<Config> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(e.into()),
        };
        Config::parse(&text).map_err(|message| Error::Config {
            path: path.to_path_buf(),
            message,
        })
    }

    /// Parse TOML text and clamp it. The error is the TOML parser's message.
    pub fn parse(text: &str) -> std::result::Result<Config, String> {
        toml::from_str::<Config>(text)
            .map(Config::clamped)
            .map_err(|e| e.message().to_string())
    }

    /// Write the config, creating its directory. Used to drop a commented
    /// default file on first run so "Settings" has something to open.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = toml::to_string_pretty(self).map_err(|e| Error::Config {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        std::fs::write(path, body)?;
        Ok(())
    }

    /// Write [`DEFAULT_CONFIG_TOML`] if there is no config yet. Returns
    /// whether it wrote one.
    pub fn write_default_if_missing(path: &Path) -> Result<bool> {
        if path.exists() {
            return Ok(false);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, DEFAULT_CONFIG_TOML)?;
        Ok(true)
    }

    /// Pull out-of-range values back to something that works.
    pub fn clamped(mut self) -> Config {
        let h = &mut self.hotkey;
        h.tap_max_ms = h.tap_max_ms.clamp(100, 1000);
        h.double_tap_ms = h.double_tap_ms.clamp(150, 1000);
        let d = &mut self.dictation;
        d.paste_restore_ms = d.paste_restore_ms.clamp(50, 5000);
        d.max_secs = d.max_secs.clamp(10, 3600);
        let m = &mut self.meeting;
        m.chunk_secs = m.chunk_secs.clamp(5, 300);
        m.heads_up_secs = if m.heads_up_secs < 0 {
            -1
        } else {
            m.heads_up_secs.min(3600)
        };
        m.max_minutes = m.max_minutes.clamp(5, 24 * 60);
        if self.model.dir.trim().is_empty() {
            self.model.dir = DEFAULT_MODEL_DIR.to_string();
        }
        if self.model.url.trim().is_empty() {
            self.model.url = DEFAULT_MODEL_URL.to_string();
        }
        self
    }
}

/// The file written on first run: every key at its default, commented so the
/// user can see what is there without the file overriding future defaults.
pub const DEFAULT_CONFIG_TOML: &str = r#"# squawk settings. Every key is optional; these are the defaults.
# The app reads this at launch, when you open its menu after an edit, and on `squawk reload`.

# Where dictations, meetings and dictionary.txt live.
# data_dir = "~/squawk"

# Keep a WAV of each dictation and meeting (under ~/Library/Application Support/squawk/audio).
# keep_audio = false

[hotkey]
# A fn press shorter than this is a tap (two taps = hands-free, one = nothing).
# tap_max_ms = 300
# How long after the first tap the second one may come.
# double_tap_ms = 400

[dictation]
# remove_fillers = true
# claude_code_mode = true
# trailing_space = true
# paste_restore_ms = 300
# input_device = ""
# max_secs = 600

[meeting]
# chunk_secs = 30
# system_audio = true
# calendar_titles = true
# Take the other side out of your mic when the call plays on speakers. Other audio is
# ducked a little while a meeting records; with headphones you can turn this off.
# echo_cancellation = true
# The rest is what the popover's Settings tab changes.
# Offer to record this many seconds before a calendar event with other people or a
# call link starts: 0 = at the start, -1 = off.
# heads_up_secs = 15
# Offer to take notes when Zoom, Teams, FaceTime, Slack, Webex, Discord or a Meet tab
# starts using the mic.
# detect_calls = true
# Stop and save after this many minutes (warns 2 min before).
# max_minutes = 120

[model]
# dir = "parakeet-tdt-0.6b-v3-int8"
# url = "https://blob.handy.computer/parakeet-v3-int8.tar.gz"
# threads = 0
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn default_file_parses_to_defaults() {
        assert_eq!(
            Config::parse(DEFAULT_CONFIG_TOML).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let c = Config::parse("keep_audio = true\n[dictation]\nremove_fillers = false\n").unwrap();
        assert!(c.keep_audio);
        assert!(!c.dictation.remove_fillers);
        assert!(c.dictation.claude_code_mode);
        assert_eq!(c.meeting.chunk_secs, 30);
        assert_eq!(c.model.url, DEFAULT_MODEL_URL);
    }

    #[test]
    fn echo_cancellation_is_on_unless_turned_off() {
        assert!(Config::default().meeting.echo_cancellation);
        let c = Config::parse("[meeting]\necho_cancellation = false\n").unwrap();
        assert!(!c.meeting.echo_cancellation);
        assert!(c.meeting.system_audio);
    }

    #[test]
    fn notetaker_defaults_and_clamps() {
        let m = Config::default().meeting;
        assert_eq!(
            (m.heads_up_secs, m.detect_calls, m.max_minutes),
            (15, true, 120)
        );
        let c = Config::parse("[meeting]\nheads_up_secs = -40\nmax_minutes = 0\n").unwrap();
        assert_eq!(c.meeting.heads_up_secs, -1);
        assert_eq!(c.meeting.max_minutes, 5);
        let c = Config::parse("[meeting]\nheads_up_secs = 99999\nmax_minutes = 99999\n").unwrap();
        assert_eq!(c.meeting.heads_up_secs, 3600);
        assert_eq!(c.meeting.max_minutes, 1440);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let c = Config::parse("future_key = 1\n[hotkey]\nwhatever = \"x\"\n").unwrap();
        assert_eq!(c, Config::default());
    }

    /// Files written while "Stop when the call ends" existed still load,
    /// quietly.
    #[test]
    fn the_retired_stop_when_call_ends_key_is_ignored() {
        let text = "[meeting]\nstop_when_call_ends = false\n";
        assert_eq!(Config::parse(text).unwrap(), Config::default());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        let (config, note) = Config::load(&path);
        assert_eq!(config, Config::default());
        assert_eq!(note, None);
    }

    #[test]
    fn bad_values_are_clamped() {
        let c = Config::parse(
            "[hotkey]\ntap_max_ms = 5\ndouble_tap_ms = 99999\n[meeting]\nchunk_secs = 0\n[model]\ndir = \"\"\n",
        )
        .unwrap();
        assert_eq!(c.hotkey.tap_max_ms, 100);
        assert_eq!(c.hotkey.double_tap_ms, 1000);
        assert_eq!(c.meeting.chunk_secs, 5);
        assert_eq!(c.model.dir, DEFAULT_MODEL_DIR);
    }

    #[test]
    fn malformed_file_loads_defaults_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "keep_audio = maybe").unwrap();
        let (c, note) = Config::load(&path);
        assert_eq!(c, Config::default());
        let note = note.unwrap();
        assert!(note.contains("config.toml"), "{note}");
    }

    #[test]
    fn missing_file_is_defaults_without_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let (c, note) = Config::load(&dir.path().join("nope.toml"));
        assert_eq!(c, Config::default());
        assert!(note.is_none());
    }

    #[test]
    fn save_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.toml");
        let c = Config {
            data_dir: Some("~/elsewhere".into()),
            meeting: MeetingConfig {
                system_audio: false,
                ..MeetingConfig::default()
            },
            ..Config::default()
        };
        c.save(&path).unwrap();
        assert_eq!(Config::try_load(&path).unwrap(), c);
    }

    #[test]
    fn default_file_written_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c/config.toml");
        assert!(Config::write_default_if_missing(&path).unwrap());
        std::fs::write(&path, "keep_audio = true").unwrap();
        assert!(!Config::write_default_if_missing(&path).unwrap());
        assert!(Config::try_load(&path).unwrap().keep_audio);
    }
}
