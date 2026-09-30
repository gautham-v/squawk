//! Squawk.app as a library, so examples can build the popover without the
//! menu-bar binary.
//!
//! OWNED BY THE APP AGENT. Module APIs are the contract in SPEC.md
//! ("squawk-app"); bodies are stubs until implemented.
//!
//! Threads:
//! - **main** (gpui + AppKit): status item, popover, pasteboard and synthetic
//!   cmd+V, NSWorkspace reads.
//! - **hotkey tap**: a CGEventTap on its own CFRunLoop; owns the
//!   `squawk_core::hotkey::Machine` (behind a mutex it shares with the
//!   controller) because the swallow decision must be made inside the tap
//!   callback.
//! - **controller**: owns the engine sessions and runs a dictation from
//!   fn-down to paste; everything slow happens here, never on main.
//! - **ipc**: accept loop on the socket; forwards requests to the controller.

pub mod calendar;
pub mod controller;
pub mod frontmost;
pub mod hotkey;
pub mod ipc_server;
pub mod launch_at_login;
pub mod menu_bar_icon;
pub mod paste;
pub mod permissions;
pub mod status_item;
pub mod ui;
