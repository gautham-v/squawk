//! One meeting as markdown.
//!
//! ```markdown
//! ---
//! title: "Weekly sync"
//! date: 2026-09-29T14:00:00-07:00
//! length: 00:42:10
//! ---
//!
//! # Weekly sync
//!
//! **You** 00:00:04
//! Morning. Can everyone hear me?
//!
//! **Them** 00:00:09
//! Yes, loud and clear.
//! ```
//!
//! While recording, the front matter also has `status: recording`; the
//! recorder rewrites the whole file after each chunk and drops that line when
//! the meeting ends. Timestamps are offsets from the start, `HH:MM:SS`.

use std::path::PathBuf;

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};

use crate::status::format_hms;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Speaker {
    /// The mic.
    You,
    /// System audio: everyone else on the call.
    Them,
}

impl Speaker {
    pub fn label(self) -> &'static str {
        match self {
            Speaker::You => "You",
            Speaker::Them => "Them",
        }
    }
}

/// A piece of transcript from one track, timed from the meeting start. What
/// the recorder produces per chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub speaker: Speaker,
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
}

/// One block in the file: a speaker's consecutive speech.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    pub speaker: Speaker,
    /// Seconds from the meeting start.
    pub start_secs: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meeting {
    pub title: String,
    pub started_at: DateTime<Local>,
    pub length_secs: u64,
    /// Still recording: the file is partial.
    pub in_progress: bool,
    /// Stretches with no "You" because the mic was lost, oldest first.
    pub mic_outages: Vec<Outage>,
    pub utterances: Vec<Utterance>,
}

/// A stretch of the meeting in which the mic was gone: only "Them" was
/// recorded. Written to the front matter as `mic_lost: 00:00:28 to
/// 00:05:10` (`to end` while it lasts) and noted under the title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outage {
    /// Seconds from the meeting start.
    pub from_secs: u64,
    /// `None` while the mic is still gone.
    pub to_secs: Option<u64>,
}

impl Outage {
    fn front_matter(&self) -> String {
        match self.to_secs {
            Some(to) => format!("{} to {}", format_hms(self.from_secs), format_hms(to)),
            None => format!("{} to end", format_hms(self.from_secs)),
        }
    }

    fn parse(value: &str) -> Option<Outage> {
        let (from, to) = value.split_once(" to ")?;
        let from_secs = parse_hms(from.trim())?;
        let to_secs = match to.trim() {
            "end" => None,
            t => Some(parse_hms(t)?),
        };
        Some(Outage { from_secs, to_secs })
    }

    fn note(&self) -> String {
        match self.to_secs {
            Some(to) => format!(
                "*Your mic was lost at {}; only the other side was recorded until {}.*",
                format_hms(self.from_secs),
                format_hms(to)
            ),
            None => format!(
                "*Your mic was lost at {}; only the other side was recorded after that.*",
                format_hms(self.from_secs)
            ),
        }
    }
}

/// What a list of meetings needs, without the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingSummary {
    pub path: PathBuf,
    pub title: String,
    /// From the front matter, else the file name; `None` if neither parses.
    pub started_at: Option<DateTime<Local>>,
    pub length_secs: u64,
    pub in_progress: bool,
    /// The first few words said, for a preview line.
    pub snippet: String,
}

impl MeetingSummary {
    pub(crate) fn from_file(path: PathBuf, file_name: &str, text: &str) -> MeetingSummary {
        let front = parse_front_matter(text);
        let (name_time, name_title) = parse_file_name(file_name);
        let meeting = parse_meeting(text);
        let snippet: String = meeting
            .utterances
            .first()
            .map(|u| u.text.chars().take(120).collect())
            .unwrap_or_default();
        MeetingSummary {
            path,
            title: front
                .title
                .clone()
                .filter(|t| !t.is_empty())
                .or(name_title)
                .unwrap_or_else(|| "Meeting".into()),
            started_at: front.date.or(name_time),
            length_secs: front.length.unwrap_or(0),
            in_progress: front.recording,
            snippet,
        }
    }
}

