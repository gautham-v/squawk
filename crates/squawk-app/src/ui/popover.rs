//! Root popover view.
//!
//! Shaped like claudebar's: a system-menu material, 5 px of inset around
//! plain rows, hairline separators, ink only — copper appears just on the
//! state line while recording or in a meeting. Top to bottom:
//! - header: one state line from [`format::header`] ("Ready · fn to talk",
//!   "Recording 0:07", "Downloading model 42%" with a hairline progress bar,
//!   "Needs Accessibility" with an Open Settings button), then the last error
//!   or config note as a muted line; then, until fixed or hidden, a hint to
//!   set Keyboard › Press 🌐 key to › Do nothing;
//! - tabs: History | Meetings | Dictionary | Settings (text tabs, the
//!   selected one in primary ink);
//! - body (scrolls): History = the 50 most recent dictations, "app ·
//!   project" and time, then the text clamped to 2 lines; click copies.
//!   Meetings = the next meeting pinned on top ("Next · in 25 min", its
//!   title, "14:30–15:00 · Google Meet · will record", then a ready line:
//!   ✓ Calendar ✓ Call detection ✓ System audio; "Now" while one records),
//!   then title, date · length per meeting; click opens the file. Dictionary =
//!   "Edit dictionary.txt" pinned above the list (opens the file), then one
//!   line per entry, a replacement as "spoken → written". Settings = the
//!   notetaker's `[meeting]` settings, then "Edit config.toml"
//!   (`ui::settings`);
//! - footer: Record meeting ⌥M, Launch at login, Quit.
//!
//! Keys (`ui::nav`): ← → switch tabs, ↑ ↓ select a row, Enter copies the
//! dictation / opens the meeting / opens dictionary.txt, Esc closes.
//!
//! The data comes from the files through [`PopoverData::load`], read on the
//! background executor at startup, whenever the snapshot's `revision` moves
//! and each time the popover opens (the files can change behind the app's
//! back). The view keeps the last read in memory, so opening or scrolling
//! never waits on the disk; the preview example feeds fixture data instead.

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::Local;
use gpui::prelude::FluentBuilder;
use gpui::{
    actions, div, px, relative, uniform_list, App, Context, Div, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement, IntoElement, KeyBinding, ParentElement, Point,
    Render, Rgba, ScrollHandle, ScrollStrategy, SharedString, StatefulInteractiveElement, Styled,
    UniformListScrollHandle, Window,
};
use squawk_core::dictionary::{Dictionary, Entry, DICTIONARY_HEADER};
use squawk_core::notetaker::Setting;
use squawk_core::store::{DictationEntry, MeetingSummary};
use squawk_core::{Config, Paths, Store};

use crate::controller::Snapshot;
use crate::launch_at_login;
use crate::permissions::{self, Pane};
use crate::ui::format::{self, Tone};
use crate::ui::nav::{self, Enter, Step};
use crate::ui::settings::{self, Menu};
use crate::ui::theme::{self, Theme};

/// How many dictations the History tab lists.
pub const HISTORY_ROWS: usize = 50;
/// How long "Copied" replaces a row's time.
const COPIED_FOR: Duration = Duration::from_millis(1200);
/// The marker file that hides the fn-key hint for good.
const HINT_DISMISSED_FILE: &str = "fn-hint-dismissed";

/// Which tab is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    History,
    Meetings,
    Dictionary,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::History, Tab::Meetings, Tab::Dictionary, Tab::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Tab::History => "History",
            Tab::Meetings => "Meetings",
            Tab::Dictionary => "Dictionary",
            Tab::Settings => "Settings",
        }
    }
}

/// What the popover asks its owner (main.rs) to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopoverEvent {
    Close,
    ToggleMeeting,
    /// A Settings-tab change, to be written to config.toml.
    SetSetting(Setting),
}

actions!(
    squawk,
    [Dismiss, SelectUp, SelectDown, TabLeft, TabRight, Activate]
);

/// Key context for the popover.
pub const KEY_CONTEXT: &str = "Squawk";

/// Install the popover's key bindings. Call once at app start.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT)),
        KeyBinding::new("up", SelectUp, Some(KEY_CONTEXT)),
        KeyBinding::new("down", SelectDown, Some(KEY_CONTEXT)),
        KeyBinding::new("left", TabLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("right", TabRight, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", Activate, Some(KEY_CONTEXT)),
    ]);
}

