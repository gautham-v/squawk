// screencapturekit links a Swift bridge that needs the Swift runtime
// (libswift_Concurrency) from /usr/lib/swift. Its own build script's rpath
// does not reach binaries in other packages, so every binary that links
// squawk-engine (this crate's tests and examples, squawk-app, squawk-cli)
// needs this line in its build.rs.
fn main() {
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
