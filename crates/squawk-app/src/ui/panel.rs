//! The notetaker's prompt: a small panel that hangs under the menu bar icon
//! (not a notification: no permission, not silenced by Focus, and it sits
//! where the eye already goes for squawk). Same material and ink as the
//! popover. One title line, one muted line, one or two small buttons:
//!
//! | prompt | title | line | buttons |
//! |---|---|---|---|
//! | heads-up | the event | 15:00–15:30 | Record · Not now |
//! | heads-up, records on its own | the event | 15:00–15:30 · records when the call starts | Skip this one |
//! | call | Call detected in Zoom | Start notes? | Start · Not now |
//! | recording on its own | Recording · Design review | Started with Zoom | Stop |
//! | limit | Stopping in 2 min | Weekly sync · 2 h limit | Keep going +30 min |
//! | saved | Saved notes · Weekly sync | 2:00:00 · reached the time limit | Open |
//!
//! main.rs opens it as a non-activating PopUp window (it never takes focus
//! from the call) whenever the snapshot has a prompt and the popover is
//! closed.

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, Context, EventEmitter, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Pixels, Render, SharedString, StatefulInteractiveElement, Styled, Window,
};
use squawk_core::notetaker::{max_length_label, Prompt, Reply, StopReason, WARN_BEFORE};
use squawk_core::status::format_elapsed;

use crate::ui::theme::{self, Theme};

/// The panel's size.
pub const PANEL_WIDTH_PX: f32 = 292.0;
pub const PANEL_WIDTH: Pixels = px(PANEL_WIDTH_PX);
pub const PANEL_HEIGHT_PX: f32 = 84.0;

/// The words on the panel for a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelText {
    pub title: String,
    pub line: String,
    pub accept: &'static str,
    /// The second button, when there is one.
    pub dismiss: Option<&'static str>,
}

pub fn panel_text(prompt: &Prompt) -> PanelText {
    match prompt {
        Prompt::HeadsUp {
            title,
            start,
            end,
            auto,
            ..
        } => {
            let span = format!("{}–{}", start.format("%H:%M"), end.format("%H:%M"));
            if *auto {
                PanelText {
                    title: title.clone(),
                    line: format!("{span} · records when the call starts"),
                    accept: "Skip this one",
                    dismiss: None,
                }
            } else {
                PanelText {
                    title: title.clone(),
                    line: span,
                    accept: "Record",
                    dismiss: Some("Not now"),
                }
            }
        }
        Prompt::Call { app, .. } => PanelText {
            title: format!("Call detected in {app}"),
            line: "Start notes?".into(),
            accept: "Start",
            dismiss: Some("Not now"),
        },
        Prompt::Recording { title, app } => PanelText {
            title: format!("Recording · {title}"),
            line: format!("Started with {app}"),
            accept: "Stop",
            dismiss: None,
        },
        Prompt::StoppingSoon { title, limit } => PanelText {
            title: format!("Stopping in {} min", WARN_BEFORE.as_secs() / 60),
            line: format!("{title} · {} limit", max_length_label(limit.as_secs() / 60)),
            accept: "Keep going +30 min",
            dismiss: None,
        },
        Prompt::Saved {
            title,
            length_secs,
            reason,
            ..
        } => PanelText {
            title: format!("Saved notes · {title}"),
            line: format!(
                "{} · {}",
                format_elapsed(*length_secs),
                match reason {
                    StopReason::MaxLength => "reached the time limit",
                }
            ),
            accept: "Open",
            dismiss: None,
        },
    }
}

/// A button was pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelReply(pub Reply);

pub struct PromptPanel {
    prompt: Option<Prompt>,
    theme: Theme,
    appearance: Option<gpui::Subscription>,
}

impl PromptPanel {
    pub fn new(prompt: Option<Prompt>) -> PromptPanel {
        PromptPanel {
            prompt,
            theme: Theme::default(),
            appearance: None,
        }
    }

    pub fn set_prompt(&mut self, prompt: Option<Prompt>, cx: &mut Context<Self>) {
        if self.prompt != prompt {
            self.prompt = prompt;
            cx.notify();
        }
    }

    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        primary: bool,
        reply: Reply,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        div()
            .id(id)
            .px(px(9.))
            .rounded(theme::ROW_RADIUS)
            .border_1()
            .border_color(theme.separator)
            .text_size(theme::TEXT_TINY)
            .line_height(px(20.))
            .when(primary, |el| el.font_weight(FontWeight::SEMIBOLD))
            .cursor_pointer()
            .hover(move |s| s.bg(theme.hover))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(PanelReply(reply))))
            .child(label)
    }
}

impl EventEmitter<PanelReply> for PromptPanel {}

