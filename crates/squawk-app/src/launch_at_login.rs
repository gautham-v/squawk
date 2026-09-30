//! "Launch at login", via `SMAppService` (macOS 13+).
//!
//! `SMAppService.mainApp` registers *the running bundle's path* as a login
//! item, which has two consequences worth knowing: it only works when squawk
//! is running from a `.app` (a bare `cargo run` binary has no bundle, so the
//! toggle reports [`Availability::NoBundle`] and stays disabled), and moving
//! the `.app` afterwards leaves a login item pointing at the old path — toggle
//! it off and on again after a move.
//!
//! Asking `SMAppService` for its status is a synchronous round trip to the
//! background task daemon: 80–300 ms, measured. The popover renders on every
//! scroll event and hover change, so the status is cached here: read once
//! (off the main thread, via [`refresh`]) and updated by [`set_enabled`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2_foundation::{NSBundle, NSError};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

/// Whether the toggle can do anything at all in this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// Running from a `.app`; the toggle works.
    Ready,
    /// Running as a bare binary (`cargo run`, an example): there is no bundle
    /// to register, so the toggle is shown disabled with a note.
    NoBundle,
}

/// Whether this process has a bundle identifier, which is what `SMAppService`
/// needs. `cargo run` and the examples do not.
pub fn availability() -> Availability {
    static AVAILABILITY: OnceLock<Availability> = OnceLock::new();
    *AVAILABILITY.get_or_init(|| {
        let has_identifier = NSBundle::mainBundle()
            .bundleIdentifier()
            .is_some_and(|id| !id.to_string().is_empty());
        if has_identifier {
            Availability::Ready
        } else {
            Availability::NoBundle
        }
    })
}

/// The note shown beside a disabled toggle.
pub const NO_BUNDLE_NOTE: &str = "Only from the app bundle";

/// The status as [`refresh`] last read it.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether the app is registered to start at login, as last read by
/// [`refresh`] (off until then). Cheap: the popover calls it while
/// rendering. The app refreshes it in the background at startup and each
/// time the popover opens.
///
/// `RequiresApproval` counts as off: the registration exists but the user has
/// switched it off in System Settings, and the checkmark should agree with what
/// System Settings shows.
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Ask `SMAppService` again (slow: call it off the main thread) and cache
/// the answer. The user can flip the switch in System Settings, so the
/// popover refreshes this each time it opens.
pub fn refresh() -> bool {
    let enabled = availability() == Availability::Ready && {
        let service = unsafe { SMAppService::mainAppService() };
        unsafe { service.status() == SMAppServiceStatus::Enabled }
    };
    ENABLED.store(enabled, Ordering::Relaxed);
    enabled
}

/// Turn launch-at-login on or off (slow, like [`refresh`]: call it off the
/// main thread). The error is already phrased for display.
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if availability() != Availability::Ready {
        return Err(NO_BUNDLE_NOTE.to_string());
    }
    let service = unsafe { SMAppService::mainAppService() };
    let result = if enabled {
        unsafe { service.registerAndReturnError() }
    } else {
        unsafe { service.unregisterAndReturnError() }
    };
    let result = result.map_err(|error| describe(&error, enabled));
    refresh();
    result
}

fn describe(error: &Retained<NSError>, enabling: bool) -> String {
    let verb = if enabling { "enable" } else { "disable" };
    format!("Could not {verb}: {}", error.localizedDescription())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_test_binary_has_no_bundle_to_register() {
        // `cargo test` runs without a bundle identifier, so the toggle is off
        // and inert rather than reporting a spurious "enabled".
        assert_eq!(availability(), Availability::NoBundle);
        assert!(!is_enabled());
        assert_eq!(set_enabled(true), Err(NO_BUNDLE_NOTE.to_string()));
    }
}
