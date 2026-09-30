//! The socket server: `squawk_core::ipc::bind`, then an accept loop on its
//! own thread; each connection is served with `squawk_core::ipc::serve_one`
//! on a short-lived thread (so a slow `MeetStop` never blocks `Status`),
//! forwarding the request to the controller as `Command::Ipc` and waiting
//! for the reply. The caller removes the socket file on quit.

use std::path::Path;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use squawk_core::ipc::{self, Request, Response};

use crate::controller::{Command, Controller};

/// Bind and start serving. Errors when another squawk already answers on
/// the socket.
pub fn spawn(socket: &Path, controller: Controller) -> Result<JoinHandle<()>, squawk_core::Error> {
    let listener = ipc::bind(socket)?;
    let handle = thread::Builder::new()
        .name("squawk-ipc".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let controller = controller.clone();
                let _ = thread::Builder::new()
                    .name("squawk-ipc-conn".into())
                    .spawn(move || {
                        let result = ipc::serve_one(stream, |request| ask(&controller, request));
                        if let Err(e) = result {
                            log::warn!("ipc: {e}");
                        }
                    });
            }
        })?;
    Ok(handle)
}

/// Forward one request to the controller and wait for its answer.
fn ask(controller: &Controller, request: Request) -> Response {
    if request == Request::Ping {
        return Response::Pong {
            version: squawk_core::VERSION.into(),
        };
    }
    let wait = reply_timeout(&request);
    let (reply, answer) = crossbeam_channel::bounded(1);
    controller.send(Command::Ipc { request, reply });
    answer
        .recv_timeout(wait)
        .unwrap_or_else(|_| Response::Error {
            message: "squawk did not answer in time".into(),
        })
}

/// A stop waits for the meeting's last chunks; everything else is quick.
pub fn reply_timeout(request: &Request) -> Duration {
    match request {
        Request::MeetStop => ipc::MEET_STOP_TIMEOUT,
        _ => ipc::DEFAULT_TIMEOUT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use squawk_core::status::MeetingInfo;

    #[test]
    fn only_a_stop_waits_long() {
        assert_eq!(reply_timeout(&Request::MeetStop), ipc::MEET_STOP_TIMEOUT);
        assert_eq!(reply_timeout(&Request::Status), ipc::DEFAULT_TIMEOUT);
        assert_eq!(
            reply_timeout(&Request::MeetStart { title: None }),
            ipc::DEFAULT_TIMEOUT
        );
    }

    /// The whole path: a client on the socket, the server, a stand-in
    /// controller answering, and the answer back on the socket.
    #[test]
    fn requests_reach_the_controller_and_answers_come_back() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.sock");
        let (tx, rx) = crossbeam_channel::unbounded();
        spawn(&socket, Controller::from_sender(tx)).unwrap();
        std::thread::spawn(move || {
            while let Ok(command) = rx.recv() {
                if let Command::Ipc { request, reply } = command {
                    let _ = reply.send(match request {
                        Request::MeetStart { title } => Response::MeetingStarted(MeetingInfo {
                            title: title.unwrap_or_default(),
                            path: "/m.md".into(),
                            started_at: "2026-09-29T14:00:00-07:00".into(),
                            elapsed_secs: 0,
                        }),
                        _ => Response::Ok,
                    });
                }
            }
        });

        // Ping is answered without the controller.
        assert_eq!(
            ipc::send(&socket, &Request::Ping, ipc::DEFAULT_TIMEOUT).unwrap(),
            Response::Pong {
                version: squawk_core::VERSION.into()
            }
        );
        match ipc::send(
            &socket,
            &Request::MeetStart {
                title: Some("Standup".into()),
            },
            ipc::DEFAULT_TIMEOUT,
        )
        .unwrap()
        {
            Response::MeetingStarted(info) => assert_eq!(info.title, "Standup"),
            other => panic!("{other:?}"),
        }
        // A second instance refuses to bind over a live one.
        let (tx2, _rx2) = crossbeam_channel::unbounded();
        assert!(spawn(&socket, Controller::from_sender(tx2)).is_err());
    }
}
