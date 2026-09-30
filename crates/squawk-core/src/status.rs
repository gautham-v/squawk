//! Shared state types: what the app is doing, how the model is, which
//! permissions are granted. The app owns the values; the engine reports model
//! status; the CLI prints them after asking over the socket.

use serde::{Deserialize, Serialize};

/// What the dictation side of the app is doing right now. A meeting runs
/// alongside this and is reported separately ([`MeetingInfo`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AppState {
    /// Ready for fn.
    Idle,
    /// The mic is open. `hands_free` once a double tap locked it on.
    /// `elapsed_ms` is measured when the status is taken.
    Recording { hands_free: bool, elapsed_ms: u64 },
    /// fn released; the tail is being transcribed and pasted.
    Transcribing,
    /// Cannot dictate yet; `reason` says why ("Downloading model 42%",
    /// "Needs Accessibility", ...).
    NotReady { reason: String },
}

/// Where the speech model is. Reported by the engine, shown by the popover
/// header and `squawk status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ModelStatus {
    /// Not on disk.
    Missing,
    /// Downloading. `total` is `None` when the server sent no length.
    Downloading {
        downloaded: u64,
        total: Option<u64>,
    },
    /// Download done, unpacking the tarball.
    Extracting,
    /// On disk, being loaded into ONNX Runtime.
    Loading,
    Ready,
    Failed {
        message: String,
    },
}

impl ModelStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, ModelStatus::Ready)
    }

    /// 0.0..=1.0 while downloading with a known length.
    pub fn progress(&self) -> Option<f32> {
        match self {
            ModelStatus::Downloading {
                downloaded,
                total: Some(total),
            } if *total > 0 => Some((*downloaded as f64 / *total as f64).clamp(0.0, 1.0) as f32),
            _ => None,
        }
    }

    /// One short line for the popover header and `squawk status`.
    pub fn label(&self) -> String {
        match self {
            ModelStatus::Missing => "Model not downloaded".into(),
            ModelStatus::Downloading { downloaded, total } => match self.progress() {
                Some(p) => format!("Downloading model {:.0}%", p * 100.0),
                None => {
                    let _ = total;
                    format!("Downloading model {} MB", downloaded / 1_000_000)
                }
            },
            ModelStatus::Extracting => "Unpacking model".into(),
            ModelStatus::Loading => "Loading model".into(),
            ModelStatus::Ready => "Model ready".into(),
            ModelStatus::Failed { message } => format!("Model failed: {message}"),
        }
    }
}

/// The three TCC grants squawk needs. `None` is "could not tell".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Permissions {
    /// Event tap + synthetic cmd+V. Required.
    pub accessibility: Option<bool>,
    /// The mic. Required.
    pub microphone: Option<bool>,
    /// System audio for meetings ("Screen & System Audio Recording"). Only
    /// needed for meetings.
    pub screen_recording: Option<bool>,
}

impl Permissions {
    /// Everything dictation needs is granted (or unknown, which we do not
    /// block on).
    pub fn can_dictate(&self) -> bool {
        self.accessibility != Some(false) && self.microphone != Some(false)
    }
}

/// A meeting in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingInfo {
    pub title: String,
    /// The markdown file being written.
    pub path: String,
    /// RFC 3339, local offset.
    pub started_at: String,
    pub elapsed_secs: u64,
}

/// Everything `squawk status` prints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusInfo {
    pub version: String,
    pub state: AppState,
    pub model: ModelStatus,
    pub permissions: Permissions,
    pub meeting: Option<MeetingInfo>,
    /// A config parse note, if the config file is malformed.
    pub config_note: Option<String>,
    /// Dictations pasted since the app started.
    pub dictations_this_run: u64,
}

/// `m:ss` under an hour, `h:mm:ss` from then on: the menu bar timer.
pub fn format_elapsed(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `HH:MM:SS` always: meeting timestamps and lengths.
pub fn format_hms(secs: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_formats() {
        assert_eq!(format_elapsed(0), "0:00");
        assert_eq!(format_elapsed(7), "0:07");
        assert_eq!(format_elapsed(724), "12:04");
        assert_eq!(format_elapsed(3725), "1:02:05");
    }

    #[test]
    fn hms_formats() {
        assert_eq!(format_hms(0), "00:00:00");
        assert_eq!(format_hms(1386), "00:23:06");
        assert_eq!(format_hms(36000), "10:00:00");
    }

    #[test]
    fn model_progress_and_label() {
        let s = ModelStatus::Downloading {
            downloaded: 42,
            total: Some(100),
        };
        assert_eq!(s.progress(), Some(0.42));
        assert_eq!(s.label(), "Downloading model 42%");
        let s = ModelStatus::Downloading {
            downloaded: 5_000_000,
            total: None,
        };
        assert_eq!(s.progress(), None);
        assert_eq!(s.label(), "Downloading model 5 MB");
    }

    #[test]
    fn state_json_shape() {
        let json = serde_json::to_string(&AppState::Recording {
            hands_free: true,
            elapsed_ms: 7000,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"state":"recording","hands_free":true,"elapsed_ms":7000}"#
        );
        let json = serde_json::to_string(&ModelStatus::Ready).unwrap();
        assert_eq!(json, r#"{"state":"ready"}"#);
    }

    #[test]
    fn unknown_permission_does_not_block() {
        assert!(Permissions::default().can_dictate());
        let p = Permissions {
            microphone: Some(false),
            ..Default::default()
        };
        assert!(!p.can_dictate());
    }
}
