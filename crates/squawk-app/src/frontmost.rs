//! The frontmost app, from `NSWorkspace.sharedWorkspace.frontmostApplication`.
//!
//! Read on the controller thread: NSWorkspace and NSRunningApplication are
//! thread-safe for these reads, and the value is kept current by the main
//! thread's run loop (which gpui is always running).

use objc2::rc::autoreleasepool;
use objc2_app_kit::NSWorkspace;
use squawk_core::context::FrontApp;

pub fn front_app() -> Option<FrontApp> {
    autoreleasepool(|_| {
        let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        let bundle_id = app
            .bundleIdentifier()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let name = app.localizedName().map(|s| s.to_string());
        Some(FrontApp {
            name: display_name(name.as_deref(), &bundle_id),
            bundle_id,
            pid: app.processIdentifier(),
        })
    })
}

/// The name written in the day file: the localized name, else the last part
/// of the bundle id ("com.mitchellh.ghostty" → "ghostty"), else "Unknown".
pub fn display_name(localized: Option<&str>, bundle_id: &str) -> String {
    if let Some(name) = localized.map(str::trim).filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    match bundle_id.rsplit('.').next().filter(|s| !s.is_empty()) {
        Some(tail) => tail.to_string(),
        None => "Unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_fall_back_to_the_bundle_id_tail() {
        assert_eq!(
            display_name(Some("Ghostty"), "com.mitchellh.ghostty"),
            "Ghostty"
        );
        assert_eq!(display_name(Some("  "), "com.mitchellh.ghostty"), "ghostty");
        assert_eq!(display_name(None, ""), "Unknown");
    }

    #[test]
    fn reading_the_front_app_never_panics() {
        // Headless test runs may have no front app at all.
        let _ = front_app();
    }
}
