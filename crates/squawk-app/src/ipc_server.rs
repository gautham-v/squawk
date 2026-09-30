//! The socket server: `squawk_core::ipc::bind`, then an accept loop on its
//! own thread; each connection is served with `squawk_core::ipc::serve_one`
//! on a short-lived thread (so a slow `MeetStop` never blocks `Status`),
//! forwarding the request to the controller as `Command::Ipc` and waiting
//! for the reply. The socket file is removed on quit.

use std::path::Path;
use std::thread::JoinHandle;

use crate::controller::Controller;

pub fn spawn(socket: &Path, controller: Controller) -> Result<JoinHandle<()>, squawk_core::Error> {
    let _ = (socket, controller);
    todo!("app agent")
}
