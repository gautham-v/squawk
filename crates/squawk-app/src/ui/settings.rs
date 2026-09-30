//! The popover's Settings tab: the notetaker's four `[meeting]` settings as
//! rows (a label, a muted line, a pop-up or a switch), then "Edit
//! config.toml" for everything else. config.toml stays the source of truth:
//! a change is written to the file (comments and other keys kept) and the
//! app reloads it; the tab shows what the file says.
//!
//! [`rows`] is the pure part (what each row says and holds), tested; the
//! rest draws it.

use gpui::prelude::FluentBuilder;
use gpui::{
    anchored, deferred, div, px, AnyElement, Context, Div, FontWeight, InteractiveElement,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled,
};
use squawk_core::config::MeetingConfig;
use squawk_core::notetaker::{
    heads_up_label, max_length_label, Setting, HEADS_UP_CHOICES, MAX_LENGTH_CHOICES,
};

use crate::permissions::{self, Pane};
use crate::ui::popover::Popover;
use crate::ui::theme::{self, Theme};

/// A row's pop-up menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Menu {
    HeadsUp,
    MaxLength,
}

impl Menu {
    /// The menu's items: label, what choosing it writes, whether it is the
    /// current value.
    pub fn items(self, m: &MeetingConfig) -> Vec<(String, Setting, bool)> {
        match self {
            Menu::HeadsUp => HEADS_UP_CHOICES
                .iter()
                .map(|&s| {
                    let current = s == m.heads_up_secs || (s < 0 && m.heads_up_secs < 0);
                    (heads_up_label(s), Setting::HeadsUpSecs(s), current)
                })
                .collect(),
            Menu::MaxLength => MAX_LENGTH_CHOICES
                .iter()
                .map(|&min| {
                    (
                        max_length_label(min),
                        Setting::MaxMinutes(min),
                        min == m.max_minutes,
                    )
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// A pop-up showing the current value.
    Choice { menu: Menu, value: String },
    /// A switch; clicking writes `toggled`.
    Switch { on: bool, toggled: Setting },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: &'static str,
    pub label: &'static str,
    pub note: String,
    /// The note is a fix: clicking it opens this System Settings pane.
    pub fix: Option<Pane>,
    pub control: Control,
}

pub const SECTION: &str = "Meetings";
pub const EDIT_CONFIG: &str = "Edit config.toml";
pub const EDIT_CONFIG_NOTE: &str = "Dictation, model";

/// The Settings tab's rows for `m`. `calendar` is the Calendar grant
/// (`None` = not asked yet).
pub fn rows(m: &MeetingConfig, calendar: Option<bool>) -> Vec<Row> {
    let heads_up_on = m.heads_up_secs >= 0;
    let calendar_refused = heads_up_on && calendar == Some(false);
    vec![
        Row {
            id: "setting-heads-up",
            label: "Heads-up before meetings",
            note: if calendar_refused {
                "Needs Calendar access · Open Settings".into()
            } else {
                "Calendar events with people or a call link".into()
            },
            fix: calendar_refused.then_some(Pane::Calendars),
            control: Control::Choice {
                menu: Menu::HeadsUp,
                value: heads_up_label(m.heads_up_secs),
            },
        },
        Row {
            id: "setting-detect-calls",
            label: "Detect calls",
            note: "Offer notes when a call app starts using the mic".into(),
            fix: None,
            control: Control::Switch {
                on: m.detect_calls,
                toggled: Setting::DetectCalls(!m.detect_calls),
            },
        },
        Row {
            id: "setting-max-length",
            label: "Maximum recording length",
            note: "Warns 2 min before it stops".into(),
            fix: None,
            control: Control::Choice {
                menu: Menu::MaxLength,
                value: max_length_label(m.max_minutes),
            },
        },
    ]
}

/// Draw the tab. `open` is the pop-up menu showing, if any.
pub fn body(
    theme: Theme,
    m: &MeetingConfig,
    calendar: Option<bool>,
    open: Option<Menu>,
    cx: &mut Context<Popover>,
) -> Vec<AnyElement> {
    let mut out = vec![div()
        .flex_shrink_0()
        .px(theme::ROW_PAD_X)
        .pt(px(4.))
        .pb(px(2.))
        .text_size(theme::TEXT_TINY)
        .line_height(theme::LINE_TINY)
        .text_color(theme.tertiary)
        .child(SECTION)
        .into_any_element()];
    for row in rows(m, calendar) {
        out.push(setting_row(theme, row, m, open, cx));
    }
    out.push(
        div()
            .flex_shrink_0()
            .h(theme::HAIRLINE)
            .mx(theme::SEPARATOR_INSET)
            .my(theme::SEPARATOR_MARGIN)
            .bg(theme.separator)
            .into_any_element(),
    );
    out.push(
        div()
            .id("setting-edit-config")
            .flex()
            .flex_row()
            .flex_shrink_0()
            .justify_between()
            .items_center()
            .px(theme::ROW_PAD_X)
            .py(theme::ROW_PAD_Y)
            .rounded(theme::ROW_RADIUS)
            .text_size(theme::TEXT_SMALL)
            .line_height(theme::LINE_SMALL)
            .text_color(theme.secondary)
            .cursor_pointer()
            .hover(move |s| s.bg(theme.hover).text_color(theme.text))
            .on_click(cx.listener(|this, _, _, cx| this.open_settings(cx)))
            .child(EDIT_CONFIG)
            .child(
                div()
                    .text_size(theme::TEXT_TINY)
                    .text_color(theme.tertiary)
                    .child(EDIT_CONFIG_NOTE),
            )
            .into_any_element(),
    );
    out
}

fn setting_row(
    theme: Theme,
    row: Row,
    m: &MeetingConfig,
    open: Option<Menu>,
    cx: &mut Context<Popover>,
) -> AnyElement {
    let note = div()
        .id(SharedString::from(format!("{}-note", row.id)))
        .text_size(theme::TEXT_TINY)
        .line_height(theme::LINE_TINY)
        .text_color(theme.secondary)
        .child(row.note.clone());
    let note = match row.fix {
        Some(pane) => note
            .cursor_pointer()
            .hover(move |s| s.text_color(theme.text))
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                permissions::open_pane(pane);
            })),
        None => note,
    };
    let label = div()
        .flex()
        .flex_col()
        .min_w(px(0.))
        .child(
            div()
                .text_size(theme::TEXT_BODY)
                .line_height(theme::LINE_BODY)
                .child(row.label),
        )
        .child(note);
    let base = div()
        .id(row.id)
        .flex()
        .flex_row()
        .flex_shrink_0()
        .justify_between()
        .items_center()
        .gap(px(10.))
        .px(theme::ROW_PAD_X)
        .py(theme::LIST_ROW_PAD_Y)
        .rounded(theme::ROW_RADIUS)
        .cursor_pointer()
        .hover(move |s| s.bg(theme.hover))
        .child(label);
    match row.control {
        Control::Switch { on, toggled } => base
            .on_click(cx.listener(move |this, _, _, cx| this.change_setting(toggled, cx)))
            .child(switch(theme, on))
            .into_any_element(),
        Control::Choice { menu, value } => {
            let is_open = open == Some(menu);
            let items = if is_open { menu.items(m) } else { Vec::new() };
            base.on_click(cx.listener(move |this, _, _, cx| this.toggle_menu(menu, cx)))
                .child(
                    div()
                        .relative()
                        .flex_shrink_0()
                        .child(popup_button(theme, value))
                        .when(is_open, |el| {
                            el.child(
                                div().absolute().top(px(22.)).right_0().child(deferred(
                                    anchored()
                                        .anchor(gpui::Corner::TopRight)
                                        .snap_to_window_with_margin(px(6.))
                                        .child(menu_list(theme, items, cx)),
                                )),
                            )
                        }),
                )
                .into_any_element()
        }
    }
}

/// The pop-up button: the value and a small chevron.
fn popup_button(theme: Theme, value: String) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(5.))
        .pl(px(8.))
        .pr(px(6.))
        .rounded(px(5.))
        .border_1()
        .border_color(theme.separator)
        .bg(theme.hover)
        .text_size(theme::TEXT_SMALL)
        .line_height(px(18.))
        .child(value)
        .child(
            div()
                .text_size(px(9.))
                .text_color(theme.secondary)
                .child(theme::POPUP_CHEVRON),
        )
}