/// What the three tabs list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PopoverData {
    pub history: Vec<DictationEntry>,
    pub meetings: Vec<MeetingSummary>,
    pub dictionary: Vec<Entry>,
    /// A file that could not be read, shown in place of its list.
    pub error: Option<String>,
}

impl PopoverData {
    /// Read everything from the files. Small: 50 entries, the meeting
    /// headers, one dictionary.
    pub fn load(paths: &Paths) -> PopoverData {
        let store = Store::new(paths);
        let mut error = None;
        let history = store.recent(HISTORY_ROWS).unwrap_or_else(|e| {
            error = Some(format!("could not read dictations: {e}"));
            Vec::new()
        });
        let meetings = store.meetings().unwrap_or_else(|e| {
            error = Some(format!("could not read meetings: {e}"));
            Vec::new()
        });
        let dictionary = Dictionary::load(&paths.dictionary_file)
            .map(|d| d.entries().to_vec())
            .unwrap_or_default();
        PopoverData {
            history,
            meetings,
            dictionary,
            error,
        }
    }
}

pub struct Popover {
    focus: FocusHandle,
    tab: Tab,
    snapshot: Snapshot,
    data: PopoverData,
    /// Read the files again (`None` in the preview: fixture data).
    live_files: bool,
    /// Bumped by each background read, so only the newest one lands.
    load_seq: u64,
    /// The History row showing "Copied", and when it was clicked.
    copied: Option<(usize, Instant)>,
    /// The row ↑/↓ selected in the current tab's list.
    selected: Option<usize>,
    /// The Settings tab's open pop-up menu.
    open_menu: Option<Menu>,
    /// Whether the fn key is set to something that fights squawk.
    fn_hint: bool,
    login_error: Option<SharedString>,
    /// The list's scroll position: back to the top on open and on a tab
    /// switch, so the newest entries are what shows.
    list_scroll: ScrollHandle,
    /// The same for the Dictionary list, which is virtualized: it can run
    /// to thousands of entries, and only the visible rows are laid out.
    dictionary_scroll: UniformListScrollHandle,
    theme: Theme,
    appearance: Option<gpui::Subscription>,
}

impl Popover {
    /// A popover over the real files.
    pub fn new(snapshot: Snapshot, cx: &mut Context<Self>) -> Popover {
        Popover {
            focus: cx.focus_handle(),
            tab: Tab::default(),
            snapshot,
            data: PopoverData::default(),
            live_files: true,
            load_seq: 0,
            copied: None,
            selected: None,
            open_menu: None,
            fn_hint: false,
            login_error: None,
            list_scroll: ScrollHandle::new(),
            dictionary_scroll: UniformListScrollHandle::new(),
            theme: Theme::default(),
            appearance: None,
        }
    }

