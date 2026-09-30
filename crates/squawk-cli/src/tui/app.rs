//! TUI state and key handling, kept free of drawing so it is testable.
//!
//! Keys never do I/O themselves: they change state and return an
//! [`Effect`] for the run loop to carry out (copy, open an editor, reload,
//! quit). That keeps every binding a plain unit test.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use squawk_core::store::DictationEntry;
use squawk_core::Store;

use super::data::{Data, MeetingItem};

/// How long "Copied" (or an error) stays in the key bar.
pub const FLASH_FOR: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    History,
    Meetings,
}

impl Tab {
    fn index(self) -> usize {
        match self {
            Tab::History => 0,
            Tab::Meetings => 1,
        }
    }

    fn other(self) -> Tab {
        match self {
            Tab::History => Tab::Meetings,
            Tab::Meetings => Tab::History,
        }
    }
}

/// What the run loop should do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Quit,
    Copy(String),
    Edit(PathBuf),
    Reload,
}

/// Which item is selected, by identity, so a reload or a new filter keeps
/// the same item under the cursor when it is still there.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    Dictation(chrono::NaiveDateTime, String),
    Meeting(PathBuf),
}

pub struct App {
    pub tab: Tab,
    pub data: Data,
    /// For "Today"/"Yesterday" labels; fixed in tests.
    pub today: NaiveDate,
    store: Store,
    /// The filter, applied to both tabs.
    pub query: String,
    /// Typing goes into the query.
    pub searching: bool,
    /// Selected position in each tab's *visible* list.
    sel: [usize; 2],
    /// First preview line shown.
    pub scroll: u16,
    /// Preview lines after wrapping, and rows on screen; set by the drawer so
    /// scrolling can clamp and page by half a screen.
    pub preview_lines: u16,
    pub preview_height: u16,
    /// First list row shown in each tab; kept by the drawer.
    pub list_offset: [usize; 2],
    pub flash: Option<(String, Instant)>,
}

impl App {
    pub fn new(store: Store, data: Data, today: NaiveDate) -> App {
        App {
            tab: Tab::History,
            data,
            today,
            store,
            query: String::new(),
            searching: false,
            sel: [0, 0],
            scroll: 0,
            preview_lines: 0,
            preview_height: 0,
            list_offset: [0, 0],
            flash: None,
        }
    }

    /// Indices into `data.dictations` that match the query, in order.
    pub fn visible_dictations(&self) -> Vec<usize> {
        let terms = terms(&self.query);
        (0..self.data.dictations.len())
            .filter(|&i| {
                let e = &self.data.dictations[i];
                matches_all(
                    &terms,
                    &[&e.text, &e.app, e.project.as_deref().unwrap_or("")],
                )
            })
            .collect()
    }

    /// Indices into `data.meetings` that match the query (title or
    /// anything said), in order.
    pub fn visible_meetings(&self) -> Vec<usize> {
        let terms = terms(&self.query);
        (0..self.data.meetings.len())
            .filter(|&i| {
                let m = &self.data.meetings[i];
                matches_all(&terms, &[&m.summary.title, &m.body])
            })
            .collect()
    }

    fn visible_len(&self, tab: Tab) -> usize {
        match tab {
            Tab::History => self.visible_dictations().len(),
            Tab::Meetings => self.visible_meetings().len(),
        }
    }

    /// Selected position in the current tab's visible list.
    pub fn selected(&self) -> usize {
        self.sel[self.tab.index()]
    }

    pub fn selected_dictation(&self) -> Option<&DictationEntry> {
        let vis = self.visible_dictations();
        vis.get(self.sel[0]).map(|&i| &self.data.dictations[i])
    }

    pub fn selected_meeting(&self) -> Option<&MeetingItem> {
        let vis = self.visible_meetings();
        vis.get(self.sel[1]).map(|&i| &self.data.meetings[i])
    }

    fn key_of(&self, tab: Tab) -> Option<Key> {
        match tab {
            Tab::History => {
                let vis = self.visible_dictations();
                vis.get(self.sel[0]).map(|&i| {
                    let e = &self.data.dictations[i];
                    Key::Dictation(e.at, e.text.clone())
                })
            }
            Tab::Meetings => {
                let vis = self.visible_meetings();
                vis.get(self.sel[1])
                    .map(|&i| Key::Meeting(self.data.meetings[i].summary.path.clone()))
            }
        }
    }

