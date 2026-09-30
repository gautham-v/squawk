//! One day of dictations as markdown.
//!
//! ```markdown
//! # 2026-09-29
//!
//! ## 14:03:12 · Ghostty · squawk
//! Fix the resampler so it handles 48 kHz input.
//!
//! ## 14:05:40 · Safari
//! Thanks, that works.
//! ```
//!
//! Each entry is a `## HH:MM:SS · app[ · project]` heading followed by the
//! text exactly as it was pasted. A text line that would itself parse as an
//! entry heading is written with a leading backslash.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

/// The separator in an entry heading.
const SEP: &str = " · ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictationEntry {
    /// Local wall-clock time it was pasted.
    pub at: NaiveDateTime,
    /// The front app's name ("Ghostty", "Safari").
    pub app: String,
    /// The Claude Code / Codex session's project (its cwd's directory name),
    /// when there was one.
    pub project: Option<String>,
    /// What was pasted, without the trailing space.
    pub text: String,
}

impl DictationEntry {
    /// "Ghostty · squawk" or "Safari": the label the popover and TUI show.
    pub fn source(&self) -> String {
        match &self.project {
            Some(p) => format!("{}{SEP}{p}", self.app),
            None => self.app.clone(),
        }
    }
}

/// The entry as it is appended: heading, text, one newline.
pub fn format_entry(entry: &DictationEntry) -> String {
    let mut out = format!(
        "## {}{SEP}{}",
        entry.at.format("%H:%M:%S"),
        clean_field(&entry.app)
    );
    if let Some(project) = entry.project.as_deref().filter(|p| !p.is_empty()) {
        out.push_str(SEP);
        out.push_str(&clean_field(project));
    }
    out.push('\n');
    for line in entry.text.trim().lines() {
        if needs_escape(line) {
            out.push('\\');
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Parse a day file. Anything before the first entry heading (the `# date`
/// title, or notes the user added) is ignored; so are headings that do not
/// carry a time.
pub fn parse_day(date: NaiveDate, text: &str) -> Vec<DictationEntry> {
    let mut out: Vec<DictationEntry> = Vec::new();
    let mut lines: Vec<&str> = Vec::new();
    let mut current: Option<DictationEntry> = None;

    let mut finish = |current: &mut Option<DictationEntry>, lines: &mut Vec<&str>| {
        if let Some(mut e) = current.take() {
            e.text = lines
                .iter()
                .map(|l| unescape(l))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
            out.push(e);
        }
        lines.clear();
    };

    for line in text.lines() {
        if let Some((time, app, project)) = parse_heading(line) {
            finish(&mut current, &mut lines);
            current = Some(DictationEntry {
                at: date.and_time(time),
                app,
                project,
                text: String::new(),
            });
        } else if current.is_some() {
            lines.push(line);
        }
    }
    finish(&mut current, &mut lines);
    out
}

fn parse_heading(line: &str) -> Option<(NaiveTime, String, Option<String>)> {
    let rest = line.strip_prefix("## ")?;
    let mut parts = rest.split(SEP);
    let time = NaiveTime::parse_from_str(parts.next()?.trim(), "%H:%M:%S").ok()?;
    let app = parts.next().unwrap_or("").trim().to_string();
    let project = parts
        .next()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    Some((time, app, project))
}

/// A text line that would read as a heading once any leading backslashes
/// are removed. Writing adds one backslash, reading removes one, so text
/// that already starts with backslashes round-trips too.
fn needs_escape(line: &str) -> bool {
    parse_heading(line.trim_start_matches('\\')).is_some()
}

fn unescape(line: &str) -> String {
    match line.strip_prefix('\\') {
        Some(rest) if needs_escape(line) => rest.to_string(),
        _ => line.to_string(),
    }
}

/// App and project names go in a heading: no newlines, no separator.
fn clean_field(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
        .replace(SEP, " ")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 29).unwrap()
    }

    fn e(text: &str) -> DictationEntry {
        DictationEntry {
            at: date().and_hms_opt(9, 1, 2).unwrap(),
            app: "Ghostty".into(),
            project: Some("squawk".into()),
            text: text.into(),
        }
    }

    #[test]
    fn heading_format() {
        assert_eq!(
            format_entry(&e("Hello.")),
            "## 09:01:02 · Ghostty · squawk\nHello.\n"
        );
    }

    #[test]
    fn source_label() {
        assert_eq!(e("x").source(), "Ghostty · squawk");
        let mut x = e("x");
        x.project = None;
        assert_eq!(x.source(), "Ghostty");
    }

    #[test]
    fn heading_like_text_round_trips() {
        for text in [
            "## 10:00:00 · fake",
            "\\## 10:00:00 · fake",
            "## not a time",
            "# title-ish",
        ] {
            let entry = e(text);
            let file = format!("# 2026-09-29\n\n{}", format_entry(&entry));
            let parsed = parse_day(date(), &file);
            assert_eq!(parsed.len(), 1, "{text}: {file}");
            assert_eq!(parsed[0].text, text);
        }
    }

    #[test]
    fn user_notes_before_first_entry_are_ignored() {
        let file = "# 2026-09-29\nsome note\n\n## 08:00:00 · Notes\nhi\n\n## bogus\nstill hi\n";
        let parsed = parse_day(date(), file);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].app, "Notes");
        assert_eq!(parsed[0].text, "hi\n\n## bogus\nstill hi");
    }

    #[test]
    fn separator_in_app_name_is_neutralised() {
        let mut entry = e("x");
        entry.app = "Weird · App".into();
        let parsed = parse_day(date(), &format_entry(&entry));
        assert_eq!(parsed[0].app, "Weird App");
        assert_eq!(parsed[0].project.as_deref(), Some("squawk"));
    }
}