    /// A popover over fixture data (the preview example).
    pub fn with_data(
        snapshot: Snapshot,
        data: PopoverData,
        tab: Tab,
        fn_hint: bool,
        cx: &mut Context<Self>,
    ) -> Popover {
        let mut popover = Popover::new(snapshot, cx);
        popover.data = data;
        popover.live_files = false;
        popover.tab = tab;
        popover.fn_hint = fn_hint;
        popover
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Called every time the popover opens. Shows what is already in
    /// memory straight away; re-reading the files, the fn-key setting
    /// (`defaults` takes a few ms) and the launch-at-login status (an XPC
    /// round trip) all happen in the background and land when ready.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.copied = None;
        self.selected = None;
        self.open_menu = None;
        self.login_error = None;
        self.scroll_to_top();
        cx.notify();
        if !self.live_files {
            return;
        }
        self.reload(cx);
        let dismissed = self.hint_dismissed_path();
        cx.spawn(async move |this, cx| {
            let fn_hint = cx
                .background_executor()
                .spawn(async move { !dismissed.exists() && !permissions::fn_key_does_nothing() })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.fn_hint != fn_hint {
                    this.fn_hint = fn_hint;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let before = launch_at_login::is_enabled();
            let now = cx
                .background_executor()
                .spawn(async { launch_at_login::refresh() })
                .await;
            if now != before {
                let _ = this.update(cx, |_, cx| cx.notify());
            }
        })
        .detach();
    }

    /// A new snapshot from the controller.
    pub fn set_snapshot(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        let files_changed =
            snapshot.revision != self.snapshot.revision || snapshot.paths != self.snapshot.paths;
        self.snapshot = snapshot;
        if files_changed {
            self.reload(cx);
        }
        cx.notify();
    }

    /// Read the files on the background executor and swap the result in
    /// when it lands (only the newest read counts).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if !self.live_files {
            return;
        }
        self.load_seq += 1;
        let seq = self.load_seq;
        let paths = self.snapshot.paths.clone();
        cx.spawn(async move |this, cx| {
            let data = cx
                .background_executor()
                .spawn(async move { PopoverData::load(&paths) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.load_seq == seq && this.data != data {
                    this.data = data;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn hint_dismissed_path(&self) -> std::path::PathBuf {
        self.snapshot.paths.support_dir.join(HINT_DISMISSED_FILE)
    }

    fn select(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if tab != self.tab {
            self.scroll_to_top();
            self.selected = None;
            self.open_menu = None;
        }
        self.tab = tab;
        cx.notify();
    }

    /// How many rows ↑/↓ move through on this tab.
    fn row_count(&self) -> usize {
        match self.tab {
            Tab::History => self.data.history.len(),
            Tab::Meetings => self.data.meetings.len(),
            Tab::Dictionary => self.data.dictionary.len(),
            Tab::Settings => 0,
        }
    }

    fn step_selection(&mut self, step: Step, cx: &mut Context<Self>) {
        if !self.tab.has_rows() {
            return;
        }
        self.selected = nav::step(self.selected, self.row_count(), step);
        if let Some(index) = self.selected {
            if self.tab == Tab::Dictionary {
                // Virtualized: the row may not be laid out yet, so the list
                // scrolls to it by index. Only as far as it takes, from the
                // side the selection is moving toward.
                let strategy = match step {
                    Step::Up => ScrollStrategy::Top,
                    Step::Down => ScrollStrategy::Bottom,
                };
                self.dictionary_scroll.scroll_to_item(index, strategy);
            } else {
                self.list_scroll.scroll_to_item(index);
            }
        }
        cx.notify();
    }

    fn on_select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(Step::Up, cx);
    }

    fn on_select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(Step::Down, cx);
    }

    fn on_tab_left(&mut self, _: &TabLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.tab.prev(), cx);
    }

    fn on_tab_right(&mut self, _: &TabRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.tab.next(), cx);
    }

    fn on_activate(&mut self, _: &Activate, _: &mut Window, cx: &mut Context<Self>) {
        match nav::enter(self.tab, self.selected) {
            Some(Enter::CopyDictation(index)) => self.copy_row(index, cx),
            Some(Enter::OpenMeeting(index)) => self.open_meeting(index, cx),
            Some(Enter::EditDictionary) => self.edit_dictionary(cx),
            None => {}
        }
    }

    /// Open or close a Settings pop-up.
    pub fn toggle_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.open_menu = if self.open_menu == Some(menu) {
            None
        } else {
            Some(menu)
        };
        cx.notify();
    }

