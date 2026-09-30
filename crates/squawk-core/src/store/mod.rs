//! The markdown files under the data dir: the source of truth for history
//! and meetings. The app writes them; the popover, the CLI, the TUI and
//! Claude read them. Nothing is cached in between, so a hand edit is seen
//! by everyone on the next read.

mod dictation;
mod meeting;

pub use dictation::{format_entry, parse_day, DictationEntry};
pub use meeting::{
    format_meeting, meeting_file_name, merge_segments, parse_meeting, sanitize_title, Meeting,
    MeetingSummary, Segment, Speaker, Utterance,
};

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDate};

use crate::error::Result;
use crate::paths::Paths;

/// Reads and writes the dictation and meeting files. Cheap to construct and
/// to clone; holds only paths.
#[derive(Debug, Clone)]
pub struct Store {
    dictations_dir: PathBuf,
    meetings_dir: PathBuf,
}

impl Store {
    pub fn new(paths: &Paths) -> Store {
        Store {
            dictations_dir: paths.dictations_dir.clone(),
            meetings_dir: paths.meetings_dir.clone(),
        }
    }

    pub fn dictations_dir(&self) -> &Path {
        &self.dictations_dir
    }

    pub fn meetings_dir(&self) -> &Path {
        &self.meetings_dir
    }

    /// `<dictations>/YYYY-MM-DD.md`
    pub fn day_path(&self, date: NaiveDate) -> PathBuf {
        self.dictations_dir
            .join(format!("{}.md", date.format("%Y-%m-%d")))
    }

