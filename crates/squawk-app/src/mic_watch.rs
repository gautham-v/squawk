//! Which other processes are using a microphone, from Core Audio's process
//! objects (macOS 14.2+): `kAudioHardwarePropertyProcessObjectList`, and per
//! process `kAudioProcessPropertyIsRunningInput`, `…PID` and `…BundleID`.
//!
//! Thin on purpose: this lists and listens; deciding what counts as a call
//! (known apps, browser tab titles, the 3 s and 10 s debounces) is
//! `squawk_core::notetaker`, tested with synthetic timestamps.
//!
//! [`MicWatcher::start`] runs one thread. Core Audio listener blocks wake it:
//! on the process list, on each process's "running input", and on each
//! device's "running somewhere" (the per-process property does not notify
//! reliably when a process starts its input; the device one does). While a
//! call app or a browser has input running it also re-reads once a second (a
//! tab title can change without the mic changing). `on_change` hears only
//! changes in the set of call apps.

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::autoreleasepool;
use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSString;
use squawk_core::notetaker::calls::{self, MicUser, Source};

type AudioObjectId = u32;
type OsStatus = i32;

#[repr(C)]
#[derive(Clone, Copy)]
struct PropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

const SYSTEM_OBJECT: AudioObjectId = 1;
const SCOPE_GLOBAL: u32 = u32::from_be_bytes(*b"glob");
const ELEMENT_MAIN: u32 = 0;
const PROCESS_OBJECT_LIST: u32 = u32::from_be_bytes(*b"prs#");
const PROCESS_PID: u32 = u32::from_be_bytes(*b"ppid");
const PROCESS_BUNDLE_ID: u32 = u32::from_be_bytes(*b"pbid");
const PROCESS_IS_RUNNING_INPUT: u32 = u32::from_be_bytes(*b"piri");
const HARDWARE_DEVICES: u32 = u32::from_be_bytes(*b"dev#");
const DEVICE_IS_RUNNING_SOMEWHERE: u32 = u32::from_be_bytes(*b"gone");

type ListenerBlock = block2::Block<dyn Fn(u32, *const c_void)>;

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyDataSize(
        object: AudioObjectId,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        out_size: *mut u32,
    ) -> OsStatus;
    fn AudioObjectGetPropertyData(
        object: AudioObjectId,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        io_size: *mut u32,
        out: *mut c_void,
    ) -> OsStatus;
    fn AudioObjectAddPropertyListenerBlock(
        object: AudioObjectId,
        address: *const PropertyAddress,
        queue: *mut c_void,
        listener: &ListenerBlock,
    ) -> OsStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const c_void);
    fn CFGetTypeID(cf: *const c_void) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> *const c_void;
    fn AXUIElementCopyAttributeValue(
        element: *const c_void,
        attribute: *const c_void,
        value: *mut *const c_void,
    ) -> i32;
}

fn address(selector: u32) -> PropertyAddress {
    PropertyAddress {
        selector,
        scope: SCOPE_GLOBAL,
        element: ELEMENT_MAIN,
    }
}

/// The audio process objects Core Audio knows about (every process that has
/// touched audio, running or not). Empty before macOS 14.2.
fn process_objects() -> Vec<AudioObjectId> {
    object_list(PROCESS_OBJECT_LIST)
}

/// Every audio device (inputs among them).
fn devices() -> Vec<AudioObjectId> {
    object_list(HARDWARE_DEVICES)
}

/// An array-of-object-ids property of the system object.
fn object_list(selector: u32) -> Vec<AudioObjectId> {
    let addr = address(selector);
    let mut size = 0u32;
    // SAFETY: valid address, out-pointer to a local.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &addr, 0, std::ptr::null(), &mut size)
    };
    if status != 0 || size == 0 {
        return Vec::new();
    }
    let mut ids = vec![0 as AudioObjectId; size as usize / std::mem::size_of::<AudioObjectId>()];
    // SAFETY: the buffer holds `size` bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            ids.as_mut_ptr().cast(),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    ids.truncate(size as usize / std::mem::size_of::<AudioObjectId>());
    ids
}