    fn position_of(&self, key: &Key) -> Option<usize> {
        match key {
            Key::Dictation(at, text) => self.visible_dictations().iter().position(|&i| {
                let e = &self.data.dictations[i];
                e.at == *at && e.text == *text
            }),
            Key::Meeting(path) => self
                .visible_meetings()
                .iter()
                .position(|&i| self.data.meetings[i].summary.path == *path),
        }
    }

    /// Run `change`, then put each tab's selection back on the item it was
    /// on, or clamp it when that item is gone.
    fn keeping_selection(&mut self, change: impl FnOnce(&mut App)) {
        let before = [self.key_of(Tab::History), self.key_of(Tab::Meetings)];
        change(self);
        for tab in [Tab::History, Tab::Meetings] {
            let i = tab.index();
            let len = self.visible_len(tab);
            let found = before[i].as_ref().and_then(|k| self.position_of(k));
            let moved = found != Some(self.sel[i]);
            self.sel[i] = match found {
                Some(p) => p,
                None if len == 0 => 0,
                None => self.sel[i].min(len - 1),
            };
            if moved && tab == self.tab {
                self.scroll = 0;
            }
        }
    }

    /// New data from disk (a dictation was appended, a meeting grew).
    pub fn set_data(&mut self, data: Data) {
        self.keeping_selection(|app| app.data = data);
    }

    fn set_query(&mut self, query: String) {
        self.keeping_selection(|app| app.query = query);
    }

    fn select(&mut self, pos: usize) {
        let len = self.visible_len(self.tab);
        let pos = if len == 0 { 0 } else { pos.min(len - 1) };
        if pos != self.sel[self.tab.index()] {
            self.sel[self.tab.index()] = pos;
            self.scroll = 0;
        }
    }

    fn move_by(&mut self, delta: isize) {
        let pos = self.selected().saturating_add_signed(delta);
        self.select(pos);
    }

    fn switch_tab(&mut self) {
        self.tab = self.tab.other();
        self.scroll = 0;
    }

    fn scroll_by(&mut self, delta: i32) {
        let max = self.preview_lines.saturating_sub(self.preview_height);
        let next = (self.scroll as i32 + delta).clamp(0, max as i32);
        self.scroll = next as u16;
    }

    pub fn flash(&mut self, message: impl Into<String>) {
        self.flash = Some((message.into(), Instant::now()));
    }

    /// The flash message while it is fresh.
    pub fn flash_text(&self) -> Option<&str> {
        self.flash
            .as_ref()
            .filter(|(_, at)| at.elapsed() < FLASH_FOR)
            .map(|(m, _)| m.as_str())
    }

    /// What `enter` copies: a dictation's text, or a meeting's transcript.
    fn copy_target(&self) -> Option<String> {
        match self.tab {
            Tab::History => self.selected_dictation().map(|e| e.text.clone()),
            Tab::Meetings => self.selected_meeting().map(|m| m.body.trim().to_string()),
        }
    }

    /// What `o` opens: the meeting file, or the day file a dictation is in.
    fn edit_target(&self) -> Option<PathBuf> {
        match self.tab {
            Tab::History => self
                .selected_dictation()
                .map(|e| self.store.day_path(e.at.date())),
            Tab::Meetings => self.selected_meeting().map(|m| m.summary.path.clone()),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return Effect::Quit;
        }
        let half = (self.preview_height / 2).max(1) as i32;
        match key.code {
            KeyCode::Char('d') if ctrl => self.scroll_by(half),
            KeyCode::Char('u') if ctrl => self.scroll_by(-half),
            KeyCode::PageDown => self.scroll_by(half),
            KeyCode::PageUp => self.scroll_by(-half),
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::Tab | KeyCode::BackTab => self.switch_tab(),
            _ if self.searching => return self.search_key(key),
            _ => return self.normal_key(key),
        }
        Effect::None
    }

    fn search_key(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Enter => self.searching = false,
            KeyCode::Esc => {
                self.searching = false;
                self.set_query(String::new());
            }
            KeyCode::Backspace => {
                let mut q = self.query.clone();
                q.pop();
                self.set_query(q);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let mut q = self.query.clone();
                q.push(c);
                self.set_query(q);
            }
            _ => {}
        }
        Effect::None
    }

    fn normal_key(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Char('q') => return Effect::Quit,
            KeyCode::Esc if self.query.is_empty() => return Effect::Quit,
            KeyCode::Esc => self.set_query(String::new()),
            KeyCode::Char('j') => self.move_by(1),
            KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Char('g') | KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => self.select(usize::MAX),
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Char('r') => return Effect::Reload,
            KeyCode::Enter => {
                if let Some(text) = self.copy_target() {
                    return Effect::Copy(text);
                }
            }
            KeyCode::Char('o') => {
                if let Some(path) = self.edit_target() {
                    return Effect::Edit(path);
                }
            }
            _ => {}
        }
        Effect::None
    }
}

