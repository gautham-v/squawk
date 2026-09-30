//! Drawing: tabs line, list | preview, key bar. No boxes: air, weight and
//! faintness do the structuring, with one dim rule between the two panes.

use chrono::{Datelike, NaiveDate};
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;
use squawk_core::status::{format_elapsed, format_hms};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::app::{App, Tab};
use super::data::MeetingItem;
use super::theme;

/// The bar beside the selected row; two columns with its gap.
const SELECTED: &str = "▎ ";
const UNSELECTED: &str = "  ";

pub fn draw(f: &mut Frame, app: &mut App) {
    let [top, _, body, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let inset = Margin::new(1, 0);
    draw_tabs(f, app, top.inner(inset));
    draw_keys(f, app, keys.inner(inset));

    let body = body.inner(inset);
    let list_w = (body.width * 2 / 5).clamp(24.min(body.width), 56);
    let [list, rule, _, preview] = Layout::horizontal([
        Constraint::Length(list_w),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(0),
    ])
    .areas(body);
    draw_list(f, app, list);
    let bar: Vec<Line> = (0..rule.height).map(|_| Line::from("│")).collect();
    f.render_widget(Paragraph::new(bar).style(theme::RULE), rule);
    draw_preview(f, app, preview);
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
    let tab = |name: &'static str, on: bool| {
        Span::styled(name, if on { theme::STRONG } else { theme::DIM })
    };
    let left = Line::from(vec![
        tab("History", app.tab == Tab::History),
        Span::raw("   "),
        tab("Meetings", app.tab == Tab::Meetings),
    ]);
    f.render_widget(Paragraph::new(left), area);

    if !app.searching && app.query.is_empty() {
        return;
    }
    let (shown, total) = match app.tab {
        Tab::History => (app.visible_dictations().len(), app.data.dictations.len()),
        Tab::Meetings => (app.visible_meetings().len(), app.data.meetings.len()),
    };
    let query = format!("/{}", app.query);
    let count = format!("   {shown} of {total}");
    let right = Line::from(vec![
        Span::styled(query.clone(), theme::PLAIN),
        Span::styled(count.clone(), theme::DIM),
    ]);
    f.render_widget(Paragraph::new(right).right_aligned(), area);
    if app.searching {
        let w = (query.width() + count.width()) as u16;
        let x = area.right().saturating_sub(w) + query.width() as u16;
        f.set_cursor_position(Position::new(x.min(area.right().saturating_sub(1)), area.y));
    }
}

/// One line of the list, and which visible item it belongs to.
struct Row {
    line: Line<'static>,
    item: Option<usize>,
}

fn blank() -> Row {
    Row {
        line: Line::default(),
        item: None,
    }
}

fn gutter(selected: bool) -> Span<'static> {
    if selected {
        Span::styled(SELECTED, theme::MARK)
    } else {
        Span::raw(UNSELECTED)
    }
}

/// "Today", "Yesterday", "Mon Sep 28", with the year once it is not this one.
pub fn day_label(date: NaiveDate, today: NaiveDate) -> String {
    if date == today {
        "Today".into()
    } else if Some(date) == today.pred_opt() {
        "Yesterday".into()
    } else if date.year() == today.year() {
        date.format("%a %b %-d").to_string()
    } else {
        date.format("%a %b %-d %Y").to_string()
    }
}

