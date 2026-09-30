//! Plain-text output for the subcommands, kept pure so every shape in
//! SPEC.md ("squawk-cli") is a unit test rather than a manual check. No
//! colour anywhere: the output is read by people and by Claude alike, and
//! piped as often as not.

use squawk_core::status::{format_elapsed, MeetingInfo, Permissions, StatusInfo};
use squawk_core::store::{DictationEntry, MeetingSummary};
use squawk_core::AppState;

/// `squawk history`: a date line when the day changes, then
/// `14:03  Ghostty · squawk` and the text indented two spaces, entries
/// separated by a blank line.
pub fn history(entries: &[DictationEntry]) -> String {
    let mut out = String::new();
    let mut day = None;
    for e in entries {
        if !out.is_empty() {
            out.push('\n');
        }
        if day != Some(e.at.date()) {
            day = Some(e.at.date());
            out.push_str(&e.at.format("%a %Y-%m-%d").to_string());
            out.push('\n');
        }
        out.push_str(&format!("{}  {}\n", e.at.format("%H:%M"), e.source()));
        for line in e.text.lines() {
            if line.is_empty() {
                out.push('\n');
            } else {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

/// `squawk history --json`: one object per line.
pub fn history_json(entries: &[DictationEntry]) -> String {
    /// Field order is the documented one, so a struct and not a map.
    #[derive(serde::Serialize)]
    struct Row<'a> {
        at: String,
        app: &'a str,
        project: Option<&'a str>,
        text: &'a str,
    }
    let mut out = String::new();
    for e in entries {
        let row = Row {
            at: e.at.format("%Y-%m-%dT%H:%M:%S").to_string(),
            app: &e.app,
            project: e.project.as_deref(),
            text: &e.text,
        };
        out.push_str(&serde_json::to_string(&row).expect("plain strings serialize"));
        out.push('\n');
    }
    out
}

/// `squawk meet list`: `2026-09-29 14:00  42:10  Weekly sync`, lengths
/// right-aligned so titles line up.
pub fn meetings(list: &[MeetingSummary]) -> String {
    let lengths: Vec<String> = list.iter().map(|m| format_elapsed(m.length_secs)).collect();
    let width = lengths.iter().map(|l| l.len()).max().unwrap_or(0);
    let mut out = String::new();
    for (m, len) in list.iter().zip(&lengths) {
        let when = m
            .started_at
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| format!("{:16}", "-"));
        out.push_str(&format!("{when}  {len:>width$}  {}", m.title));
        if m.in_progress {
            out.push_str("  recording");
        }
        out.push('\n');
    }
    out
}

/// `Recording "Standup" → <path>`
pub fn meeting_started(info: &MeetingInfo) -> String {
    format!("Recording \"{}\" → {}\n", info.title, info.path)
}

/// `Saved "Standup" (42:10) → <path>`
pub fn meeting_stopped(title: &str, path: &str, length_secs: u64) -> String {
    format!(
        "Saved \"{title}\" ({}) → {path}\n",
        format_elapsed(length_secs)
    )
}

/// The first line of `squawk status`: what dictation is doing.
pub fn state_line(state: &AppState) -> String {
    match state {
        AppState::Idle => "idle".into(),
        AppState::Recording {
            hands_free,
            elapsed_ms,
        } => {
            let t = format_elapsed(elapsed_ms / 1000);
            if *hands_free {
                format!("recording {t} (hands-free)")
            } else {
                format!("recording {t}")
            }
        }
        AppState::Transcribing => "transcribing".into(),
        AppState::NotReady { reason } => format!("not ready: {reason}"),
    }
}

fn grant(g: Option<bool>, when_missing: &str) -> String {
    match g {
        Some(true) => "granted".into(),
        Some(false) => format!("not granted{when_missing}"),
        None => "unknown".into(),
    }
}

/// `squawk status` for a running app: a headline, then aligned lines.
pub fn status(info: &StatusInfo) -> String {
    let Permissions {
        accessibility,
        microphone,
        screen_recording,
    } = info.permissions;
    let meeting = match &info.meeting {
        Some(m) => format!(
            "{} · {} → {}",
            m.title,
            format_elapsed(m.elapsed_secs),
            m.path
        ),
        None => "none".into(),
    };
    let mut rows: Vec<(&str, String)> = vec![
        ("model", info.model.label()),
        ("mic", grant(microphone, "")),
        ("accessibility", grant(accessibility, "")),
        ("screen audio", grant(screen_recording, " (meetings only)")),
        ("meeting", meeting),
    ];
    if let Some(note) = &info.config_note {
        rows.push(("config", note.clone()));
    }
    let mut out = format!("squawk {} · {}\n", info.version, state_line(&info.state));
    out.push_str(&aligned(&rows));
    out
}

/// `label  value` lines with the values in one column.
pub fn aligned(rows: &[(&str, String)]) -> String {
    let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    rows.iter()
        .map(|(k, v)| format!("{k:width$}  {v}\n"))
        .collect()
}

/// The one updating line `squawk model download` keeps rewriting on stderr.
pub fn download_progress(downloaded: u64, total: Option<u64>) -> String {
    let mb = |b: u64| b / 1_000_000;
    match total {
        Some(total) if total > 0 => {
            let pct = (downloaded as f64 / total as f64 * 100.0).clamp(0.0, 100.0);
            format!("Downloading {pct:.0}%  {}/{} MB", mb(downloaded), mb(total))
        }
        _ => format!("Downloading {} MB", mb(downloaded)),
    }
}

/// `audio 12.3s · model load 1.4s · transcribe 410ms`
pub fn timings(audio_secs: f64, load: std::time::Duration, run: std::time::Duration) -> String {
    format!(
        "audio {audio_secs:.1}s · model load {:.1}s · transcribe {}ms",
        load.as_secs_f64(),
        run.as_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, NaiveDateTime, TimeZone};
    use squawk_core::ModelStatus;
    use std::path::PathBuf;

    fn entry(at: &str, app: &str, project: Option<&str>, text: &str) -> DictationEntry {
        DictationEntry {
            at: NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M:%S").unwrap(),
            app: app.into(),
            project: project.map(Into::into),
            text: text.into(),
        }
    }

    #[test]
    fn history_groups_by_day_and_indents() {
        let entries = [
            entry("2026-09-29 14:05:40", "Safari", None, "Thanks, that works."),
            entry(
                "2026-09-29 14:03:12",
                "Ghostty",
                Some("squawk"),
                "Fix the resampler.\nThen the tests.",
            ),
            entry("2026-09-28 09:00:00", "Notes", None, "Buy milk."),
        ];
        assert_eq!(
            history(&entries),
            "Tue 2026-09-29\n\
             14:05  Safari\n  Thanks, that works.\n\
             \n\
             14:03  Ghostty · squawk\n  Fix the resampler.\n  Then the tests.\n\
             \n\
             Mon 2026-09-28\n\
             09:00  Notes\n  Buy milk.\n"
        );
        assert_eq!(history(&[]), "");
    }

    #[test]
    fn history_json_is_one_object_per_line() {
        let out = history_json(&[
            entry(
                "2026-09-29 14:03:12",
                "Ghostty",
                Some("squawk"),
                "say \"hi\"",
            ),
            entry("2026-09-29 14:05:40", "Safari", None, "a\nb"),
        ]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            r#"{"at":"2026-09-29T14:03:12","app":"Ghostty","project":"squawk","text":"say \"hi\""}"#
        );
        let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["project"], serde_json::Value::Null);
        assert_eq!(v["text"], "a\nb");
    }

    fn summary(title: &str, start: Option<(u32, u32)>, len: u64, live: bool) -> MeetingSummary {
        MeetingSummary {
            path: PathBuf::from(format!("/m/{title}.md")),
            title: title.into(),
            started_at: start.map(|(h, m)| Local.with_ymd_and_hms(2026, 9, 29, h, m, 0).unwrap()),
            length_secs: len,
            in_progress: live,
            snippet: String::new(),
        }
    }

    #[test]
    fn meeting_list_aligns_lengths() {
        let out = meetings(&[
            summary("Standup", Some((15, 0)), 65, true),
            summary("Weekly sync", Some((14, 0)), 2530, false),
            summary("Planning", Some((9, 30)), 3725, false),
            summary("Notes", None, 0, false),
        ]);
        assert_eq!(
            out,
            "2026-09-29 15:00     1:05  Standup  recording\n\
             2026-09-29 14:00    42:10  Weekly sync\n\
             2026-09-29 09:30  1:02:05  Planning\n\
             -                    0:00  Notes\n"
        );
    }

    #[test]
    fn meeting_start_and_stop_lines() {
        let info = MeetingInfo {
            title: "Standup".into(),
            path: "/m/2026-09-29 1500 Standup.md".into(),
            started_at: "2026-09-29T15:00:00-07:00".into(),
            elapsed_secs: 0,
        };
        assert_eq!(
            meeting_started(&info),
            "Recording \"Standup\" → /m/2026-09-29 1500 Standup.md\n"
        );
        assert_eq!(
            meeting_stopped("Standup", "/m/a.md", 2530),
            "Saved \"Standup\" (42:10) → /m/a.md\n"
        );
    }

    #[test]
    fn state_lines() {
        assert_eq!(state_line(&AppState::Idle), "idle");
        assert_eq!(
            state_line(&AppState::Recording {
                hands_free: false,
                elapsed_ms: 7_400
            }),
            "recording 0:07"
        );
        assert_eq!(
            state_line(&AppState::Recording {
                hands_free: true,
                elapsed_ms: 62_000
            }),
            "recording 1:02 (hands-free)"
        );
        assert_eq!(state_line(&AppState::Transcribing), "transcribing");
        assert_eq!(
            state_line(&AppState::NotReady {
                reason: "Needs Accessibility".into()
            }),
            "not ready: Needs Accessibility"
        );
    }

    #[test]
    fn status_block() {
        let info = StatusInfo {
            version: "0.1.0".into(),
            state: AppState::Idle,
            model: ModelStatus::Ready,
            permissions: Permissions {
                accessibility: Some(true),
                microphone: Some(true),
                screen_recording: Some(false),
            },
            meeting: Some(MeetingInfo {
                title: "Standup".into(),
                path: "/m/a.md".into(),
                started_at: String::new(),
                elapsed_secs: 724,
            }),
            config_note: Some("config.toml: line 3: bad value".into()),
            dictations_this_run: 4,
        };
        assert_eq!(
            status(&info),
            "squawk 0.1.0 · idle\n\
             model          Model ready\n\
             mic            granted\n\
             accessibility  granted\n\
             screen audio   not granted (meetings only)\n\
             meeting        Standup · 12:04 → /m/a.md\n\
             config         config.toml: line 3: bad value\n"
        );
        let bare = StatusInfo {
            permissions: Permissions::default(),
            meeting: None,
            config_note: None,
            ..info
        };
        let out = status(&bare);
        assert!(out.contains("mic            unknown\n"));
        assert!(out.contains("meeting        none\n"));
        assert!(!out.contains("config"));
    }

    #[test]
    fn download_line() {
        assert_eq!(
            download_progress(201_000_000, Some(478_517_071)),
            "Downloading 42%  201/478 MB"
        );
        assert_eq!(download_progress(5_000_000, None), "Downloading 5 MB");
        assert_eq!(download_progress(0, Some(0)), "Downloading 0 MB");
    }

    #[test]
    fn timing_line() {
        use std::time::Duration;
        assert_eq!(
            timings(
                12.34,
                Duration::from_millis(1400),
                Duration::from_millis(410)
            ),
            "audio 12.3s · model load 1.4s · transcribe 410ms"
        );
    }
}
