//! The three TCC grants, checked without prompting, and the buttons that
//! fix them.
//!
//! - Accessibility: `AXIsProcessTrusted()` (event tap + synthetic cmd+V).
//!   An event tap also needs Input Monitoring on some setups; the tap failing
//!   to create is the real test, so the header shows "Needs Accessibility"
//!   if either fails.
//! - Microphone: `AVCaptureDevice authorizationStatusForMediaType:
//!   AVMediaTypeAudio`; `NotDetermined` → request on first dictation.
//! - Screen & System Audio Recording: `CGPreflightScreenCaptureAccess()`;
//!   only checked when starting a meeting with system audio.

use squawk_core::status::Permissions;

/// A System Settings pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Accessibility,
    InputMonitoring,
    Microphone,
    ScreenRecording,
    /// Keyboard, for "Press 🌐 key to: Do nothing".
    Keyboard,
}

impl Pane {
    /// The `x-apple.systempreferences:` URL that opens it.
    pub fn url(self) -> &'static str {
        match self {
            Pane::Accessibility => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            }
            Pane::InputMonitoring => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
            }
            Pane::Microphone => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
            }
            Pane::ScreenRecording => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            }
            Pane::Keyboard => "x-apple.systempreferences:com.apple.Keyboard-Settings.extension",
        }
    }
}

/// Check all three without prompting.
pub fn check() -> Permissions {
    todo!("app agent")
}

/// Show the system Accessibility prompt (AXIsProcessTrustedWithOptions with
/// kAXTrustedCheckOptionPrompt). Once per launch at most.
pub fn prompt_accessibility() {
    todo!("app agent")
}

/// Ask for the mic; `done(granted)` runs on an arbitrary thread.
pub fn request_microphone(done: impl FnOnce(bool) + Send + 'static) {
    let _ = done;
    todo!("app agent")
}

/// Open a pane with `/usr/bin/open <url>`.
pub fn open_pane(pane: Pane) {
    let _ = std::process::Command::new("/usr/bin/open")
        .arg(pane.url())
        .spawn();
}