impl Render for PromptPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.appearance.is_none() {
            let this = cx.entity();
            self.appearance = Some(window.observe_window_appearance(move |_window, cx| {
                this.update(cx, |_, cx| cx.notify());
            }));
        }
        self.theme = Theme::for_appearance(window.appearance());
        let theme = self.theme;
        let text = self.prompt.as_ref().map(panel_text);
        let root = div()
            .flex()
            .flex_col()
            .size_full()
            .p(theme::POPOVER_PAD)
            .bg(theme.bg)
            .rounded(theme::POPOVER_RADIUS)
            .border_1()
            .border_color(theme.border)
            .overflow_hidden()
            .font_family(theme::UI_FAMILY)
            .text_color(theme.text);
        let Some(text) = text else {
            return root;
        };
        let accept = self.button("panel-accept", text.accept, true, Reply::Accept, cx);
        let dismiss = text
            .dismiss
            .map(|label| self.button("panel-dismiss", label, false, Reply::Dismiss, cx));
        root.child(
            div()
                .flex()
                .flex_col()
                .gap(px(1.))
                .px(theme::ROW_PAD_X)
                .pt(px(4.))
                .pb(px(6.))
                .child(
                    div()
                        .text_size(theme::TEXT_BODY)
                        .line_height(theme::LINE_BODY)
                        .font_weight(FontWeight::MEDIUM)
                        .text_ellipsis()
                        .line_clamp(1)
                        .child(SharedString::from(text.title)),
                )
                .child(
                    div()
                        .text_size(theme::TEXT_TINY)
                        .line_height(theme::LINE_TINY)
                        .text_color(theme.secondary)
                        .text_ellipsis()
                        .line_clamp(1)
                        .child(SharedString::from(text.line)),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(6.))
                .px(theme::ROW_PAD_X)
                .pt(px(4.))
                .pb(px(5.))
                .child(accept)
                .children(dismiss),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};
    use squawk_core::notetaker::calls::CallId;
    use std::time::Duration;

    #[test]
    fn a_heads_up_shows_the_event_and_its_time() {
        let start = Local.with_ymd_and_hms(2026, 9, 30, 15, 0, 0).unwrap();
        let text = panel_text(&Prompt::HeadsUp {
            key: "k".into(),
            title: "Design review".into(),
            start,
            end: start + chrono::Duration::minutes(30),
            auto: false,
        });
        assert_eq!(text.title, "Design review");
        assert_eq!(text.line, "15:00–15:30");
        assert_eq!((text.accept, text.dismiss), ("Record", Some("Not now")));
    }

    #[test]
    fn a_heads_up_for_a_meeting_that_records_itself_offers_only_skip() {
        let start = Local.with_ymd_and_hms(2026, 9, 30, 15, 0, 0).unwrap();
        let text = panel_text(&Prompt::HeadsUp {
            key: "k".into(),
            title: "Design review".into(),
            start,
            end: start + chrono::Duration::minutes(30),
            auto: true,
        });
        assert_eq!(text.title, "Design review");
        assert_eq!(text.line, "15:00–15:30 · records when the call starts");
        assert_eq!((text.accept, text.dismiss), ("Skip this one", None));
    }

    #[test]
    fn a_call_recording_on_its_own_says_so_and_offers_stop() {
        let text = panel_text(&Prompt::Recording {
            title: "Design review".into(),
            app: "Google Meet".into(),
        });
        assert_eq!(text.title, "Recording · Design review");
        assert_eq!(text.line, "Started with Google Meet");
        assert_eq!((text.accept, text.dismiss), ("Stop", None));
    }

    #[test]
    fn a_call_names_the_app() {
        let text = panel_text(&Prompt::Call {
            call: CallId(1),
            app: "Zoom".into(),
        });
        assert_eq!(text.title, "Call detected in Zoom");
        assert_eq!(text.line, "Start notes?");
        assert_eq!((text.accept, text.dismiss), ("Start", Some("Not now")));
    }

    #[test]
    fn the_limit_warning_offers_more_time() {
        let text = panel_text(&Prompt::StoppingSoon {
            title: "Weekly sync".into(),
            limit: Duration::from_secs(2 * 3600),
        });
        assert_eq!(text.title, "Stopping in 2 min");
        assert_eq!(text.line, "Weekly sync · 2 h limit");
        assert_eq!((text.accept, text.dismiss), ("Keep going +30 min", None));
    }

    #[test]
    fn saved_says_why_it_stopped() {
        let text = panel_text(&Prompt::Saved {
            title: "Weekly sync".into(),
            path: "/m.md".into(),
            length_secs: 7200,
            reason: StopReason::MaxLength,
        });
        assert_eq!(text.title, "Saved notes · Weekly sync");
        assert_eq!(text.line, "2:00:00 · reached the time limit");
        assert_eq!((text.accept, text.dismiss), ("Open", None));
    }
}