    /// Append one dictation to its day's file, creating the file (with a
    /// `# YYYY-MM-DD` title) and the directory as needed.
    pub fn append_dictation(&self, entry: &DictationEntry) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.dictations_dir)?;
        let path = self.day_path(entry.at.date());
        let is_new = !path.exists();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let mut body = String::new();
        if is_new {
            body.push_str(&format!("# {}\n", entry.at.date().format("%Y-%m-%d")));
        }
        body.push('\n');
        body.push_str(&format_entry(entry));
        file.write_all(body.as_bytes())?;
        Ok(path)
    }

    /// One day's dictations, oldest first. A missing file is an empty day.
    pub fn day(&self, date: NaiveDate) -> Result<Vec<DictationEntry>> {
        match std::fs::read_to_string(self.day_path(date)) {
            Ok(text) => Ok(parse_day(date, &text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Every day that has a file, newest first.
    pub fn days(&self) -> Result<Vec<NaiveDate>> {
        let mut days: Vec<NaiveDate> = read_dir_names(&self.dictations_dir)?
            .iter()
            .filter_map(|name| name.strip_suffix(".md"))
            .filter_map(|stem| NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok())
            .collect();
        days.sort_unstable_by(|a, b| b.cmp(a));
        Ok(days)
    }

    /// The `n` most recent dictations across days, newest first.
    pub fn recent(&self, n: usize) -> Result<Vec<DictationEntry>> {
        let mut out = Vec::new();
        if n == 0 {
            return Ok(out);
        }
        for date in self.days()? {
            let mut entries = self.day(date)?;
            entries.reverse();
            for e in entries {
                out.push(e);
                if out.len() == n {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }

    /// The newest dictation, if any.
    pub fn last(&self) -> Result<Option<DictationEntry>> {
        Ok(self.recent(1)?.into_iter().next())
    }

    /// A fresh path for a meeting that starts now, never one that exists:
    /// a second "Meeting" in the same minute gets " 2".
    pub fn new_meeting_path(&self, started_at: &DateTime<Local>, title: &str) -> PathBuf {
        let base = meeting_file_name(started_at, title);
        let stem = base.trim_end_matches(".md");
        let mut path = self.meetings_dir.join(&base);
        let mut k = 2;
        while path.exists() {
            path = self.meetings_dir.join(format!("{stem} {k}.md"));
            k += 1;
        }
        path
    }

    /// Write (or rewrite) a meeting file atomically: a reader never sees
    /// half a file, which matters because the recorder rewrites it after
    /// every chunk while the TUI or Claude may be reading it.
    pub fn write_meeting(&self, path: &Path, meeting: &Meeting) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("md.tmp");
        std::fs::write(&tmp, format_meeting(meeting))?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Read a meeting file back.
    pub fn read_meeting(&self, path: &Path) -> Result<Meeting> {
        let text = std::fs::read_to_string(path)?;
        Ok(parse_meeting(&text))
    }

    /// Every meeting file, newest first. Files whose front matter does not
    /// parse still appear, titled from their file name, so nothing the user
    /// wrote by hand disappears from the list.
    pub fn meetings(&self) -> Result<Vec<MeetingSummary>> {
        let mut out = Vec::new();
        for name in read_dir_names(&self.meetings_dir)? {
            if !name.ends_with(".md") {
                continue;
            }
            let path = self.meetings_dir.join(&name);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            out.push(MeetingSummary::from_file(path, &name, &text));
        }
        out.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.path.cmp(&a.path))
        });
        Ok(out)
    }
}

fn read_dir_names(dir: &Path) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    Ok(entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDateTime, TimeZone};

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        (dir, Store::new(&paths))
    }

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn entry(s: &str, text: &str) -> DictationEntry {
        DictationEntry {
            at: at(s),
            app: "Ghostty".into(),
            project: Some("squawk".into()),
            text: text.into(),
        }
    }

    #[test]
    fn append_then_read_back() {
        let (_d, store) = store();
        store
            .append_dictation(&entry("2026-09-29 14:03:12", "First."))
            .unwrap();
        store
            .append_dictation(&DictationEntry {
                at: at("2026-09-29 14:05:40"),
                app: "Safari".into(),
                project: None,
                text: "Second,\nwith two lines.".into(),
            })
            .unwrap();
        let path = store.day_path(at("2026-09-29 00:00:00").date());
        let body = std::fs::read_to_string(path).unwrap();
        assert_eq!(
            body,
            "# 2026-09-29\n\n## 14:03:12 · Ghostty · squawk\nFirst.\n\n## 14:05:40 · Safari\nSecond,\nwith two lines.\n"
        );
        let day = store.day(at("2026-09-29 00:00:00").date()).unwrap();
        assert_eq!(day.len(), 2);
        assert_eq!(day[0], entry("2026-09-29 14:03:12", "First."));
        assert_eq!(day[1].project, None);
        assert_eq!(day[1].text, "Second,\nwith two lines.");
    }

    #[test]
    fn recent_spans_days_newest_first() {
        let (_d, store) = store();
        store
            .append_dictation(&entry("2026-09-27 09:00:00", "a"))
            .unwrap();
        store
            .append_dictation(&entry("2026-09-29 10:00:00", "c"))
            .unwrap();
        store
            .append_dictation(&entry("2026-09-29 11:00:00", "d"))
            .unwrap();
        store
            .append_dictation(&entry("2026-09-28 12:00:00", "b"))
            .unwrap();
        let texts: Vec<String> = store
            .recent(3)
            .unwrap()
            .into_iter()
            .map(|e| e.text)
            .collect();
        assert_eq!(texts, ["d", "c", "b"]);
        assert_eq!(store.recent(10).unwrap().len(), 4);
        assert!(store.recent(0).unwrap().is_empty());
        assert_eq!(store.last().unwrap().unwrap().text, "d");
    }

    #[test]
    fn empty_store() {
        let (_d, store) = store();
        assert!(store.recent(5).unwrap().is_empty());
        assert!(store.last().unwrap().is_none());
        assert!(store.meetings().unwrap().is_empty());
    }

    #[test]
    fn stray_files_are_ignored() {
        let (_d, store) = store();
        std::fs::create_dir_all(store.dictations_dir()).unwrap();
        std::fs::write(store.dictations_dir().join("notes.md"), "x").unwrap();
        std::fs::write(store.dictations_dir().join(".DS_Store"), "x").unwrap();
        store
            .append_dictation(&entry("2026-09-29 14:03:12", "ok"))
            .unwrap();
        assert_eq!(store.days().unwrap().len(), 1);
    }

    #[test]
    fn meeting_round_trip_and_listing() {
        let (_d, store) = store();
        let started = Local.with_ymd_and_hms(2026, 9, 29, 14, 0, 0).unwrap();
        let path = store.new_meeting_path(&started, "Weekly sync: roadmap");
        assert!(path.ends_with("2026-09-29 1400 Weekly sync- roadmap.md"));
        let m = Meeting {
            title: "Weekly sync: roadmap".into(),
            started_at: started,
            length_secs: 2530,
            in_progress: false,
            utterances: vec![
                Utterance {
                    speaker: Speaker::You,
                    start_secs: 4,
                    text: "Hi all.".into(),
                },
                Utterance {
                    speaker: Speaker::Them,
                    start_secs: 31,
                    text: "Hello.".into(),
                },
            ],
        };
        store.write_meeting(&path, &m).unwrap();
        assert_eq!(store.read_meeting(&path).unwrap(), m);

        let later = Local.with_ymd_and_hms(2026, 9, 30, 9, 30, 0).unwrap();
        let p2 = store.new_meeting_path(&later, "");
        store
            .write_meeting(
                &p2,
                &Meeting {
                    title: "Meeting".into(),
                    started_at: later,
                    length_secs: 60,
                    in_progress: true,
                    utterances: vec![],
                },
            )
            .unwrap();
        let list = store.meetings().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].title, "Meeting");
        assert!(list[0].in_progress);
        assert_eq!(list[1].title, "Weekly sync: roadmap");
        assert_eq!(list[1].length_secs, 2530);
    }

    #[test]
    fn same_minute_same_title_gets_a_number() {
        let (_d, store) = store();
        let t = Local.with_ymd_and_hms(2026, 9, 29, 14, 0, 0).unwrap();
        let p1 = store.new_meeting_path(&t, "Meeting");
        std::fs::create_dir_all(p1.parent().unwrap()).unwrap();
        std::fs::write(&p1, "").unwrap();
        let p2 = store.new_meeting_path(&t, "Meeting");
        assert!(p2.ends_with("2026-09-29 1400 Meeting 2.md"));
    }

    #[test]
    fn hand_written_meeting_file_still_lists() {
        let (_d, store) = store();
        std::fs::create_dir_all(store.meetings_dir()).unwrap();
        std::fs::write(
            store.meetings_dir().join("2026-09-01 0900 Kickoff.md"),
            "just some notes",
        )
        .unwrap();
        let list = store.meetings().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Kickoff");
        assert_eq!(
            list[0]
                .started_at
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string()),
            Some("2026-09-01 09:00".into())
        );
    }
}