    pub(crate) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.open_menu.take().is_some() {
            cx.notify();
        }
    }

    /// A Settings-tab change: shown at once, written by the owner (the
    /// controller writes config.toml and reloads).
    pub(crate) fn change_setting(&mut self, setting: Setting, cx: &mut Context<Self>) {
        self.open_menu = None;
        setting.apply(&mut self.snapshot.meeting_config);
        cx.emit(PopoverEvent::SetSetting(setting));
        cx.notify();
    }

    fn scroll_to_top(&self) {
        self.list_scroll.set_offset(Point::default());
        let dictionary = self.dictionary_scroll.0.borrow();
        dictionary.base_handle.set_offset(Point::default());
    }

    /// Scroll the list to its end (the preview uses it to show the bottom
    /// of a long list).
    pub fn scroll_to_end(&mut self, cx: &mut Context<Self>) {
        self.list_scroll.scroll_to_bottom();
        if let Some(last) = self.data.dictionary.len().checked_sub(1) {
            self.dictionary_scroll
                .scroll_to_item(last, ScrollStrategy::Bottom);
        }
        cx.notify();
    }

    fn copy_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.data.history.get(index) else {
            return;
        };
        crate::paste::copy(&entry.text);
        let clicked = Instant::now();
        self.copied = Some((index, clicked));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            let _ = this.update(cx, |this, cx| {
                if this.copied.is_some_and(|(_, at)| at == clicked) {
                    this.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn open_settings(&mut self, cx: &mut Context<Self>) {
        let path = self.snapshot.paths.config_file.clone();
        if let Err(e) = Config::write_default_if_missing(&path) {
            log::warn!("could not write {}: {e}", path.display());
        }
        open_in_editor(&path);
        cx.emit(PopoverEvent::Close);
    }

    fn edit_dictionary(&mut self, cx: &mut Context<Self>) {
        let path = self.snapshot.paths.dictionary_file.clone();
        if !path.exists() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, DICTIONARY_HEADER);
        }
        open_in_editor(&path);
        cx.emit(PopoverEvent::Close);
    }

    fn open_meeting(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(meeting) = self.data.meetings.get(index) {
            let _ = std::process::Command::new("/usr/bin/open")
                .arg(&meeting.path)
                .spawn();
            cx.emit(PopoverEvent::Close);
        }
    }

    fn hide_hint(&mut self, cx: &mut Context<Self>) {
        let path = self.hint_dismissed_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, "");
        self.fn_hint = false;
        cx.notify();
    }

    /// `SMAppService` register/unregister is a slow XPC round trip: do it
    /// in the background and show the result when it lands.
    fn toggle_launch_at_login(&mut self, cx: &mut Context<Self>) {
        let wanted = !launch_at_login::is_enabled();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { launch_at_login::set_enabled(wanted) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.login_error = result.err().map(Into::into);
                cx.notify();
            });
        })
        .detach();
    }

    fn on_dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        if self.open_menu.is_some() {
            self.close_menu(cx);
        } else {
            cx.emit(PopoverEvent::Close);
        }
    }

    // ── Layout ──────────────────────────────────────────────────────────────

    fn tone_color(&self, tone: Tone) -> Rgba {
        match tone {
            Tone::Primary => self.theme.text,
            Tone::Accent => self.theme.accent,
            Tone::Secondary => self.theme.secondary,
        }
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let theme = self.theme;
        let header = format::header(&self.snapshot, Instant::now());
        let state_line = div()
            .flex()
            .flex_row()
            .justify_between()
            .items_center()
            .child(
                div()
                    .text_size(theme::TEXT_BODY)
                    .line_height(theme::LINE_BODY)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(self.tone_color(header.tone))
                    .child(header.text),
            )
            .children(header.fix.map(|pane| self.fix_button(pane, cx)));

        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(theme::ROW_PAD_X)
            .pt(px(4.))
            .pb(px(2.))
            .child(state_line)
            .children(header.progress.map(|p| {
                div()
                    .w_full()
                    .h(theme::PROGRESS_HEIGHT)
                    .rounded(theme::PROGRESS_HEIGHT)
                    .bg(theme.separator)
                    .overflow_hidden()
                    .child(div().w(relative(p.clamp(0.0, 1.0))).h_full().bg(theme.text))
            }))
            .children(header.note.map(|note| {
                div()
                    .text_size(theme::TEXT_TINY)
                    .line_height(theme::LINE_TINY)
                    .text_color(theme.secondary)
                    .line_clamp(2)
                    .child(note)
            }))
    }

    fn fix_button(&self, pane: Pane, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        div()
            .id(SharedString::from(format!("fix-{pane:?}")))
            .px(px(8.))
            .rounded(theme::ROW_RADIUS)
            .border_1()
            .border_color(theme.separator)
            .text_size(theme::TEXT_TINY)
            .line_height(theme::LINE_SMALL)
            .cursor_pointer()
            .hover(|s| s.bg(theme.hover))
            .on_click(cx.listener(move |_, _, _, _| permissions::open_pane(pane)))
            .child(format::FIX_LABEL)
    }

    fn hint(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.fn_hint {
            return None;
        }
        let theme = self.theme;
        Some(
            div().flex().flex_col().child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .items_center()
                    .px(theme::ROW_PAD_X)
                    .pb(px(4.))
                    .text_size(theme::TEXT_TINY)
                    .line_height(theme::LINE_TINY)
                    .text_color(theme.tertiary)
                    .child(
                        div()
                            .id("hint")
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme.secondary))
                            .on_click(
                                cx.listener(|_, _, _, _| permissions::open_pane(Pane::Keyboard)),
                            )
                            .child("Set Keyboard › Press 🌐 key to › Do nothing"),
                    )
                    .child(
                        div()
                            .id("hint-hide")
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme.secondary))
                            .on_click(cx.listener(|this, _, _, cx| this.hide_hint(cx)))
                            .child("Hide"),
                    ),
            ),
        )
    }

    fn tabs(&self, cx: &mut Context<Self>) -> Div {
        let theme = self.theme;
        div()
            .flex()
            .flex_row()
            .gap(theme::TAB_GAP)
            .px(theme::ROW_PAD_X)
            .pb(px(4.))
            .text_size(theme::TEXT_SMALL)
            .line_height(theme::LINE_SMALL)
            .children(Tab::ALL.map(|tab| {
                let selected = tab == self.tab;
                div()
                    .id(tab.label())
                    .cursor_pointer()
                    .text_color(if selected {
                        theme.text
                    } else {
                        theme.secondary
                    })
                    .when(selected, |el| el.font_weight(FontWeight::MEDIUM))
                    .when(!selected, |el| el.hover(|s| s.text_color(theme.text)))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(tab, cx)))
                    .child(tab.label())
            }))
    }

    /// The tab's list. It takes whatever height is left between the tabs
    /// and the footer and scrolls inside it; a tab may pin a row above the
    /// scrolling part (Meetings' next meeting, Dictionary's "Edit
    /// dictionary.txt").
    fn body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (pinned, rows): (Option<gpui::AnyElement>, Vec<gpui::AnyElement>) = match self.tab {
            Tab::History => (None, self.history_rows(cx)),
            Tab::Meetings => (Some(self.next_meeting_row(cx)), self.meeting_rows(cx)),
            Tab::Dictionary => (Some(self.dictionary_edit_row(cx)), Vec::new()),
            Tab::Settings => (
                None,
                settings::body(
                    self.theme,
                    &self.snapshot.meeting_config,
                    self.snapshot.calendar_access,
                    self.open_menu,
                    cx,
                ),
            ),
        };
        let list = if self.tab == Tab::Dictionary && !self.data.dictionary.is_empty() {
            self.dictionary_list(cx)
        } else if self.tab == Tab::Dictionary {
            self.empty(EMPTY_DICTIONARY)
        } else {
            div()
                .id("list")
                .track_scroll(&self.list_scroll)
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .children(rows)
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .children(pinned)
            .child(list)
    }

    fn empty(&self, line: &'static str) -> gpui::AnyElement {
        div()
            .flex_shrink_0()
            .px(theme::ROW_PAD_X)
            .py(theme::LIST_ROW_PAD_Y)
            .text_size(theme::TEXT_SMALL)
            .line_height(theme::LINE_SMALL)
            .text_color(self.theme.tertiary)
            .child(line)
            .into_any_element()
    }

    fn history_rows(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        if self.data.history.is_empty() {
            return vec![self.empty(
                self.data
                    .error
                    .as_deref()
                    .map_or(EMPTY_HISTORY, |_| NO_FILES),
            )];
        }
        let theme = self.theme;
        let today = Local::now().date_naive();
        self.data
            .history
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let copied = self.copied.is_some_and(|(i, _)| i == index);
                let time = if copied {
                    "Copied".to_string()
                } else {
                    format::row_time(entry.at, today)
                };
                list_row(theme, SharedString::from(format!("history-{index}")))
                    .when(self.selected == Some(index), |el| el.bg(theme.hover))
                    .on_click(cx.listener(move |this, _, _, cx| this.copy_row(index, cx)))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_between()
                            .gap(px(8.))
                            .text_size(theme::TEXT_TINY)
                            .line_height(theme::LINE_TINY)
                            .child(
                                div()
                                    .text_color(theme.secondary)
                                    .truncate()
                                    .child(entry.source()),
                            )
                            .child(div().flex_shrink_0().text_color(theme.tertiary).child(time)),
                    )
                    .child(
                        div()
                            .text_size(theme::TEXT_SMALL)
                            .line_height(theme::LINE_SMALL)
                            .text_ellipsis()
                            .line_clamp(2)
                            .child(entry.text.clone()),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// The next meeting (or the one recording), pinned above the list: a
    /// small label line, the title, a meta line, then the ready line and a
    /// separator. From the snapshot only: the controller reads the
    /// calendar.
    fn next_meeting_row(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let row = format::next_row(&self.snapshot, Instant::now(), Local::now());
        let small = |text: SharedString| {
            div()
                .text_size(theme::TEXT_TINY)
                .line_height(theme::LINE_TINY)
                .child(text)
        };
        let label_line = |label: &'static str, when: String, live: bool| {
            div()
                .flex()
                .flex_row()
                .justify_between()
                .gap(px(8.))
                .text_color(theme.tertiary)
                .child(small(label.into()).when(live, |el| el.text_color(theme.accent)))
                .child(small(when.into()))
        };
        let title = |text: String| {
            div()
                .text_size(theme::TEXT_BODY)
                .line_height(theme::LINE_BODY)
                .truncate()
                .child(text)
        };
        let block = div()
            .flex()
            .flex_col()
            .gap(px(1.))
            .px(theme::ROW_PAD_X)
            .pt(px(4.))
            .pb(px(2.));
        let block = match row {
            format::NextRow::Now { title: t, meta } => block
                .child(label_line(format::NOW_LABEL, String::new(), true))
                .child(title(t))
                .child(small(meta.into()).text_color(theme.accent)),
            format::NextRow::Next {
                when,
                title: t,
                meta,
            } => block
                .child(label_line(format::NEXT_LABEL, when, false))
                .child(title(t))
                .child(small(meta.into()).text_color(theme.secondary)),
            format::NextRow::Line { text, note, fix } => block
                .child(label_line(format::NEXT_LABEL, String::new(), false))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w(px(0.))
                                .child(
                                    div()
                                        .text_size(theme::TEXT_SMALL)
                                        .line_height(theme::LINE_SMALL)
                                        .text_color(if fix.is_some() {
                                            theme.text
                                        } else {
                                            theme.tertiary
                                        })
                                        .child(text),
                                )
                                .children(
                                    note.map(|n| small(n.into()).text_color(theme.secondary)),
                                ),
                        )
                        .children(fix.map(|pane| self.fix_button(pane, cx))),
                ),
        };
        let checks = format::ready_checks(&self.snapshot);
        let ready = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_x(px(8.))
            .px(theme::ROW_PAD_X)
            .pt(px(2.))
            .children(checks.into_iter().map(|(name, ok)| {
                small(format::ready_check(name, ok).into()).text_color(if ok {
                    theme.tertiary
                } else {
                    theme.secondary
                })
            }));
        div()
            .id("next-meeting")
            .flex()
            .flex_col()
            .flex_shrink_0()
            .child(block)
            .child(ready)
            .child(separator(theme))
            .into_any_element()
    }

    fn meeting_rows(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        if self.data.meetings.is_empty() {
            return vec![self.empty(EMPTY_MEETINGS)];
        }
        let theme = self.theme;
        self.data
            .meetings
            .iter()
            .enumerate()
            .map(|(index, meeting)| {
                list_row(theme, SharedString::from(format!("meeting-{index}")))
                    .when(self.selected == Some(index), |el| el.bg(theme.hover))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_meeting(index, cx)))
                    .child(
                        div()
                            .text_size(theme::TEXT_BODY)
                            .line_height(theme::LINE_BODY)
                            .truncate()
                            .child(meeting.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(theme::TEXT_TINY)
                            .line_height(theme::LINE_TINY)
                            .text_color(if meeting.in_progress {
                                theme.accent
                            } else {
                                theme.secondary
                            })
                            .child(format::meeting_meta(meeting)),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// "Edit dictionary.txt", with the count on the right. Pinned above
    /// the list so it stays in reach however long the dictionary gets.
    fn dictionary_edit_row(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let count = self.data.dictionary.len();
        div()
            .id("dictionary-edit")
            .flex()
            .flex_row()
            .flex_shrink_0()
            .justify_between()
            .items_center()
            .px(theme::ROW_PAD_X)
            .py(theme::LIST_ROW_PAD_Y)
            .rounded(theme::ROW_RADIUS)
            .text_size(theme::TEXT_SMALL)
            .line_height(theme::LINE_SMALL)
            .text_color(theme.secondary)
            .cursor_pointer()
            .hover(move |s| s.bg(theme.hover).text_color(theme.text))
            .on_click(cx.listener(|this, _, _, cx| this.edit_dictionary(cx)))
            .child("Edit dictionary.txt")
            .when(count > 0, |el| {
                el.child(
                    div()
                        .flex_shrink_0()
                        .text_size(theme::TEXT_TINY)
                        .line_height(theme::LINE_TINY)
                        .text_color(theme.tertiary)
                        .child(format::dictionary_count(count)),
                )
            })
            .into_any_element()
    }

    /// The entries, one uniform-height row each, laid out only while
    /// visible: scrolling a long dictionary costs the same as a short one.
    fn dictionary_list(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        uniform_list(
            "dictionary",
            self.data.dictionary.len(),
            cx.processor(|this, range: std::ops::Range<usize>, _window, _cx| {
                this.dictionary_rows(range)
            }),
        )
        .track_scroll(self.dictionary_scroll.clone())
        .flex_1()
        .min_h(px(0.))
        .into_any_element()
    }

    /// One line per entry: a term as it is written, a replacement as the
    /// spoken side (muted), an arrow, and the written side. Either side
    /// ends in an ellipsis when the line runs out of room.
    fn dictionary_rows(&self, range: std::ops::Range<usize>) -> Vec<gpui::AnyElement> {
        let theme = self.theme;
        let end = range.end.min(self.data.dictionary.len());
        let start = range.start.min(end);
        self.data.dictionary[start..end]
            .iter()
            .enumerate()
            .map(|(offset, entry)| {
                let index = start + offset;
                // `w_full`: the list lays each row out on its own, and
                // without it a long row sizes to its text and overflows
                // instead of ending in an ellipsis.
                let row = div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .flex_shrink_0()
                    .rounded(theme::ROW_RADIUS)
                    .when(self.selected == Some(index), |el| el.bg(theme.hover))
                    .items_center()
                    .gap(theme::ARROW_GAP)
                    .px(theme::ROW_PAD_X)
                    .py(theme::DICT_ROW_PAD_Y)
                    .text_size(theme::TEXT_SMALL)
                    .line_height(theme::LINE_SMALL);
                match entry {
                    Entry::Term(term) => row.child(clipped(term.clone())),
                    Entry::Replace { from, to } => {
                        // The written side is the one that matters, so the
                        // spoken side gives up its width first.
                        let mut spoken = clipped(from.clone()).text_color(theme.secondary);
                        spoken.style().flex_shrink = Some(theme::SPOKEN_SHRINK);
                        row.child(spoken)
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(theme.tertiary)
                                    .child(format::REPLACE_ARROW),
                            )
                            .child(clipped(to.clone()))
                    }
                }
                .into_any_element()
            })
            .collect()
    }

    fn footer(&self, cx: &mut Context<Self>) -> Div {
        let theme = self.theme;
        let recording = self.snapshot.meeting.is_some();
        let login_available =
            launch_at_login::availability() == launch_at_login::Availability::Ready;
        div()
            .flex()
            .flex_col()
            .child(
                menu_row(
                    theme,
                    "row-meeting",
                    format::meeting_action(recording),
                    true,
                )
                .child(
                    div()
                        .text_color(theme.tertiary)
                        .child(format::MEETING_SHORTCUT),
                )
                .on_click(cx.listener(|_, _, _, cx| cx.emit(PopoverEvent::ToggleMeeting))),
            )
            .child(
                menu_row(theme, "row-login", "Launch at login", login_available)
                    .child(div().child(if launch_at_login::is_enabled() {
                        "\u{2713}"
                    } else {
                        ""
                    }))
                    .when(login_available, |el| {
                        el.on_click(cx.listener(|this, _, _, cx| this.toggle_launch_at_login(cx)))
                    }),
            )
            .when(!login_available, |el| {
                el.child(note(theme, launch_at_login::NO_BUNDLE_NOTE.into()))
            })
            .when_some(self.login_error.clone(), |el, message| {
                el.child(note(theme, message))
            })
            .child(separator(theme))
            .child(
                menu_row(theme, "row-quit", "Quit Squawk", true)
                    .on_click(cx.listener(|_, _, _, cx| cx.quit())),
            )
    }
}

/// A clickable two-line row in the scrolling list. `flex_shrink_0`: the
/// list scrolls, so a row keeps its full height instead of being squeezed
/// to fit.
fn list_row(theme: Theme, id: SharedString) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .flex_shrink_0()
        .gap(px(1.))
        .px(theme::ROW_PAD_X)
        .py(theme::LIST_ROW_PAD_Y)
        .rounded(theme::ROW_RADIUS)
        .cursor_pointer()
        .hover(move |s| s.bg(theme.hover))
}