/// Sort both tracks' segments by start time and fold each run of one
/// speaker into a single block, timed at its first segment. Empty segments
/// are dropped. Ties go to You, so an answer never precedes its question.
pub fn merge_segments(mut segments: Vec<Segment>) -> Vec<Utterance> {
    segments.retain(|s| !s.text.trim().is_empty());
    segments.sort_by(|a, b| {
        a.start_secs
            .total_cmp(&b.start_secs)
            .then_with(|| (a.speaker == Speaker::Them).cmp(&(b.speaker == Speaker::Them)))
    });
    let mut out: Vec<Utterance> = Vec::new();
    for s in segments {
        let text = s.text.trim();
        match out.last_mut() {
            Some(last) if last.speaker == s.speaker => {
                last.text.push(' ');
                last.text.push_str(text);
            }
            _ => out.push(Utterance {
                speaker: s.speaker,
                start_secs: s.start_secs.max(0.0) as u64,
                text: text.to_string(),
            }),
        }
    }
    out
}

pub fn format_meeting(m: &Meeting) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("title: {}\n", yaml_quote(&m.title)));
    out.push_str(&format!(
        "date: {}\n",
        m.started_at
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
    ));
    out.push_str(&format!("length: {}\n", format_hms(m.length_secs)));
    if m.in_progress {
        out.push_str("status: recording\n");
    }
    for o in &m.mic_outages {
        out.push_str(&format!("mic_lost: {}\n", o.front_matter()));
    }
    out.push_str("---\n\n");
    out.push_str(&format!("# {}\n", one_line(&m.title)));
    for o in &m.mic_outages {
        out.push_str(&format!("\n{}\n", o.note()));
    }
    for u in &m.utterances {
        out.push_str(&format!(
            "\n**{}** {}\n{}\n",
            u.speaker.label(),
            format_hms(u.start_secs),
            u.text.trim()
        ));
    }
    out
}

/// Parse a meeting file. Lenient: missing front matter gives defaults, and
/// text outside speaker blocks is ignored.
pub fn parse_meeting(text: &str) -> Meeting {
    let front = parse_front_matter(text);
    let body = strip_front_matter(text);
    let mut utterances: Vec<Utterance> = Vec::new();
    let mut current: Option<Utterance> = None;
    for line in body.lines() {
        if let Some((speaker, secs)) = parse_block_heading(line) {
            if let Some(u) = current.take() {
                utterances.push(u);
            }
            current = Some(Utterance {
                speaker,
                start_secs: secs,
                text: String::new(),
            });
        } else if let Some(u) = current.as_mut() {
            if line.trim().is_empty() {
                continue;
            }
            if !u.text.is_empty() {
                u.text.push('\n');
            }
            u.text.push_str(line);
        }
    }
    if let Some(u) = current.take() {
        utterances.push(u);
    }
    Meeting {
        title: front.title.unwrap_or_else(|| "Meeting".into()),
        started_at: front
            .date
            .unwrap_or_else(|| Local.timestamp_opt(0, 0).single().expect("epoch")),
        length_secs: front.length.unwrap_or(0),
        in_progress: front.recording,
        mic_outages: front.mic_outages,
        utterances,
    }
}

/// `YYYY-MM-DD HHMM <title>.md`, title made safe for a file name.
pub fn meeting_file_name(started_at: &DateTime<Local>, title: &str) -> String {
    format!(
        "{} {}.md",
        started_at.format("%Y-%m-%d %H%M"),
        sanitize_title(title)
    )
}

/// Strip what a file name cannot hold (`/ \ : * ? " < > |`, control chars),
/// collapse spaces, cap at 80 chars. Empty becomes "Meeting".
pub fn sanitize_title(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(|c: char| c == '.' || c == '-' || c.is_whitespace());
    let capped: String = trimmed.chars().take(80).collect();
    let capped = capped.trim_end().to_string();
    if capped.is_empty() {
        "Meeting".into()
    } else {
        capped
    }
}

#[derive(Debug, Default)]
struct FrontMatter {
    title: Option<String>,
    date: Option<DateTime<Local>>,
    length: Option<u64>,
    recording: bool,
    mic_outages: Vec<Outage>,
}

