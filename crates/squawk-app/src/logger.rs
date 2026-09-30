//! A tiny `log` backend that appends to `squawk.log`, one line per record:
//! `<RFC3339> <LEVEL> <target>: <message>`.
//!
//! Info and up from squawk's own crates; warnings and up from everything
//! else (ORT and friends are chatty at info). Dictated text is never logged —
//! callers log lengths only.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Local, SecondsFormat};
use log::{Level, LevelFilter, Log, Metadata, Record};

struct FileLogger {
    file: Mutex<File>,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        wanted(metadata.level(), metadata.target())
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format_line(
            Local::now(),
            record.level(),
            record.target(),
            &record.args().to_string(),
        );
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
        }
        if cfg!(debug_assertions) {
            eprint!("{line}");
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

/// Install the logger. A second call (or a log file that cannot be opened)
/// leaves logging off rather than failing the app.
pub fn install(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(file) = OpenOptions::new().create(true).append(true).open(path) else {
        eprintln!("squawk: cannot open log file {}", path.display());
        return;
    };
    let logger = Box::new(FileLogger {
        file: Mutex::new(file),
    });
    if log::set_boxed_logger(logger).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}

fn wanted(level: Level, target: &str) -> bool {
    let ours = target.starts_with("squawk") || target == "dictation" || target == "meeting";
    if ours {
        level <= Level::Info
    } else {
        level <= Level::Warn
    }
}

/// One log line, newline included.
pub fn format_line(at: DateTime<Local>, level: Level, target: &str, message: &str) -> String {
    // One record is one line: fold any newline a message smuggles in.
    let message = message.replace('\n', " ");
    format!(
        "{} {} {}: {}\n",
        at.to_rfc3339_opts(SecondsFormat::Secs, false),
        level,
        target,
        message
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn a_line_is_time_level_target_message() {
        let at = Local.with_ymd_and_hms(2026, 9, 29, 14, 3, 12).unwrap();
        let line = format_line(at, Level::Info, "dictation", "audio=1.0s\nchars=3");
        assert!(line.starts_with("2026-09-29T14:03:12"), "{line}");
        assert!(
            line.ends_with(" INFO dictation: audio=1.0s chars=3\n"),
            "{line}"
        );
        assert_eq!(line.matches('\n').count(), 1);
    }

    #[test]
    fn other_crates_only_log_warnings() {
        assert!(wanted(Level::Info, "squawk_app::controller"));
        assert!(wanted(Level::Info, "dictation"));
        assert!(!wanted(Level::Debug, "squawk_engine"));
        assert!(!wanted(Level::Info, "ort::session"));
        assert!(wanted(Level::Warn, "ort::session"));
    }
}
