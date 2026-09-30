//! Squawk.app as a library, so the examples can build the popover and the
//! menu bar item without the full app. See SPEC.md, "squawk-app".
//!
//! Threads:
//! - **main** (gpui + AppKit): status item and popover.
//! - **hotkey tap**: a CGEventTap on its own CFRunLoop; owns the
//!   `squawk_core::hotkey::Machine` (behind a mutex it shares with the
//!   controller) because the swallow decision must be made inside the tap
//!   callback.
//! - **controller**: owns the engine sessions and runs a dictation from
//!   fn-down to paste (front app, Claude Code context, pasteboard and the
//!   synthetic cmd+V included); everything slow happens here, never on main.
//! - **ipc**: accept loop on the socket; forwards requests to the controller.

pub mod calendar;
pub mod controller;
pub mod frontmost;
pub mod hotkey;
pub mod ipc_server;
pub mod launch_at_login;
pub mod logger;
pub mod menu_bar_icon;
pub mod paste;
pub mod permissions;
pub mod status_item;
pub mod ui;