fn front_matter_lines(text: &str) -> Option<Vec<&str>> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut out = Vec::new();
    for line in lines {
        if line.trim() == "---" {
            return Some(out);
        }
        out.push(line);
    }
    None
}

fn parse_front_matter(text: &str) -> FrontMatter {
    let mut fm = FrontMatter::default();
    for line in front_matter_lines(text).unwrap_or_default() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "title" => fm.title = Some(yaml_unquote(value)),
            "date" => {
                fm.date = DateTime::parse_from_rfc3339(value)
                    .ok()
                    .map(|d| d.with_timezone(&Local))
            }
            "length" => fm.length = parse_hms(value),
            "status" => fm.recording = value == "recording",
            "mic_lost" => fm.mic_outages.extend(Outage::parse(value)),
            _ => {}
        }
    }
    fm
}

fn strip_front_matter(text: &str) -> &str {
    if front_matter_lines(text).is_none() {
        return text;
    }
    // Skip the opening fence, then everything through the closing one.
    let after_open = text.find('\n').map(|i| i + 1).unwrap_or(text.len());
    let rest = &text[after_open..];
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        if line.trim() == "---" {
            return &rest[offset..];
        }
    }
    ""
}

fn parse_block_heading(line: &str) -> Option<(Speaker, u64)> {
    let line = line.trim();
    [Speaker::You, Speaker::Them]
        .into_iter()
        .find_map(|speaker| {
            let rest = line.strip_prefix(&format!("**{}**", speaker.label()))?;
            Some((speaker, parse_hms(rest.trim())?))
        })
}

fn parse_hms(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let n: Vec<u64> = parts
        .iter()
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    Some(n[0] * 3600 + n[1] * 60 + n[2])
}

