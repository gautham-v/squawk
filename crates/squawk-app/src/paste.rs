//! Putting text into the focused app.
//!
//! Save every item on the general pasteboard (all types, as bytes), write the
//! text as a plain string, post cmd+V (keyDown/keyUp for keycode 9 with only
//! the command flag set, so a still-held ctrl or fn does not turn it into
//! something else), then restore the saved items `restore_after` later —
//! unless the pasteboard's changeCount moved in between (the user copied
//! something), in which case theirs stays.
//!
//! Runs on the controller thread rather than hopping to main: NSPasteboard
//! and CGEventPost are thread-safe, and the hop would put the main thread's
//! rendering between fn-up and the paste. A second paste inside the restore
//! window reuses the first one's saved items, so rapid dictations never leave
//! squawk's own text behind as "the user's clipboard".
//!
//! Never appends a newline: squawk never submits a prompt. The caller adds
//! the optional trailing space.

use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
};
use objc2_foundation::{NSArray, NSData, NSString};

/// kVK_ANSI_V.
const KEY_V: u16 = 9;

/// Marks our write as short-lived, so clipboard managers that honour
/// nspasteboard.org's conventions do not record every dictation.
const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";

/// One pasteboard item: every type it carried, with its bytes.
type SavedItem = Vec<(String, Vec<u8>)>;

struct Pending {
    saved: Vec<SavedItem>,
    /// The changeCount right after our write; anything else means the user
    /// (or another app) put something new there.
    ours: isize,
    generation: u64,
}

/// The clipboard as it was when a dictation started, read while the user
/// talks so the paste itself does not wait on it.
struct Prepared {
    count: isize,
    saved: Vec<SavedItem>,
}

struct RestoreState {
    pending: Option<Pending>,
    prepared: Option<Prepared>,
    generation: u64,
}

static RESTORE: Mutex<RestoreState> = Mutex::new(RestoreState {
    pending: None,
    prepared: None,
    generation: 0,
});

fn restore_state() -> MutexGuard<'static, RestoreState> {
    RESTORE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Read the clipboard now, ahead of a paste that is probably coming (called
/// when a dictation starts). Reading every type of a large clipboard — an
/// image, a promised file — can take a while, and that time would otherwise
/// sit between fn-up and the paste.
pub fn prepare() {
    let mut state = restore_state();
    if state.pending.is_some() {
        // A restore is due; the paste will reuse what it saved.
        return;
    }
    autoreleasepool(|_| {
        let board = NSPasteboard::generalPasteboard();
        state.prepared = Some(Prepared {
            count: board.changeCount(),
            saved: save_items(&board),
        });
    });
}

/// Drop what [`prepare`] read (the dictation was cancelled); a large
/// clipboard should not sit in memory until the next paste.
pub fn discard_prepared() {
    restore_state().prepared = None;
}

/// Paste `text` into whatever has keyboard focus.
pub fn paste(text: &str, restore_after: Duration) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    let mut state = restore_state();
    let pending = state.pending.take();
    let prepared = state.prepared.take();
    let (saved, ours) = autoreleasepool(|_| {
        let board = NSPasteboard::generalPasteboard();
        let saved = choose_saved(board.changeCount(), pending, prepared)
            .unwrap_or_else(|| save_items(&board));
        write_text(&board, text, true)?;
        Ok::<_, String>((saved, board.changeCount()))
    })?;
    post_command_v()?;

    state.generation += 1;
    let generation = state.generation;
    state.pending = Some(Pending {
        saved,
        ours,
        generation,
    });
    drop(state);

    thread::Builder::new()
        .name("squawk-paste-restore".into())
        .spawn(move || {
            thread::sleep(restore_after);
            restore_if_ours(generation);
        })
        .map_err(|e| format!("could not schedule the clipboard restore: {e}"))?;
    Ok(())
}

/// Which saved clipboard a paste should restore later, given the
/// pasteboard's changeCount now: the one an earlier paste is still holding
/// (if nothing was copied since that paste), else the one read when the
/// dictation started (if nothing was copied since), else `None` — read it
/// now.
fn choose_saved(
    count: isize,
    pending: Option<Pending>,
    prepared: Option<Prepared>,
) -> Option<Vec<SavedItem>> {
    match (pending, prepared) {
        (Some(p), _) if p.ours == count => Some(p.saved),
        (_, Some(p)) if p.count == count => Some(p.saved),
        _ => None,
    }
}

