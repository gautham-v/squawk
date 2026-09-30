//! The app's control socket: JSON lines over a Unix socket at
//! `Paths::socket`.
//!
//! One request per connection: the client connects, writes one [`Request`]
//! as a single JSON line, reads one [`Response`] line, and closes. That keeps
//! the server a plain accept loop and makes the protocol easy to poke at by
//! hand:
//!
//! ```sh
//! echo '{"cmd":"status"}' | nc -U ~/Library/Application\ Support/squawk/squawk.sock
//! ```
//!
//! The CLI uses [`send`]; the app's server uses [`read_request`] and
//! [`write_response`] (or [`serve_one`]).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::status::{MeetingInfo, StatusInfo};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Is the app there? Answered with `Pong`.
    Ping,
    /// Answered with `Status`.
    Status,
    /// Start a meeting. No title: the calendar's current event, else "Meeting".
    /// Answered with `MeetingStarted`, or `Error` if one is already running.
    MeetStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// Stop the meeting. Answered once the file is final (the last chunk is
    /// transcribed), with `MeetingStopped`, or `Error` if none is running.
    MeetStop,
    /// Re-read config.toml. Answered with `Ok`.
    Reload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Pong {
        version: String,
    },
    Status(StatusInfo),
    MeetingStarted(MeetingInfo),
    MeetingStopped {
        title: String,
        path: String,
        length_secs: u64,
    },
    Ok,
    Error {
        message: String,
    },
}

/// How long [`send`] waits for most answers.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// `MeetStop` waits for the last chunk to be transcribed.
pub const MEET_STOP_TIMEOUT: Duration = Duration::from_secs(120);

/// Send one request and wait for its answer. [`Error::NotRunning`] when
/// nothing is listening (no socket, or a stale one).
pub fn send(socket: &Path, request: &Request, timeout: Duration) -> Result<Response> {
    let stream = match UnixStream::connect(socket) {
        Ok(s) => s,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(Error::NotRunning(socket.to_path_buf()))
        }
        Err(e) => return Err(e.into()),
    };
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut writer = &stream;
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()?;

    let mut reader = BufReader::new(&stream);
    let mut answer = String::new();
    match reader.read_line(&mut answer) {
        Ok(0) => {
            return Err(Error::Ipc(
                "squawk closed the connection without answering".into(),
            ))
        }
        Ok(_) => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            return Err(Error::Ipc("squawk did not answer in time".into()))
        }
        Err(e) => return Err(e.into()),
    }
    serde_json::from_str(answer.trim_end())
        .map_err(|e| Error::Ipc(format!("could not read squawk's answer: {e}")))
}

/// Read one request line from a connection. A line that does not parse is
/// an `Err` the server should answer with `Response::Error`.
pub fn read_request(stream: &UnixStream) -> Result<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(Error::Ipc("empty request".into()));
    }
    serde_json::from_str(line.trim_end()).map_err(|e| Error::Ipc(format!("bad request: {e}")))
}

pub fn write_response(mut stream: &UnixStream, response: &Response) -> Result<()> {
    let mut line = serde_json::to_string(response)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    Ok(())
}

/// Serve one connection: read the request, answer it with `handler`, close.
/// Parse errors are answered, not propagated.
pub fn serve_one(stream: UnixStream, handler: impl FnOnce(Request) -> Response) -> Result<()> {
    stream.set_read_timeout(Some(DEFAULT_TIMEOUT))?;
    let response = match read_request(&stream) {
        Ok(request) => handler(request),
        Err(e) => Response::Error {
            message: e.to_string(),
        },
    };
    write_response(&stream, &response)
}

/// Bind the server socket, replacing a stale one left by a crash. Refuses
/// (with `Ipc`) if another live instance answers on it.
pub fn bind(socket: &Path) -> Result<std::os::unix::net::UnixListener> {
    if socket.exists() {
        if UnixStream::connect(socket).is_ok() {
            return Err(Error::Ipc(format!(
                "another squawk is already running ({})",
                socket.display()
            )));
        }
        std::fs::remove_file(socket)?;
    }
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(std::os::unix::net::UnixListener::bind(socket)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{AppState, ModelStatus, Permissions};

    #[test]
    fn request_json_shapes() {
        let cases = [
            (Request::Ping, r#"{"cmd":"ping"}"#),
            (Request::Status, r#"{"cmd":"status"}"#),
            (
                Request::MeetStart { title: None },
                r#"{"cmd":"meet_start"}"#,
            ),
            (
                Request::MeetStart {
                    title: Some("Standup".into()),
                },
                r#"{"cmd":"meet_start","title":"Standup"}"#,
            ),
            (Request::MeetStop, r#"{"cmd":"meet_stop"}"#),
            (Request::Reload, r#"{"cmd":"reload"}"#),
        ];
        for (req, json) in cases {
            assert_eq!(serde_json::to_string(&req).unwrap(), json);
            assert_eq!(serde_json::from_str::<Request>(json).unwrap(), req);
        }
    }

    #[test]
    fn response_json_shapes() {
        assert_eq!(
            serde_json::to_string(&Response::Ok).unwrap(),
            r#"{"type":"ok"}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::Error {
                message: "no".into()
            })
            .unwrap(),
            r#"{"type":"error","message":"no"}"#
        );
        let started = Response::MeetingStarted(MeetingInfo {
            title: "Standup".into(),
            path: "/x.md".into(),
            started_at: "2026-09-29T09:00:00-07:00".into(),
            elapsed_secs: 0,
        });
        let json = serde_json::to_string(&started).unwrap();
        assert!(
            json.starts_with(r#"{"type":"meeting_started","title":"Standup""#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), started);
    }

    #[test]
    fn round_trip_over_a_socket() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("s.sock");
        let listener = bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                serve_one(stream, |req| match req {
                    Request::Ping => Response::Pong {
                        version: "t".into(),
                    },
                    Request::Status => Response::Status(StatusInfo {
                        version: "t".into(),
                        state: AppState::Idle,
                        model: ModelStatus::Ready,
                        permissions: Permissions::default(),
                        meeting: None,
                        config_note: None,
                        dictations_this_run: 3,
                    }),
                    _ => Response::Ok,
                })
                .unwrap();
            }
        });
        assert_eq!(
            send(&sock, &Request::Ping, DEFAULT_TIMEOUT).unwrap(),
            Response::Pong {
                version: "t".into()
            }
        );
        match send(&sock, &Request::Status, DEFAULT_TIMEOUT).unwrap() {
            Response::Status(s) => assert_eq!(s.dictations_this_run, 3),
            other => panic!("{other:?}"),
        }
        server.join().unwrap();
        // A second bind while the first listener is gone replaces the stale file.
        assert!(bind(&sock).is_ok());
    }

    #[test]
    fn no_socket_is_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let err = send(
            &dir.path().join("none.sock"),
            &Request::Ping,
            DEFAULT_TIMEOUT,
        )
        .unwrap_err();
        assert!(matches!(err, Error::NotRunning(_)));
    }

    #[test]
    fn bad_request_line_is_answered() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("s.sock");
        let listener = bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve_one(stream, |_| Response::Ok).unwrap();
        });
        let stream = UnixStream::connect(&sock).unwrap();
        (&stream).write_all(b"{\"cmd\":\"nope\"}\n").unwrap();
        let mut answer = String::new();
        BufReader::new(&stream).read_line(&mut answer).unwrap();
        server.join().unwrap();
        let resp: Response = serde_json::from_str(answer.trim()).unwrap();
        assert!(matches!(resp, Response::Error { .. }));
    }
}