/// A footer row: a label on the left, room for a shortcut or checkmark on
/// the right, a hover wash like a menu item.
fn menu_row(
    theme: Theme,
    id: &'static str,
    label: &'static str,
    enabled: bool,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .justify_between()
        .items_center()
        .px(theme::ROW_PAD_X)
        .py(theme::ROW_PAD_Y)
        .rounded(theme::ROW_RADIUS)
        .text_size(theme::TEXT_BODY)
        .line_height(theme::LINE_BODY)
        .text_color(if enabled { theme.text } else { theme.tertiary })
        .when(enabled, |el| {
            el.cursor_pointer().hover(move |s| s.bg(theme.hover))
        })
        .child(label)
}

/// One line of text that may give up width to its neighbours in a row and
/// ends in an ellipsis when it does.
///
/// Not `truncate()`: that sets `white-space: nowrap`, and gpui 0.2 caches a
/// nowrap text's first measurement — taken at max-content while the flex
/// row sizes its items — so the text never learns its final width and is
/// clipped mid-letter with no ellipsis. A one-line clamp keeps wrapping on,
/// which re-measures at the width the row settles on.
fn clipped(text: String) -> Div {
    div()
        .min_w(px(0.))
        .overflow_hidden()
        .text_ellipsis()
        .line_clamp(1)
        .child(text)
}

