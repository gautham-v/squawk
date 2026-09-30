//! squawk-engine reaches ScreenCaptureKit through a Swift bridge, so the
//! binary links the Swift runtime (`@rpath/libswift_Concurrency.dylib`).
//! On macOS that lives in the OS at /usr/lib/swift; without this rpath the
//! binary (and its test harness) aborts at launch in dyld.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