fn history_rows(app: &App, width: usize) -> Vec<Row> {
    let text_w = width.saturating_sub(UNSELECTED.len());
    let mut rows = Vec::new();
    let mut day = None;
    for (pos, &i) in app.visible_dictations().iter().enumerate() {
        let e = &app.data.dictations[i];
        let on = pos == app.selected();
        if !rows.is_empty() {
            rows.push(blank());
        }
        if day != Some(e.at.date()) {
            day = Some(e.at.date());
            rows.push(Row {
                line: Line::from(vec![
                    Span::raw(UNSELECTED),
                    Span::styled(day_label(e.at.date(), app.today), theme::DIM),
                ]),
                item: None,
            });
        }
        let time = e.at.format("%H:%M").to_string();
        let source = fit(&e.source(), text_w.saturating_sub(time.len() + 2));
        rows.push(Row {
            line: Line::from(vec![
                gutter(on),
                Span::styled(time, theme::DIM),
                Span::raw("  "),
                Span::styled(source, if on { theme::STRONG } else { theme::PLAIN }),
            ]),
            item: Some(pos),
        });
        let first = e.text.lines().next().unwrap_or("");
        rows.push(Row {
            line: Line::from(vec![
                gutter(on),
                Span::styled(
                    fit(first, text_w),
                    if on { theme::PLAIN } else { theme::DIM },
                ),
            ]),
            item: Some(pos),
        });
    }
    rows
}

/// "Sep 29 14:00 · 42:10"
fn meeting_when(m: &MeetingItem) -> String {
    let len = format_elapsed(m.summary.length_secs);
    match m.summary.started_at {
        Some(t) => format!("{} · {len}", t.format("%b %-d %H:%M")),
        None => len,
    }
}

fn meeting_rows(app: &App, width: usize) -> Vec<Row> {
    let text_w = width.saturating_sub(UNSELECTED.len());
    let mut rows = Vec::new();
    for (pos, &i) in app.visible_meetings().iter().enumerate() {
        let m = &app.data.meetings[i];
        let on = pos == app.selected();
        if !rows.is_empty() {
            rows.push(blank());
        }
        rows.push(Row {
            line: Line::from(vec![
                gutter(on),
                Span::styled(
                    fit(&m.summary.title, text_w),
                    if on { theme::STRONG } else { theme::PLAIN },
                ),
            ]),
            item: Some(pos),
        });
        let mut second = vec![gutter(on), Span::styled(meeting_when(m), theme::DIM)];
        if m.summary.in_progress {
            second.push(Span::styled("  recording", theme::LIVE));
        }
        rows.push(Row {
            line: Line::from(second),
            item: Some(pos),
        });
    }
    rows
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = match app.tab {
        Tab::History => history_rows(app, area.width as usize),
        Tab::Meetings => meeting_rows(app, area.width as usize),
    };
    if rows.is_empty() {
        let msg = if !app.query.is_empty() {
            format!("Nothing matches “{}”.", app.query)
        } else if app.tab == Tab::History {
            "No dictations yet. Hold fn and talk.".into()
        } else {
            "No meetings yet. ⌥M records one.".into()
        };
        let p = Paragraph::new(Line::from(vec![
            Span::raw(UNSELECTED),
            Span::styled(msg, theme::DIM),
        ]))
        .wrap(Wrap { trim: false });
        f.render_widget(p, area);
        return;
    }
    let slot = match app.tab {
        Tab::History => 0,
        Tab::Meetings => 1,
    };
    let offset = scroll_to_selection(&rows, app.selected(), app.list_offset[slot], area.height);
    app.list_offset[slot] = offset;
    let lines: Vec<Line> = rows
        .into_iter()
        .skip(offset)
        .take(area.height as usize)
        .map(|r| r.line)
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

/// The first row to show so the selected item is on screen, moving as little
/// as possible, and taking the date line above the item along when scrolling
/// up to it.
fn scroll_to_selection(rows: &[Row], selected: usize, offset: usize, height: u16) -> usize {
    let height = height as usize;
    let Some(start) = rows.iter().position(|r| r.item == Some(selected)) else {
        return 0;
    };
    let end = rows[start..]
        .iter()
        .position(|r| r.item != Some(selected))
        .map_or(rows.len(), |n| start + n);
    let mut offset = offset.min(rows.len().saturating_sub(height));
    if start < offset {
        offset = start;
        // include the date line (and the blank before it) above the item
        while offset > 0 && rows[offset - 1].item.is_none() && start - offset < 2 {
            offset -= 1;
        }
    } else if end > offset + height {
        offset = end.saturating_sub(height);
    }
    offset
}

fn draw_preview(f: &mut Frame, app: &mut App, area: Rect) {
    let lines = match app.tab {
        Tab::History => history_preview(app),
        Tab::Meetings => meeting_preview(app),
    };
    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    app.preview_lines = p.line_count(area.width).min(u16::MAX as usize) as u16;
    app.preview_height = area.height;
    let max = app.preview_lines.saturating_sub(area.height);
    app.scroll = app.scroll.min(max);
    f.render_widget(p.scroll((app.scroll, 0)), area);
}

fn history_preview(app: &App) -> Vec<Line<'static>> {
    let Some(e) = app.selected_dictation() else {
        return Vec::new();
    };
    let meta = format!(
        "{} · {} · {}",
        day_label(e.at.date(), app.today),
        e.at.format("%H:%M:%S"),
        e.source()
    );
    let mut lines = vec![Line::styled(meta, theme::DIM), Line::default()];
    lines.extend(
        e.text
            .lines()
            .map(|l| Line::styled(l.to_string(), theme::PLAIN)),
    );
    lines
}