fn terms(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// Every term appears in at least one field, case-insensitively.
fn matches_all(terms: &[String], fields: &[&str]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let fields: Vec<String> = fields.iter().map(|f| f.to_lowercase()).collect();
    terms
        .iter()
        .all(|t| fields.iter().any(|f| f.contains(t.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::data::tests::fixture_data;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn app() -> App {
        let dir = std::path::Path::new("/tmp/squawk-test");
        let store = Store::new(&squawk_core::Paths::under(dir));
        App::new(
            store,
            fixture_data(),
            NaiveDate::from_ymd_opt(2026, 9, 29).unwrap(),
        )
    }

    fn typed(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn moves_and_clamps() {
        let mut a = app();
        let n = a.visible_dictations().len();
        assert_eq!(a.selected(), 0);
        a.handle_key(key(KeyCode::Char('k')));
        assert_eq!(a.selected(), 0);
        a.handle_key(key(KeyCode::Char('j')));
        a.handle_key(key(KeyCode::Down));
        assert_eq!(a.selected(), 2);
        a.handle_key(key(KeyCode::Char('G')));
        assert_eq!(a.selected(), n - 1);
        a.handle_key(key(KeyCode::Down));
        assert_eq!(a.selected(), n - 1);
        a.handle_key(key(KeyCode::Char('g')));
        assert_eq!(a.selected(), 0);
    }

    #[test]
    fn tabs_keep_their_own_selection() {
        let mut a = app();
        a.handle_key(key(KeyCode::Char('j')));
        a.handle_key(key(KeyCode::Tab));
        assert_eq!(a.tab, Tab::Meetings);
        assert_eq!(a.selected(), 0);
        a.handle_key(key(KeyCode::Char('j')));
        a.handle_key(key(KeyCode::BackTab));
        assert_eq!(a.tab, Tab::History);
        assert_eq!(a.selected(), 1);
        a.handle_key(key(KeyCode::Tab));
        assert_eq!(a.selected(), 1);
    }

    #[test]
    fn search_filters_case_insensitively_over_every_field() {
        let mut a = app();
        a.handle_key(key(KeyCode::Char('/')));
        assert!(a.searching);
        typed(&mut a, "RESAMPLER");
        assert_eq!(a.visible_dictations().len(), 1);
        a.handle_key(key(KeyCode::Esc));
        assert!(!a.searching);
        assert!(a.query.is_empty());

        a.handle_key(key(KeyCode::Char('/')));
        typed(&mut a, "safari");
        assert_eq!(a.visible_dictations().len(), 1);
        assert_eq!(a.selected_dictation().unwrap().app, "Safari");

        // several terms must all match, across fields
        a.handle_key(key(KeyCode::Esc));
        a.handle_key(key(KeyCode::Char('/')));
        typed(&mut a, "ghostty tests");
        let vis = a.visible_dictations();
        assert_eq!(vis.len(), 1);
        assert!(a.data.dictations[vis[0]].text.contains("tests"));
    }

    #[test]
    fn search_covers_meeting_transcripts() {
        let mut a = app();
        a.handle_key(key(KeyCode::Tab));
        a.handle_key(key(KeyCode::Char('/')));
        typed(&mut a, "loud and clear");
        assert_eq!(a.visible_meetings().len(), 1);
        assert_eq!(a.selected_meeting().unwrap().summary.title, "Weekly sync");
    }

    #[test]
    fn enter_keeps_the_filter_and_esc_then_clears_it() {
        let mut a = app();
        a.handle_key(key(KeyCode::Char('/')));
        typed(&mut a, "safari");
        a.handle_key(key(KeyCode::Enter));
        assert!(!a.searching);
        assert_eq!(a.query, "safari");
        // j/k move again once the filter is kept
        assert_eq!(a.handle_key(key(KeyCode::Char('q'))), Effect::Quit);
        a.handle_key(key(KeyCode::Esc));
        assert!(a.query.is_empty());
        assert_eq!(a.handle_key(key(KeyCode::Esc)), Effect::Quit);
    }

    #[test]
    fn selection_follows_its_item_across_filter_changes() {
        let mut a = app();
        // select the Safari entry (third newest), then filter to it and back
        a.handle_key(key(KeyCode::Char('j')));
        a.handle_key(key(KeyCode::Char('j')));
        let chosen = a.selected_dictation().unwrap().clone();
        a.handle_key(key(KeyCode::Char('/')));
        typed(&mut a, "a");
        assert_eq!(a.selected_dictation(), Some(&chosen));
        a.handle_key(key(KeyCode::Esc));
        assert_eq!(a.selected_dictation(), Some(&chosen));
        assert_eq!(a.selected(), 2);
    }

    #[test]
    fn selection_clamps_when_its_item_is_filtered_out() {
        let mut a = app();
        a.handle_key(key(KeyCode::Char('G')));
        a.handle_key(key(KeyCode::Char('/')));
        typed(&mut a, "zzz-nothing");
        assert_eq!(a.visible_dictations().len(), 0);
        assert_eq!(a.selected(), 0);
        assert_eq!(a.selected_dictation(), None);
        assert_eq!(a.handle_key(key(KeyCode::Enter)), Effect::None);
        a.handle_key(key(KeyCode::Backspace));
        assert!(a.visible_dictations().len() <= a.data.dictations.len());
        assert!(a.selected() < a.visible_dictations().len().max(1));
    }

    #[test]
    fn reload_keeps_the_selected_item() {
        let mut a = app();
        a.handle_key(key(KeyCode::Char('j')));
        let chosen = a.selected_dictation().unwrap().clone();
        let mut data = fixture_data();
        let mut newer = chosen.clone();
        newer.at += chrono::Duration::hours(3);
        newer.text = "A brand new dictation.".into();
        data.dictations.insert(0, newer);
        a.set_data(data);
        assert_eq!(a.selected_dictation(), Some(&chosen));
        assert_eq!(a.selected(), 2);
    }

    #[test]
    fn enter_copies_and_o_opens() {
        let mut a = app();
        let text = a.selected_dictation().unwrap().text.clone();
        assert_eq!(a.handle_key(key(KeyCode::Enter)), Effect::Copy(text));
        match a.handle_key(key(KeyCode::Char('o'))) {
            Effect::Edit(p) => assert!(p.ends_with("dictations/2026-09-29.md")),
            other => panic!("{other:?}"),
        }
        a.handle_key(key(KeyCode::Tab));
        match a.handle_key(key(KeyCode::Enter)) {
            Effect::Copy(t) => {
                assert!(t.starts_with("# Standup\n"), "{t}");
                assert!(t.contains("**You** 00:00:02\nQuick one today."), "{t}");
            }
            other => panic!("{other:?}"),
        }
        let path = a.selected_meeting().unwrap().summary.path.clone();
        assert_eq!(a.handle_key(key(KeyCode::Char('o'))), Effect::Edit(path));
    }

    #[test]
    fn typing_in_search_does_not_trigger_keys() {
        let mut a = app();
        a.handle_key(key(KeyCode::Char('/')));
        assert_eq!(a.handle_key(key(KeyCode::Char('q'))), Effect::None);
        assert_eq!(a.handle_key(key(KeyCode::Char('o'))), Effect::None);
        assert_eq!(a.query, "qo");
        assert_eq!(a.handle_key(ctrl('c')), Effect::Quit);
    }

    #[test]
    fn preview_scroll_clamps_and_resets_on_move() {
        let mut a = app();
        a.preview_lines = 50;
        a.preview_height = 10;
        a.handle_key(ctrl('d'));
        assert_eq!(a.scroll, 5);
        for _ in 0..20 {
            a.handle_key(ctrl('d'));
        }
        assert_eq!(a.scroll, 40);
        a.handle_key(ctrl('u'));
        assert_eq!(a.scroll, 35);
        a.handle_key(key(KeyCode::Char('j')));
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn r_reloads() {
        let mut a = app();
        assert_eq!(a.handle_key(key(KeyCode::Char('r'))), Effect::Reload);
    }

    #[test]
    fn flash_expires() {
        let mut a = app();
        a.flash("Copied");
        assert_eq!(a.flash_text(), Some("Copied"));
        a.flash = Some(("old".into(), Instant::now() - FLASH_FOR));
        assert_eq!(a.flash_text(), None);
    }
}