fn parse_file_name(name: &str) -> (Option<DateTime<Local>>, Option<String>) {
    let stem = name.strip_suffix(".md").unwrap_or(name);
    // "YYYY-MM-DD HHMM" is 15 chars.
    let (Some(stamp), Some(rest)) = (stem.get(..15), stem.get(15..)) else {
        return (None, Some(stem.to_string()).filter(|s| !s.is_empty()));
    };
    match NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H%M") {
        Ok(naive) => (
            Local.from_local_datetime(&naive).earliest(),
            Some(rest.trim().to_string()).filter(|s| !s.is_empty()),
        ),
        Err(_) => (None, Some(stem.to_string())),
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Always double-quoted: valid YAML whatever the title holds.
fn yaml_quote(s: &str) -> String {
    format!(
        "\"{}\"",
        one_line(s).replace('\\', "\\\\").replace('"', "\\\"")
    )
}

fn yaml_unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else {
                out.push(c);
            }
        }
        out
    } else if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        s[1..s.len() - 1].replace("''", "'")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(speaker: Speaker, start: f64, text: &str) -> Segment {
        Segment {
            speaker,
            start_secs: start,
            end_secs: start + 1.0,
            text: text.into(),
        }
    }

    #[test]
    fn merge_interleaves_and_coalesces() {
        let merged = merge_segments(vec![
            seg(Speaker::Them, 9.0, "Yes, loud and clear."),
            seg(Speaker::You, 4.0, "Morning."),
            seg(Speaker::You, 6.5, "Can everyone hear me?"),
            seg(Speaker::Them, 12.0, "Let's start."),
            seg(Speaker::You, 20.0, " "),
            seg(Speaker::You, 30.2, "Sure."),
        ]);
        assert_eq!(
            merged,
            vec![
                Utterance {
                    speaker: Speaker::You,
                    start_secs: 4,
                    text: "Morning. Can everyone hear me?".into()
                },
                Utterance {
                    speaker: Speaker::Them,
                    start_secs: 9,
                    text: "Yes, loud and clear. Let's start.".into()
                },
                Utterance {
                    speaker: Speaker::You,
                    start_secs: 30,
                    text: "Sure.".into()
                },
            ]
        );
    }

    #[test]
    fn ties_go_to_you() {
        let merged = merge_segments(vec![
            seg(Speaker::Them, 5.0, "answer"),
            seg(Speaker::You, 5.0, "question"),
        ]);
        assert_eq!(merged[0].speaker, Speaker::You);
    }

    #[test]
    fn format_shape() {
        let m = Meeting {
            title: "Weekly \"sync\"".into(),
            started_at: Local.with_ymd_and_hms(2026, 9, 29, 14, 0, 0).unwrap(),
            length_secs: 2530,
            in_progress: true,
            mic_outages: Vec::new(),
            utterances: vec![Utterance {
                speaker: Speaker::You,
                start_secs: 1386,
                text: "Hi.".into(),
            }],
        };
        let text = format_meeting(&m);
        assert!(text.starts_with("---\ntitle: \"Weekly \\\"sync\\\"\"\ndate: 2026-09-29T14:00:00"));
        assert!(text.contains("\nlength: 00:42:10\nstatus: recording\n---\n\n# Weekly \"sync\"\n"));
        assert!(text.ends_with("\n**You** 00:23:06\nHi.\n"));
        assert_eq!(parse_meeting(&text), m);
    }

    #[test]
    fn mic_outages_round_trip_and_are_noted() {
        let m = Meeting {
            title: "Sync".into(),
            started_at: Local.with_ymd_and_hms(2026, 10, 7, 12, 58, 0).unwrap(),
            length_secs: 3084,
            in_progress: false,
            mic_outages: vec![
                Outage {
                    from_secs: 28,
                    to_secs: Some(310),
                },
                Outage {
                    from_secs: 900,
                    to_secs: None,
                },
            ],
            utterances: vec![Utterance {
                speaker: Speaker::Them,
                start_secs: 30,
                text: "Hello?".into(),
            }],
        };
        let text = format_meeting(&m);
        assert!(text.contains("\nmic_lost: 00:00:28 to 00:05:10\nmic_lost: 00:15:00 to end\n---\n"));
        assert!(text.contains(
            "# Sync\n\n*Your mic was lost at 00:00:28; only the other side was recorded until 00:05:10.*\n\n*Your mic was lost at 00:15:00; only the other side was recorded after that.*\n\n**Them** 00:00:30\n"
        ));
        assert_eq!(parse_meeting(&text), m);
        assert_eq!(Outage::parse("garbage"), None);
    }

    #[test]
    fn parse_tolerates_hand_edits() {
        let text = "---\ntitle: Plain title\nlength: 00:01:00\n---\n\n# Plain title\n\nnotes up top\n\n**Them** 00:00:05\nline one\nline two\n\n**You** bad\n";
        let m = parse_meeting(text);
        assert_eq!(m.title, "Plain title");
        assert_eq!(m.length_secs, 60);
        assert!(!m.in_progress);
        assert_eq!(m.utterances.len(), 1);
        assert_eq!(m.utterances[0].text, "line one\nline two\n**You** bad");
    }

    #[test]
    fn no_front_matter() {
        let m = parse_meeting("**You** 00:00:01\nhello\n");
        assert_eq!(m.title, "Meeting");
        assert_eq!(m.utterances[0].text, "hello");
    }

    #[test]
    fn sanitize() {
        assert_eq!(
            sanitize_title("Weekly sync: Q4/roadmap?"),
            "Weekly sync- Q4-roadmap"
        );
        assert_eq!(sanitize_title("  "), "Meeting");
        assert_eq!(sanitize_title("..hidden"), "hidden");
        assert_eq!(sanitize_title("a\nb"), "a b");
        assert_eq!(sanitize_title(&"x".repeat(200)).len(), 80);
    }

    #[test]
    fn file_name_parse() {
        let (t, title) = parse_file_name("2026-09-29 1400 Weekly sync.md");
        assert_eq!(t.unwrap().format("%H:%M").to_string(), "14:00");
        assert_eq!(title.as_deref(), Some("Weekly sync"));
        let (t, title) = parse_file_name("random.md");
        assert!(t.is_none());
        assert_eq!(title.as_deref(), Some("random"));
    }
}
