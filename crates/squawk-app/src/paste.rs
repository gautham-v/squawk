//! Putting text into the focused app.
//!
//! Save every item on the general pasteboard (all types, as data), write the
//! text as a plain string, post cmd+V (CGEvent keyDown/keyUp for keycode 9
//! with the command flag, from a private event source so the user's own held
//! modifiers do not leak in), then restore the saved items `restore_after`
//! later — unless the pasteboard's changeCount moved in between (the user
//! copied something), in which case leave theirs alone.
//!
//! Never appends a newline: squawk never submits a prompt. The caller adds
//! the optional trailing space.
//!
//! Main thread only (NSPasteboard and the restore timer).

use std::time::Duration;

use objc2::MainThreadMarker;

pub fn paste(mtm: MainThreadMarker, text: &str, restore_after: Duration) -> Result<(), String> {
    let _ = (mtm, text, restore_after);
    todo!("app agent")
}

/// Copy to the general pasteboard and leave it there (ctrl+cmd+C, a click
/// on a History row).
pub fn copy(mtm: MainThreadMarker, text: &str) {
    let _ = (mtm, text);
    todo!("app agent")
}