fn note(theme: Theme, message: SharedString) -> impl IntoElement {
    div()
        .px(theme::ROW_PAD_X)
        .pb(px(4.))
        .text_size(theme::TEXT_TINY)
        .line_height(theme::LINE_TINY)
        .text_color(theme.tertiary)
        .child(message)
}

fn separator(theme: Theme) -> Div {
    div()
        .flex_shrink_0()
        .h(theme::HAIRLINE)
        .mx(theme::SEPARATOR_INSET)
        .my(theme::SEPARATOR_MARGIN)
        .bg(theme.separator)
}

/// Open a text file in the user's default text editor.
fn open_in_editor(path: &Path) {
    let _ = std::process::Command::new("/usr/bin/open")
        .arg("-t")
        .arg(path)
        .spawn();
}

const EMPTY_HISTORY: &str = "Nothing yet. Hold fn and talk.";
const EMPTY_MEETINGS: &str = "No meetings yet. ⌥M starts one.";
const EMPTY_DICTIONARY: &str = "No words yet. Add names and jargon squawk should spell your way.";
const NO_FILES: &str = "Could not read your dictations.";

impl EventEmitter<PopoverEvent> for Popover {}

impl Focusable for Popover {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Popover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Follow the system appearance: the window only repaints when
        // notified, so a light/dark flip while open has to wake it.
        if self.appearance.is_none() {
            let this = cx.entity();
            self.appearance = Some(window.observe_window_appearance(move |_window, cx| {
                this.update(cx, |_, cx| cx.notify());
            }));
        }
        self.theme = Theme::for_appearance(window.appearance());
        let theme = self.theme;