/// The open pop-up's choices, a check on the current one.
fn menu_list(
    theme: Theme,
    items: Vec<(String, Setting, bool)>,
    cx: &mut Context<Popover>,
) -> impl IntoElement {
    div()
        .id("setting-menu")
        .occlude()
        .flex()
        .flex_col()
        .min_w(px(112.))
        .p(px(4.))
        .rounded(theme::POPOVER_RADIUS)
        .border_1()
        .border_color(theme.border)
        .bg(theme.menu_bg)
        .shadow_md()
        .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)))
        .children(
            items
                .into_iter()
                .enumerate()
                .map(|(i, (label, setting, current))| {
                    div()
                        .id(("setting-choice", i))
                        .flex()
                        .flex_row()
                        .justify_between()
                        .gap(px(12.))
                        .px(px(8.))
                        .py(px(2.))
                        .rounded(px(5.))
                        .text_size(theme::TEXT_SMALL)
                        .line_height(theme::LINE_SMALL)
                        .cursor_pointer()
                        .hover(move |s| s.bg(theme.hover))
                        .when(current, |el| el.font_weight(FontWeight::MEDIUM))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.change_setting(setting, cx);
                        }))
                        .child(label)
                        .child(div().text_color(theme.secondary).child(if current {
                            "\u{2713}"
                        } else {
                            ""
                        }))
                }),
        )
}

