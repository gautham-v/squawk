//! The frontmost app, from `NSWorkspace.sharedWorkspace.frontmostApplication`.
//! Safe off the main thread (NSWorkspace and NSRunningApplication are
//! thread-safe for these reads).

use squawk_core::context::FrontApp;

pub fn front_app() -> Option<FrontApp> {
    todo!("app agent")
}
