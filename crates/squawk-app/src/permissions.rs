//! The three TCC grants, checked without prompting, and the buttons that
//! fix them.
//!
//! - Accessibility: `AXIsProcessTrusted()` (event tap + synthetic cmd+V).
//!   An event tap also needs Input Monitoring on some setups; the tap failing
//!   to create is the real test, so the controller treats a refused tap as
//!   "Needs Accessibility" whatever this says.
//! - Microphone: `AVCaptureDevice authorizationStatusForMediaType:
//!   AVMediaTypeAudio`; `NotDetermined` → requested on the first dictation.
//! - Screen & System Audio Recording: `CGPreflightScreenCaptureAccess()`;
//!   only checked when a meeting with system audio starts.

use std::ffi::c_void;

use block2::RcBlock;
use objc2::runtime::{AnyObject, Bool};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use squawk_core::status::Permissions;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const c_void;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// A System Settings pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Accessibility,
    InputMonitoring,
    Microphone,
    ScreenRecording,
    /// Calendars, for the notetaker's heads-up.
    Calendars,
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
            Pane::Calendars => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Calendars"
            }
            Pane::Keyboard => "x-apple.systempreferences:com.apple.Keyboard-Settings.extension",
        }
    }
}

/// Accessibility and Microphone, without prompting. Screen recording is left
/// unknown: it only matters for meetings, and is checked then.
pub fn check() -> Permissions {
    Permissions {
        accessibility: Some(accessibility()),
        microphone: microphone(),
        screen_recording: None,
    }
}

pub fn accessibility() -> bool {
    // SAFETY: no arguments; reads this process's trust.
    unsafe { AXIsProcessTrusted() }
}

/// `Some(granted)` once decided, `None` while macOS has not asked yet.
pub fn microphone() -> Option<bool> {
    // SAFETY: AVMediaTypeAudio is an immutable framework constant.
    let audio = (unsafe { AVMediaTypeAudio })?;
    // SAFETY: a valid media type.
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) };
    mic_state(status)
}

fn mic_state(status: AVAuthorizationStatus) -> Option<bool> {
    match status {
        AVAuthorizationStatus::Authorized => Some(true),
        AVAuthorizationStatus::Denied | AVAuthorizationStatus::Restricted => Some(false),
        _ => None,
    }
}

/// Show the system Accessibility prompt (AXIsProcessTrustedWithOptions with
/// kAXTrustedCheckOptionPrompt). The caller does this once per launch.
pub fn prompt_accessibility() {
    // SAFETY: kAXTrustedCheckOptionPrompt is a CFStringRef, toll-free bridged
    // to NSString; the dictionary lives across the call.
    unsafe {
        let key = &*(kAXTrustedCheckOptionPrompt as *const NSString);
        let yes = NSNumber::new_bool(true);
        let options = NSDictionary::<NSString, AnyObject>::from_slices(&[key], &[&*yes]);
        let options_ptr = &*options as *const NSDictionary<NSString, AnyObject> as *const c_void;
        AXIsProcessTrustedWithOptions(options_ptr);
    }
}

/// Ask for the mic; `done(granted)` runs on an arbitrary thread.
pub fn request_microphone(done: impl FnOnce(bool) + Send + 'static) {
    // SAFETY: AVMediaTypeAudio is an immutable framework constant.
    let Some(audio) = (unsafe { AVMediaTypeAudio }) else {
        done(false);
        return;
    };
    let done = std::sync::Mutex::new(Some(done));
    let block = RcBlock::new(move |granted: Bool| {
        if let Some(done) = done.lock().ok().and_then(|mut d| d.take()) {
            done(granted.as_bool());
        }
    });
    // SAFETY: a valid media type and a block that is `Fn(Bool)`.
    unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &block) };
}

/// Whether Screen & System Audio Recording is granted. Never prompts.
pub fn screen_recording() -> bool {
    // SAFETY: no arguments.
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Ask macOS for Screen & System Audio Recording. On Sequoia this shows the
/// system prompt the first time and does nothing after that.
pub fn request_screen_recording() -> bool {
    // SAFETY: no arguments.
    unsafe { CGRequestScreenCaptureAccess() }
}

/// Open a pane with `/usr/bin/open <url>`.
pub fn open_pane(pane: Pane) {
    let _ = std::process::Command::new("/usr/bin/open")
        .arg(pane.url())
        .spawn();
}

/// What the user's fn/globe key does on its own, from
/// `com.apple.HIToolbox AppleFnUsageType`: 0 = Do nothing, 1 = Change input
/// source, 2 = Show Emoji & Symbols, 3 = Start Dictation. Anything but 0
/// fights squawk (the emoji picker opens on every tap).
pub fn fn_key_does_nothing() -> bool {
    std::process::Command::new("/usr/bin/defaults")
        .args(["read", "com.apple.HIToolbox", "AppleFnUsageType"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| fn_usage_is_nothing(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(false)
}

fn fn_usage_is_nothing(value: &str) -> bool {
    value.trim() == "0"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mic_states_map_to_the_tri_state() {
        assert_eq!(mic_state(AVAuthorizationStatus::Authorized), Some(true));
        assert_eq!(mic_state(AVAuthorizationStatus::Denied), Some(false));
        assert_eq!(mic_state(AVAuthorizationStatus::Restricted), Some(false));
        assert_eq!(mic_state(AVAuthorizationStatus::NotDetermined), None);
    }

    #[test]
    fn only_zero_means_do_nothing() {
        assert!(fn_usage_is_nothing("0\n"));
        assert!(!fn_usage_is_nothing("2\n"));
        assert!(!fn_usage_is_nothing(""));
    }

    #[test]
    fn every_pane_is_a_settings_url() {
        for pane in [
            Pane::Accessibility,
            Pane::InputMonitoring,
            Pane::Microphone,
            Pane::ScreenRecording,
            Pane::Calendars,
            Pane::Keyboard,
        ] {
            assert!(pane.url().starts_with("x-apple.systempreferences:"));
        }
    }

    #[test]
    fn checking_never_prompts_or_panics() {
        let p = check();
        assert!(p.accessibility.is_some());
        assert!(p.screen_recording.is_none());
    }
}
