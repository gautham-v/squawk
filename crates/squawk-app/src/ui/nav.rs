//! Keyboard navigation in the popover, as pure functions: ← and → switch
//! tabs (wrapping), ↑ and ↓ move a selection through the tab's list, Enter
//! acts on it. Esc (closing) is the popover's own.

use crate::ui::popover::Tab;

impl Tab {
    /// The tab to the right, wrapping to the first.
    pub fn next(self) -> Tab {
        let i = Tab::ALL.iter().position(|&t| t == self).unwrap_or(0);
        Tab::ALL[(i + 1) % Tab::ALL.len()]
    }

    /// The tab to the left, wrapping to the last.
    pub fn prev(self) -> Tab {
        let i = Tab::ALL.iter().position(|&t| t == self).unwrap_or(0);
        Tab::ALL[(i + Tab::ALL.len() - 1) % Tab::ALL.len()]
    }

    /// Whether ↑/↓ select rows here (Settings has controls, not rows).
    pub fn has_rows(self) -> bool {
        self != Tab::Settings
    }
}

/// ↑ or ↓.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Up,
    Down,
}

/// The selection after a step through `len` rows. Nothing selected yet:
/// either key selects the first row. It stops at the ends.
pub fn step(selected: Option<usize>, len: usize, step: Step) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let next = match (selected, step) {
        (None, _) => 0,
        (Some(i), Step::Up) => i.saturating_sub(1),
        (Some(i), Step::Down) => (i + 1).min(len - 1),
    };
    Some(next.min(len - 1))
}

/// What Enter does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enter {
    CopyDictation(usize),
    OpenMeeting(usize),
    EditDictionary,
}

pub fn enter(tab: Tab, selected: Option<usize>) -> Option<Enter> {
    match tab {
        Tab::History => selected.map(Enter::CopyDictation),
        Tab::Meetings => selected.map(Enter::OpenMeeting),
        Tab::Dictionary => Some(Enter::EditDictionary),
        Tab::Settings => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_and_right_walk_the_tabs_and_wrap() {
        assert_eq!(Tab::History.next(), Tab::Meetings);
        assert_eq!(Tab::Meetings.next(), Tab::Dictionary);
        assert_eq!(Tab::Dictionary.next(), Tab::Settings);
        assert_eq!(Tab::Settings.next(), Tab::History);
        assert_eq!(Tab::History.prev(), Tab::Settings);
        assert_eq!(Tab::Settings.prev(), Tab::Dictionary);
        for tab in Tab::ALL {
            assert_eq!(tab.next().prev(), tab);
        }
    }

    #[test]
    fn the_first_step_selects_the_first_row() {
        assert_eq!(step(None, 5, Step::Down), Some(0));
        assert_eq!(step(None, 5, Step::Up), Some(0));
    }

    #[test]
    fn steps_stop_at_the_ends() {
        assert_eq!(step(Some(0), 3, Step::Down), Some(1));
        assert_eq!(step(Some(2), 3, Step::Down), Some(2));
        assert_eq!(step(Some(0), 3, Step::Up), Some(0));
        assert_eq!(step(Some(2), 3, Step::Up), Some(1));
    }

    #[test]
    fn an_empty_list_has_no_selection_and_a_shrunk_one_clamps() {
        assert_eq!(step(None, 0, Step::Down), None);
        assert_eq!(step(Some(3), 0, Step::Up), None);
        assert_eq!(step(Some(9), 3, Step::Down), Some(2));
    }

    #[test]
    fn enter_acts_on_the_tab() {
        assert_eq!(enter(Tab::History, Some(2)), Some(Enter::CopyDictation(2)));
        assert_eq!(enter(Tab::History, None), None);
        assert_eq!(enter(Tab::Meetings, Some(0)), Some(Enter::OpenMeeting(0)));
        assert_eq!(enter(Tab::Dictionary, None), Some(Enter::EditDictionary));
        assert_eq!(enter(Tab::Dictionary, Some(4)), Some(Enter::EditDictionary));
        assert_eq!(enter(Tab::Settings, Some(1)), None);
        assert!(!Tab::Settings.has_rows());
        assert!(Tab::History.has_rows());
    }
}