fn read_u32(object: AudioObjectId, selector: u32) -> Option<u32> {
    let addr = address(selector);
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: a u32-sized out buffer for a u32 (or pid_t) property.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            (&mut value as *mut u32).cast(),
        )
    };
    (status == 0).then_some(value)
}

fn read_bundle_id(object: AudioObjectId) -> String {
    let addr = address(PROCESS_BUNDLE_ID);
    let mut value: *const c_void = std::ptr::null();
    let mut size = std::mem::size_of::<*const c_void>() as u32;
    // SAFETY: the property is a CFStringRef we own (a copy) on success.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            (&mut value as *mut *const c_void).cast(),
        )
    };
    if status != 0 || value.is_null() {
        return String::new();
    }
    // SAFETY: a CFString, toll-free bridged to NSString; released after.
    unsafe {
        let text = cf_string(value).unwrap_or_default();
        CFRelease(value);
        text
    }
}

/// A CFStringRef's text, or `None` if `value` is not a string.
///
/// # Safety
/// `value` must be a live CF object.
unsafe fn cf_string(value: *const c_void) -> Option<String> {
    if CFGetTypeID(value) != CFStringGetTypeID() {
        return None;
    }
    Some((*(value as *const NSString)).to_string())
}

fn process_name(pid: i32) -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is 256 bytes; proc_name NUL-terminates.
    let len = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf8_lossy(&buf[..len as usize]).into_owned()
}

/// Every process with input running right now, squawk included (the caller
/// filters by pid).
pub fn input_processes() -> Vec<MicUser> {
    process_objects()
        .into_iter()
        .filter(|&object| read_u32(object, PROCESS_IS_RUNNING_INPUT).is_some_and(|v| v != 0))
        .filter_map(|object| {
            let pid = read_u32(object, PROCESS_PID)? as i32;
            Some(MicUser {
                pid,
                bundle_id: read_bundle_id(object),
                name: process_name(pid),
            })
        })
        .collect()
}

/// Titles of a running app's windows (Accessibility), for telling a Meet tab
/// from any other page that uses the mic. Empty when unreadable.
pub fn window_titles(bundle_id: &str) -> Vec<String> {
    autoreleasepool(|_| {
        let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(
            &NSString::from_str(bundle_id),
        );
        apps.iter()
            .flat_map(|app| app_window_titles(app.processIdentifier()))
            .collect()
    })
}

fn app_window_titles(pid: i32) -> Vec<String> {
    let mut titles = Vec::new();
    // SAFETY: plain AX/CF calls; every copied object is released below.
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return titles;
        }
        let windows_attr = NSString::from_str("AXWindows");
        let title_attr = NSString::from_str("AXTitle");
        let mut windows: *const c_void = std::ptr::null();
        let status = AXUIElementCopyAttributeValue(
            app,
            (&*windows_attr as *const NSString).cast(),
            &mut windows,
        );
        if status == 0 && !windows.is_null() {
            if CFGetTypeID(windows) == CFArrayGetTypeID() {
                for i in 0..CFArrayGetCount(windows) {
                    let window = CFArrayGetValueAtIndex(windows, i);
                    let mut title: *const c_void = std::ptr::null();
                    let status = AXUIElementCopyAttributeValue(
                        window,
                        (&*title_attr as *const NSString).cast(),
                        &mut title,
                    );
                    if status == 0 && !title.is_null() {
                        if let Some(text) = cf_string(title) {
                            titles.push(text);
                        }
                        CFRelease(title);
                    }
                }
            }
            CFRelease(windows);
        }
        CFRelease(app);
    }
    titles
}