/// Copy to the general pasteboard and leave it there (ctrl+cmd+C, a click
/// on a History row). Cancels a pending restore: this is what the user wants
/// on the clipboard now.
pub fn copy(text: &str) {
    let mut state = restore_state();
    state.pending = None;
    state.prepared = None;
    autoreleasepool(|_| {
        let board = NSPasteboard::generalPasteboard();
        if let Err(e) = write_text(&board, text, false) {
            log::warn!("copy: {e}");
        }
    });
}

fn restore_if_ours(generation: u64) {
    let mut state = restore_state();
    let is_latest = state
        .pending
        .as_ref()
        .is_some_and(|p| p.generation == generation);
    if !is_latest {
        return;
    }
    let Some(pending) = state.pending.take() else {
        return;
    };
    autoreleasepool(|_| {
        let board = NSPasteboard::generalPasteboard();
        if board.changeCount() != pending.ours {
            return;
        }
        restore_items(&board, &pending.saved);
    });
}

fn save_items(board: &NSPasteboard) -> Vec<SavedItem> {
    let Some(items) = board.pasteboardItems() else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            item.types()
                .iter()
                .filter_map(|ty| {
                    let data = item.dataForType(&ty)?;
                    Some((ty.to_string(), data.to_vec()))
                })
                .collect()
        })
        .collect()
}

fn restore_items(board: &NSPasteboard, saved: &[SavedItem]) {
    board.clearContents();
    if saved.is_empty() {
        return;
    }
    let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = saved
        .iter()
        .map(|types| {
            let item = NSPasteboardItem::new();
            for (ty, bytes) in types {
                item.setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(ty));
            }
            ProtocolObject::from_retained(item)
        })
        .collect();
    let array = NSArray::from_retained_slice(&items);
    if !board.writeObjects(&array) {
        log::warn!("could not restore the clipboard");
    }
}

fn write_text(board: &NSPasteboard, text: &str, transient: bool) -> Result<(), String> {
    board.clearContents();
    let item = NSPasteboardItem::new();
    // SAFETY: reading an immutable AppKit constant.
    let string_type = unsafe { NSPasteboardTypeString };
    item.setString_forType(&NSString::from_str(text), string_type);
    if transient {
        item.setData_forType(&NSData::new(), &NSString::from_str(TRANSIENT_TYPE));
    }
    let array = NSArray::from_retained_slice(&[ProtocolObject::from_retained(item)]);
    if board.writeObjects(&array) {
        Ok(())
    } else {
        Err("the clipboard refused the text".into())
    }
}

fn post_command_v() -> Result<(), String> {
    // A private source: its own modifier state, so the user's held keys do
    // not leak into the synthetic event.
    let source = CGEventSource::new(CGEventSourceStateID::Private);
    for down in [true, false] {
        let event = CGEvent::new_keyboard_event(source.as_deref(), KEY_V, down)
            .ok_or("could not create the cmd+V event")?;
        CGEvent::set_flags(Some(&event), CGEventFlags::MaskCommand);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(tag: &str) -> Vec<SavedItem> {
        vec![vec![(
            "public.utf8-plain-text".into(),
            tag.as_bytes().to_vec(),
        )]]
    }

    fn pending(tag: &str, ours: isize) -> Option<Pending> {
        Some(Pending {
            saved: saved(tag),
            ours,
            generation: 1,
        })
    }

    fn prepared(tag: &str, count: isize) -> Option<Prepared> {
        Some(Prepared {
            count,
            saved: saved(tag),
        })
    }

    /// Two dictations inside one restore window: the second must restore
    /// the user's clipboard, not the first dictation's text.
    #[test]
    fn a_quick_second_paste_keeps_the_users_clipboard() {
        assert_eq!(
            choose_saved(7, pending("user", 7), prepared("ours", 7)),
            Some(saved("user"))
        );
    }

    /// The user copied something after the last paste: that is the
    /// clipboard to keep, so read it fresh.
    #[test]
    fn a_copy_after_the_last_paste_wins() {
        assert_eq!(choose_saved(8, pending("user", 7), None), None);
        assert_eq!(
            choose_saved(8, pending("user", 7), prepared("new", 8)),
            Some(saved("new"))
        );
    }

    #[test]
    fn the_clipboard_read_at_start_is_used_when_unchanged() {
        assert_eq!(
            choose_saved(3, None, prepared("start", 3)),
            Some(saved("start"))
        );
        // Copied something mid-dictation: read again.
        assert_eq!(choose_saved(4, None, prepared("start", 3)), None);
        assert_eq!(choose_saved(4, None, None), None);
    }
}