fn meeting_preview(app: &App) -> Vec<Line<'static>> {
    let Some(m) = app.selected_meeting() else {
        return Vec::new();
    };
    let mut meta = vec![Span::styled(
        match m.summary.started_at {
            Some(t) => format!(
                "{} · {}",
                t.format("%a %b %-d %H:%M"),
                format_elapsed(m.summary.length_secs)
            ),
            None => format_elapsed(m.summary.length_secs),
        },
        theme::DIM,
    )];
    if m.summary.in_progress {
        meta.push(Span::styled("  recording", theme::LIVE));
    }
    let mut lines = vec![
        Line::styled(m.summary.title.clone(), theme::STRONG),
        Line::from(meta),
        Line::default(),
    ];
    if m.meeting.utterances.is_empty() {
        lines.extend(
            m.body
                .lines()
                .map(|l| Line::styled(l.to_string(), theme::PLAIN)),
        );
        return lines;
    }
    for u in &m.meeting.utterances {
        lines.push(Line::from(vec![
            Span::styled(u.speaker.label(), theme::STRONG),
            Span::styled(format!("  {}", format_hms(u.start_secs)), theme::DIM),
        ]));
        lines.extend(
            u.text
                .lines()
                .map(|l| Line::styled(l.to_string(), theme::PLAIN)),
        );
        lines.push(Line::default());
    }
    lines
}