/// The call apps among `users` by display name ("Zoom", "Google Meet"),
/// leaving out `own_pid`. Browsers count only when one of their windows is
/// on a known call site.
pub fn call_apps_among(users: &[MicUser], own_pid: i32) -> Vec<String> {
    let mut apps: Vec<String> = Vec::new();
    for user in users {
        let name = match calls::classify(user, own_pid) {
            Some(Source::App(name)) => Some(name.to_string()),
            Some(Source::Browser { bundle_id, .. }) => window_titles(bundle_id)
                .iter()
                .find_map(|title| calls::call_site(title))
                .map(str::to_string),
            None => None,
        };
        if let Some(name) = name {
            if !apps.contains(&name) {
                apps.push(name);
            }
        }
    }
    apps.sort();
    apps
}

/// The call apps using the mic now.
pub fn call_apps(own_pid: i32) -> Vec<String> {
    call_apps_among(&input_processes(), own_pid)
}

/// Keeps the listener thread alive; dropping it does not stop the thread
/// (the app runs it for its whole life).
pub struct MicWatcher;

impl MicWatcher {
    /// Start watching. `on_change` gets the call apps using the mic, first
    /// right away and then on every change, from the watcher thread.
    pub fn start(own_pid: i32, on_change: impl Fn(Vec<String>) + Send + 'static) -> MicWatcher {
        thread::Builder::new()
            .name("squawk-mic-watch".into())
            .spawn(move || watch(own_pid, on_change))
            .expect("spawn mic watcher");
        MicWatcher
    }
}

/// How often to re-read while a call app or a browser has input running
/// (a tab title can change without the mic changing; always-on listeners
/// like "Hey Siri" do not count).
const BUSY_POLL: Duration = Duration::from_secs(1);
/// A safety re-read while nothing is: listeners do the real work.
const IDLE_POLL: Duration = Duration::from_secs(30);

fn watch(own_pid: i32, on_change: impl Fn(Vec<String>)) {
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    // Held for the life of the thread: Core Audio keeps a copy, but the
    // blocks' captured sender must stay valid.
    let mut listeners: Vec<RcBlock<dyn Fn(u32, *const c_void)>> = Vec::new();
    let mut listening: HashSet<AudioObjectId> = HashSet::new();

    let listen = |object: AudioObjectId,
                  selector: u32,
                  listeners: &mut Vec<RcBlock<dyn Fn(u32, *const c_void)>>| {
        let tx = wake_tx.clone();
        let block = RcBlock::new(move |_n: u32, _a: *const c_void| {
            if std::env::var_os("SQUAWK_MIC_WATCH_DEBUG").is_some() {
                eprintln!("mic watch: woken");
            }
            let _ = tx.send(());
        });
        let addr = address(selector);
        // SAFETY: a valid address and a live block; a null queue runs the
        // block on a Core Audio thread, which only sends on a channel.
        let status = unsafe {
            AudioObjectAddPropertyListenerBlock(object, &addr, std::ptr::null_mut(), &block)
        };
        if status != 0 {
            log::debug!("mic watch: listener on {object} failed ({status})");
        }
        listeners.push(block);
    };
    listen(SYSTEM_OBJECT, PROCESS_OBJECT_LIST, &mut listeners);
    listen(SYSTEM_OBJECT, HARDWARE_DEVICES, &mut listeners);

    let mut last: Option<Vec<String>> = None;
    loop {
        for object in process_objects() {
            if listening.insert(object) {
                listen(object, PROCESS_IS_RUNNING_INPUT, &mut listeners);
            }
        }
        for device in devices() {
            if listening.insert(device) {
                listen(device, DEVICE_IS_RUNNING_SOMEWHERE, &mut listeners);
            }
        }
        let users = input_processes();
        let apps = call_apps_among(&users, own_pid);
        if last.as_ref() != Some(&apps) {
            on_change(apps.clone());
            last = Some(apps);
        }
        let call_shaped = users.iter().any(|u| calls::classify(u, own_pid).is_some());
        let wait = if call_shaped { BUSY_POLL } else { IDLE_POLL };
        match wake_rx.recv_timeout(wait) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {
                // Coalesce a burst of notifications into one read.
                while wake_rx.try_recv().is_ok() {}
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}