/// A small on/off switch, monochrome.
fn switch(theme: Theme, on: bool) -> Div {
    div()
        .flex_shrink_0()
        .w(theme::SWITCH_WIDTH)
        .h(theme::SWITCH_HEIGHT)
        .rounded(theme::SWITCH_HEIGHT)
        .bg(if on {
            theme.switch_on
        } else {
            theme.switch_off
        })
        .child(
            div()
                .mt(px(2.))
                .ml(if on { px(14.) } else { px(2.) })
                .size(px(12.))
                .rounded(px(6.))
                .bg(if on { theme.knob_on } else { theme.knob_off })
                .shadow_sm(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rows_show_the_file_s_values() {
        let m = MeetingConfig::default();
        let rows = rows(&m, Some(true));
        let labels: Vec<_> = rows.iter().map(|r| r.label).collect();
        assert_eq!(
            labels,
            [
                "Heads-up before meetings",
                "Detect calls",
                "Maximum recording length"
            ]
        );
        assert_eq!(
            rows[0].control,
            Control::Choice {
                menu: Menu::HeadsUp,
                value: "15 s".into()
            }
        );
        assert_eq!(
            rows[1].control,
            Control::Switch {
                on: true,
                toggled: Setting::DetectCalls(false)
            }
        );
        assert_eq!(
            rows[2].control,
            Control::Choice {
                menu: Menu::MaxLength,
                value: "2 h".into()
            }
        );
    }

    #[test]
    fn a_refused_calendar_turns_the_note_into_a_fix_only_when_heads_up_is_on() {
        let mut m = MeetingConfig::default();
        let row = &rows(&m, Some(false))[0];
        assert_eq!(row.fix, Some(Pane::Calendars));
        assert!(row.note.starts_with("Needs Calendar access"));
        assert_eq!(rows(&m, None)[0].fix, None);
        m.heads_up_secs = -1;
        let row = &rows(&m, Some(false))[0];
        assert_eq!(row.fix, None);
        assert_eq!(
            row.control,
            Control::Choice {
                menu: Menu::HeadsUp,
                value: "Off".into()
            }
        );
    }

    #[test]
    fn menus_check_the_current_value() {
        let m = MeetingConfig::default();
        let heads: Vec<_> = Menu::HeadsUp
            .items(&m)
            .into_iter()
            .map(|(label, _, current)| (label, current))
            .collect();
        assert_eq!(
            heads,
            [
                ("Off".to_string(), false),
                ("At start".to_string(), false),
                ("15 s".to_string(), true),
                ("1 min".to_string(), false),
                ("5 min".to_string(), false),
            ]
        );
        let max = Menu::MaxLength.items(&m);
        assert_eq!(max.len(), 5);
        assert!(max[2].2 && max[2].0 == "2 h");
        assert_eq!(max[4].1, Setting::MaxMinutes(240));
    }

    #[test]
    fn a_value_off_the_menu_still_shows_and_checks_nothing() {
        let m = MeetingConfig {
            max_minutes: 90,
            heads_up_secs: 30,
            ..MeetingConfig::default()
        };
        assert!(Menu::MaxLength.items(&m).iter().all(|(_, _, c)| !c));
        assert!(Menu::HeadsUp.items(&m).iter().all(|(_, _, c)| !c));
        let rows = rows(&m, Some(true));
        assert_eq!(
            rows[2].control,
            Control::Choice {
                menu: Menu::MaxLength,
                value: "1 h 30 min".into()
            }
        );
    }
}
