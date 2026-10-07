//! What the TUI shows, read from the files, and a cheap fingerprint of those
//! files so the run loop can re-read only when something changed (a new
//! dictation appended, a live meeting rewritten after a chunk).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use squawk_core::store::{parse_meeting, DictationEntry, Meeting, MeetingSummary};
use squawk_core::Store;

/// The most dictations the History tab holds.
pub const MAX_DICTATIONS: usize = 500;

/// A meeting for the list and the preview.
#[derive(Debug, Clone, PartialEq)]
pub struct MeetingItem {
    pub summary: MeetingSummary,
    pub meeting: Meeting,
    /// The file after its front matter: what is shown when the file has no
    /// speaker blocks (hand notes), what `enter` copies, what search reads.
    pub body: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Data {
    /// Newest first.
    pub dictations: Vec<DictationEntry>,
    /// Newest first.
    pub meetings: Vec<MeetingItem>,
}

pub fn load(store: &Store) -> anyhow::Result<Data> {
    let dictations = store.recent(MAX_DICTATIONS)?;
    let mut meetings = Vec::new();
    for summary in store.meetings()? {
        // a file that vanished between listing and reading is just skipped
        let Ok(text) = std::fs::read_to_string(&summary.path) else {
            continue;
        };
        meetings.push(MeetingItem {
            meeting: parse_meeting(&text),
            body: body_of(&text).to_string(),
            summary,
        });
    }
    Ok(Data {
        dictations,
        meetings,
    })
}

/// The text after a `---` front matter block, or all of it.
fn body_of(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---\n") else {
        return text;
    };
    match rest.find("\n---\n") {
        Some(end) => rest[end + 5..].trim_start_matches('\n'),
        None => text,
    }
}

/// Name, length and mtime of every markdown file in both directories,
/// hashed. One `readdir` + a `stat` per file: fine every two seconds for
/// years of daily files.
pub fn signature(store: &Store) -> u64 {
    let mut h = DefaultHasher::new();
    for dir in [store.dictations_dir(), store.meetings_dir()] {
        hash_dir(dir, &mut h);
    }
    h.finish()
}

fn hash_dir(dir: &Path, h: &mut DefaultHasher) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        0u8.hash(h);
        return;
    };
    let mut files: Vec<(String, u64, Option<std::time::SystemTime>)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if !name.ends_with(".md") {
                return None;
            }
            let meta = e.metadata().ok()?;
            Some((name, meta.len(), meta.modified().ok()))
        })
        .collect();
    files.sort();
    files.hash(h);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::{Local, NaiveDateTime, TimeZone};
    use squawk_core::store::{Speaker, Utterance};
    use squawk_core::Paths;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn dictation(when: &str, app: &str, project: Option<&str>, text: &str) -> DictationEntry {
        DictationEntry {
            at: at(when),
            app: app.into(),
            project: project.map(Into::into),
            text: text.into(),
        }
    }

    /// Generic fixture files: five dictations over three days, a finished
    /// meeting and a live one.
    pub(crate) fn write_fixtures(store: &Store) {
        for e in [
            dictation(
                "2026-09-27 09:15:00",
                "Ghostty",
                Some("catcher"),
                "Make the key bar dimmer.",
            ),
            dictation(
                "2026-09-28 17:42:10",
                "Notes",
                None,
                "Pick up the bike on Thursday.\nAnd call the dentist.",
            ),
            dictation(
                "2026-09-29 11:20:45",
                "Safari",
                None,
                "Thanks, that works for me.",
            ),
            dictation(
                "2026-09-29 14:03:12",
                "Ghostty",
                Some("squawk"),
                "Fix the resampler so it handles 48 kHz input.",
            ),
            dictation(
                "2026-09-29 14:07:02",
                "Ghostty",
                Some("squawk"),
                "Run the tests again and show me the failures in @src/audio.rs.",
            ),
        ] {
            store.append_dictation(&e).unwrap();
        }
        let weekly = Local.with_ymd_and_hms(2026, 9, 29, 14, 0, 0).unwrap();
        store
            .write_meeting(
                &store.new_meeting_path(&weekly, "Weekly sync"),
                &Meeting {
                    title: "Weekly sync".into(),
                    started_at: weekly,
                    length_secs: 2530,
                    in_progress: false,
                    mic_outages: Vec::new(),
                    utterances: vec![
                        Utterance {
                            speaker: Speaker::You,
                            start_secs: 4,
                            text: "Morning. Can everyone hear me?".into(),
                        },
                        Utterance {
                            speaker: Speaker::Them,
                            start_secs: 9,
                            text: "Yes, loud and clear.".into(),
                        },
                    ],
                },
            )
            .unwrap();
        let standup = Local.with_ymd_and_hms(2026, 9, 29, 15, 30, 0).unwrap();
        store
            .write_meeting(
                &store.new_meeting_path(&standup, "Standup"),
                &Meeting {
                    title: "Standup".into(),
                    started_at: standup,
                    length_secs: 185,
                    in_progress: true,
                    mic_outages: Vec::new(),
                    utterances: vec![Utterance {
                        speaker: Speaker::You,
                        start_secs: 2,
                        text: "Quick one today.".into(),
                    }],
                },
            )
            .unwrap();
    }

    pub(crate) fn fixture_data() -> Data {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&Paths::under(dir.path()));
        write_fixtures(&store);
        load(&store).unwrap()
    }

    #[test]
    fn loads_newest_first() {
        let data = fixture_data();
        assert_eq!(data.dictations.len(), 5);
        assert!(data.dictations[0].text.starts_with("Run the tests"));
        assert_eq!(data.dictations[4].app, "Ghostty");
        assert_eq!(data.meetings.len(), 2);
        assert_eq!(data.meetings[0].summary.title, "Standup");
        assert!(data.meetings[0].summary.in_progress);
        assert_eq!(data.meetings[1].meeting.utterances.len(), 2);
        assert!(data.meetings[1].body.starts_with("# Weekly sync\n"));
    }

    #[test]
    fn empty_dirs_load_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&Paths::under(dir.path()));
        assert_eq!(load(&store).unwrap(), Data::default());
    }

    #[test]
    fn body_skips_front_matter_only_when_there_is_one() {
        assert_eq!(body_of("---\ntitle: x\n---\n\n# X\nhi\n"), "# X\nhi\n");
        assert_eq!(body_of("just notes\n"), "just notes\n");
        assert_eq!(body_of("---\nunterminated"), "---\nunterminated");
    }

    #[test]
    fn signature_changes_when_a_dictation_is_appended() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&Paths::under(dir.path()));
        let empty = signature(&store);
        write_fixtures(&store);
        let full = signature(&store);
        assert_ne!(empty, full);
        assert_eq!(full, signature(&store));
        store
            .append_dictation(&dictation("2026-09-29 16:00:00", "Mail", None, "More."))
            .unwrap();
        assert_ne!(full, signature(&store));
    }
}