fn draw_keys(f: &mut Frame, app: &App, area: Rect) {
    let hints: Vec<(&str, &str)> = if app.searching {
        vec![("type", "to filter"), ("⏎", "keep"), ("esc", "clear")]
    } else {
        let mut h = vec![
            ("↑↓", "move"),
            ("/", "search"),
            ("⏎", "copy"),
            ("o", "open"),
            ("tab", "switch"),
        ];
        if !app.query.is_empty() {
            h.push(("esc", "clear"));
        }
        h.push(("q", "quit"));
        h
    };
    let mut spans = Vec::new();
    let mut room = area.width as usize;
    if let Some(msg) = app.flash_text() {
        let msg = format!("{msg}   ");
        room = room.saturating_sub(msg.width());
        spans.push(Span::styled(msg, theme::MARK));
    }
    spans.push(Span::styled(fit_hints(&hints, room), theme::DIM));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Worded hints if they all fit, bare keys if not, then keys dropped from
/// the right: the order they are worth least in.
fn fit_hints(hints: &[(&str, &str)], room: usize) -> String {
    let worded = hints
        .iter()
        .map(|(k, w)| format!("{k} {w}"))
        .collect::<Vec<_>>()
        .join("  ");
    if worded.width() <= room {
        return worded;
    }
    let mut keys: Vec<&str> = hints.iter().map(|(k, _)| *k).collect();
    while !keys.is_empty() {
        let line = keys.join("  ");
        if line.width() <= room {
            return line;
        }
        keys.pop();
    }
    String::new()
}

/// `s` cut to `width` columns, with an ellipsis when cut.
fn fit(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::data::{self, tests::write_fixtures};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;
    use ratatui::Terminal;
    use squawk_core::{Paths, Store};

    fn fixture_app() -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&Paths::under(dir.path()));
        write_fixtures(&store);
        let data = data::load(&store).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 9, 29).unwrap();
        (dir, App::new(store, data, today))
    }

    fn render(app: &mut App, w: u16, h: u16) -> (Vec<String>, ratatui::buffer::Buffer) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        let lines = (0..h)
            .map(|y| {
                let mut s = String::new();
                for x in 0..w {
                    s.push_str(buf[(x, y)].symbol());
                }
                s.trim_end().to_string()
            })
            .collect();
        (lines, buf)
    }

    /// The list column's words, joined across wrapped lines.
    fn list_text(lines: &[String]) -> String {
        lines[2..lines.len() - 1]
            .iter()
            .map(|l| l.split('│').next().unwrap_or("").trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn history_screen() {
        let (_d, mut app) = fixture_app();
        let (lines, _) = render(&mut app, 100, 18);
        let expected = [
            " History   Meetings",
            "",
            "   Today                                │  Today · 14:07:02 · Ghostty · squawk",
            " ▎ 14:07  Ghostty · squawk              │",
            " ▎ Run the tests again and show me the …│  Run the tests again and show me the failures in",
            "                                        │  @src/audio.rs.",
            "   14:03  Ghostty · squawk              │",
            "   Fix the resampler so it handles 48 k…│",
            "                                        │",
            "   11:20  Safari                        │",
            "   Thanks, that works for me.           │",
            "                                        │",
            "   Yesterday                            │",
            "   17:42  Notes                         │",
            "   Pick up the bike on Thursday.        │",
            "                                        │",
            "   Sun Sep 27                           │",
            " ↑↓ move  / search  ⏎ copy  o open  tab switch  q quit",
        ];
        assert_eq!(lines, expected);
    }

    #[test]
    fn list_scrolls_to_keep_the_selection_visible() {
        let (_d, mut app) = fixture_app();
        press(&mut app, KeyCode::Char('G'));
        let (lines, _) = render(&mut app, 100, 12);
        let body = lines[2..11].join("\n");
        assert!(body.contains("▎ 09:15  Ghostty · catcher"), "{body}");
        assert!(body.contains("▎ Make the key bar dimmer."), "{body}");
        assert!(body.contains("Sun Sep 27"), "{body}");
        assert!(!body.contains("14:07"), "{body}");
        // and back up to the top brings the date line with it
        press(&mut app, KeyCode::Char('g'));
        let (lines, _) = render(&mut app, 100, 12);
        assert_eq!(lines[2].split('│').next().unwrap().trim(), "Today");
    }

    #[test]
    fn meetings_screen() {
        let (_d, mut app) = fixture_app();
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char('j'));
        let (lines, buf) = render(&mut app, 90, 16);
        let expected = [
            " History   Meetings",
            "",
            "   Standup                          │  Weekly sync",
            "   Sep 29 15:30 · 3:05  recording   │  Tue Sep 29 14:00 · 42:10",
            "                                    │",
            " ▎ Weekly sync                      │  You  00:00:04",
            " ▎ Sep 29 14:00 · 42:10             │  Morning. Can everyone hear me?",
            "                                    │",
            "                                    │  Them  00:00:09",
            "                                    │  Yes, loud and clear.",
            "                                    │",
            "                                    │",
            "                                    │",
            "                                    │",
            "                                    │",
            " ↑↓ move  / search  ⏎ copy  o open  tab switch  q quit",
        ];
        assert_eq!(lines, expected);
        // the selected tab and speaker labels are bold, the rest is faint
        assert!(buf[(1, 0)].modifier.contains(Modifier::DIM));
        assert!(buf[(11, 0)].modifier.contains(Modifier::BOLD));
        assert!(buf[(39, 5)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn no_colour_but_the_rule_and_the_live_mark() {
        use ratatui::style::Color;
        let (_d, mut app) = fixture_app();
        press(&mut app, KeyCode::Tab);
        let (_, buf) = render(&mut app, 90, 16);
        for cell in buf.content() {
            assert!(
                matches!(cell.fg, Color::Reset | Color::DarkGray | Color::Red),
                "unexpected fg {:?} on {:?}",
                cell.fg,
                cell.symbol()
            );
            assert_eq!(cell.bg, Color::Reset);
        }
    }

    #[test]
    fn search_shows_query_count_and_cursor() {
        let (_d, mut app) = fixture_app();
        press(&mut app, KeyCode::Char('/'));
        for c in "ghostty".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let top: String = (0..80)
            .map(|x| term.backend().buffer()[(x, 0)].symbol().to_string())
            .collect();
        assert!(top.trim_end().ends_with("/ghostty   3 of 5"), "{top}");
        let bottom: String = (0..80)
            .map(|x| term.backend().buffer()[(x, 11)].symbol().to_string())
            .collect();
        assert_eq!(bottom.trim(), "type to filter  ⏎ keep  esc clear");
        let cursor = term.get_cursor_position().unwrap();
        assert_eq!(cursor.y, 0);
    }

    #[test]
    fn empty_and_no_match_states() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&Paths::under(dir.path()));
        let today = NaiveDate::from_ymd_opt(2026, 9, 29).unwrap();
        let mut app = App::new(store, data::Data::default(), today);
        let (lines, _) = render(&mut app, 80, 6);
        assert_eq!(list_text(&lines), "No dictations yet. Hold fn and talk.");
        press(&mut app, KeyCode::Tab);
        let (lines, _) = render(&mut app, 80, 6);
        assert_eq!(list_text(&lines), "No meetings yet. ⌥M records one.");

        let (_d, mut app) = fixture_app();
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('Q'));
        press(&mut app, KeyCode::Char('Z'));
        let (lines, _) = render(&mut app, 80, 6);
        assert_eq!(list_text(&lines), "Nothing matches “QZ”.");
    }

    #[test]
    fn flash_leads_the_key_bar() {
        let (_d, mut app) = fixture_app();
        app.flash("Copied");
        let (lines, _) = render(&mut app, 100, 8);
        assert!(lines[7].starts_with(" Copied   ↑↓ move"), "{}", lines[7]);
    }

    #[test]
    fn narrow_key_bar_drops_words_then_keys() {
        let hints = [("↑↓", "move"), ("/", "search"), ("q", "quit")];
        assert_eq!(fit_hints(&hints, 40), "↑↓ move  / search  q quit");
        assert_eq!(fit_hints(&hints, 12), "↑↓  /  q");
        assert_eq!(fit_hints(&hints, 5), "↑↓  /");
        assert_eq!(fit_hints(&hints, 1), "");
    }

    #[test]
    fn day_labels() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 29).unwrap();
        let d = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
        assert_eq!(day_label(today, today), "Today");
        assert_eq!(day_label(d(2026, 9, 28), today), "Yesterday");
        assert_eq!(day_label(d(2026, 9, 1), today), "Tue Sep 1");
        assert_eq!(day_label(d(2025, 12, 31), today), "Wed Dec 31 2025");
    }

    #[test]
    fn fit_cuts_with_an_ellipsis() {
        assert_eq!(fit("hello", 5), "hello");
        assert_eq!(fit("hello world", 6), "hello…");
        assert_eq!(fit("日本語テキスト", 7), "日本語…");
        assert_eq!(fit("abc", 0), "");
    }

    #[test]
    fn preview_scrolls_within_bounds() {
        let (_d, mut app) = fixture_app();
        let long: String = (0..60).map(|i| format!("line {i}\n")).collect();
        app.data.dictations[0].text = long;
        render(&mut app, 80, 12);
        assert_eq!(app.preview_height, 9);
        assert_eq!(app.preview_lines, 62);
        for _ in 0..30 {
            app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        }
        let (lines, _) = render(&mut app, 80, 12);
        assert_eq!(app.scroll, 62 - 9);
        assert!(lines[10].ends_with("line 59"), "{lines:?}");
    }
}