        let header = self.header(cx);
        let hint = self.hint(cx);
        let tabs = self.tabs(cx);
        let body = self.body(cx);
        let footer = self.footer(cx);

        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_dismiss))
            .on_action(cx.listener(Self::on_select_up))
            .on_action(cx.listener(Self::on_select_down))
            .on_action(cx.listener(Self::on_tab_left))
            .on_action(cx.listener(Self::on_tab_right))
            .on_action(cx.listener(Self::on_activate))
            .flex()
            .flex_col()
            .w(theme::POPOVER_WIDTH)
            .h_full()
            .p(theme::POPOVER_PAD)
            .bg(theme.bg)
            .rounded(theme::POPOVER_RADIUS)
            .border_1()
            .border_color(theme.border)
            .overflow_hidden()
            .font_family(theme::UI_FAMILY)
            .text_size(theme::TEXT_BODY)
            .text_color(theme.text)
            .child(header)
            .children(hint)
            .child(separator(theme))
            .child(tabs)
            .child(body)
            .child(separator(theme))
            .child(footer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn tabs_are_history_meetings_dictionary_settings() {
        let labels: Vec<_> = Tab::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(labels, ["History", "Meetings", "Dictionary", "Settings"]);
        assert_eq!(Tab::default(), Tab::History);
    }

    #[test]
    fn loading_reads_the_files_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        paths.ensure_dirs().unwrap();
        let store = Store::new(&paths);
        for (i, text) in ["First.", "Second."].iter().enumerate() {
            store
                .append_dictation(&DictationEntry {
                    at: NaiveDate::from_ymd_opt(2026, 9, 29)
                        .unwrap()
                        .and_hms_opt(14, i as u32, 0)
                        .unwrap(),
                    app: "Ghostty".into(),
                    project: Some("demo".into()),
                    text: text.to_string(),
                })
                .unwrap();
        }
        squawk_core::dictionary::add(&paths.dictionary_file, "Kubernetes").unwrap();

        let data = PopoverData::load(&paths);
        assert_eq!(data.error, None);
        let texts: Vec<_> = data.history.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["Second.", "First."]);
        assert_eq!(data.dictionary, [Entry::Term("Kubernetes".into())]);
        assert!(data.meetings.is_empty());
    }

    #[test]
    fn loading_an_empty_data_dir_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let data = PopoverData::load(&Paths::under(dir.path()));
        assert!(data.history.is_empty());
        assert!(data.dictionary.is_empty());
    }
}
