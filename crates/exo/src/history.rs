//! Back and forward through where the user has been: ⌘[ or ⌘← goes back, ⌘] or ⌘→ forward, and
//! Backspace in the list goes back. A place is a tab's route ([`Route`]): each list shown with its
//! filter, each conversation opened, each search run and See Your Week, up to 50. Going back to
//! one goes to that route.
//!
//! Leaving somewhere opened from the place before it goes back to that place rather than adding
//! it again, so forward opens it again: Escape from a conversation (or the ones J and K went on
//! to), leaving search, closing the week. The week is one place whichever week it shows, and
//! wherever it's left for takes its place. Search opened with an empty bar takes the place of the
//! first search run. Opened from a list that fills its bar in (`in:sent `), it's a search for
//! that text, a place of its own, which going back to runs.
//!
//! The keys still reach the rest of the app, and do nothing while the keyboard is in a text field
//! (the editor, a recipient field, the search bar, the palette) or a dialog.

use egui::{Key, Modifiers};

use crate::tabs::{Route, View};

/// As many places as the history keeps.
const MAX_PLACES: usize = 50;

/// The places visited, oldest first, and which one is on screen.
#[derive(Debug, Default)]
pub(crate) struct History {
    places: Vec<Route>,
    current: usize,
}

impl History {
    /// `place` is on screen. A new one goes after the current place, dropping those ahead of it,
    /// unless it's the place what's on screen was opened from, which it goes back to, or the
    /// current place gives way to it.
    pub(crate) fn visit(&mut self, place: Route) {
        if self.places.get(self.current) == Some(&place) {
            return;
        }
        if !self.places.is_empty() {
            let opened_from = self.places[..=self.current]
                .iter()
                .rposition(|visited| !opened(visited, &place));
            if let Some(index) =
                opened_from.filter(|index| *index < self.current && self.places[*index] == place)
            {
                self.current = index;
                return;
            }
        }
        let gives_way = self.places.get(self.current).is_some_and(|current| {
            matches!(current.view, View::Week(_))
                || (current.view == View::Search(None)
                    && matches!(place.view, View::Search(Some(_))))
        });
        self.places.truncate(self.current + usize::from(!gives_way));
        self.places.push(place);
        if self.places.len() > MAX_PLACES {
            self.places.remove(0);
        }
        self.current = self.places.len() - 1;
    }

    /// The current place is `place` instead, as when a conversation opened from a link couldn't
    /// be had and its list shows.
    pub(crate) fn replace(&mut self, place: Route) {
        if let Some(current) = self.places.get_mut(self.current) {
            *current = place;
        }
    }

    /// The place before the current one, now current.
    pub(crate) fn back(&mut self) -> Option<Route> {
        self.current = self.current.checked_sub(1)?;
        self.places.get(self.current).cloned()
    }

    /// The place after the current one, now current.
    pub(crate) fn forward(&mut self) -> Option<Route> {
        let next = self.places.get(self.current + 1).cloned()?;
        self.current += 1;
        Some(next)
    }
}

/// Whether `visited` was opened from `place`: a conversation from its list or search, search or
/// the week from anywhere else.
fn opened(visited: &Route, place: &Route) -> bool {
    let conversation = visited.thread.is_some()
        && place.thread.is_none()
        && Route {
            thread: None,
            ..visited.clone()
        } == *place;
    let over = |view: fn(&View) -> bool| view(&visited.view) && !view(&place.view);
    conversation
        || over(|view| matches!(view, View::Search(_)))
        || over(|view| matches!(view, View::Week(_)))
}

/// Which way a key goes: ⌘[ and ⌘← back, ⌘] and ⌘→ forward, with ⌘ alone.
fn direction(event: &egui::Event) -> Option<isize> {
    let egui::Event::Key {
        key,
        pressed: true,
        modifiers,
        ..
    } = event
    else {
        return None;
    };
    if !modifiers.matches_exact(Modifiers::COMMAND) {
        return None;
    }
    match key {
        Key::OpenBracket | Key::ArrowLeft => Some(-1),
        Key::CloseBracket | Key::ArrowRight => Some(1),
        _ => None,
    }
}

impl crate::Mail {
    /// Goes back or forward for this frame's ⌘[, ⌘], ⌘← and ⌘→, leaving the keys for what has
    /// the keyboard. Call before the commands take the frame's keys.
    pub(crate) fn history_keys(&mut self, ctx: &egui::Context) {
        let region = self.commands.region();
        if region.is_text_input() || region.is_overlay() {
            return;
        }
        let steps: Vec<isize> =
            ctx.input(|input| input.events.iter().filter_map(direction).collect());
        for step in steps {
            let place = if step < 0 {
                self.history.back()
            } else {
                self.history.forward()
            };
            if let Some(place) = place {
                self.go_to_route(&place);
            }
        }
    }

    /// Backspace in the list goes back, as ⌘[ does.
    pub(crate) fn history_back(&mut self) {
        if let Some(place) = self.history.back() {
            self.go_to_route(&place);
        }
    }

    /// Notes where the mail is. Call once the frame has gone wherever it goes.
    pub(crate) fn history_visit(&mut self) {
        self.history.visit(self.place());
    }

    /// Makes where the mail is now the current place, in place of the one it was.
    pub(crate) fn history_replace(&mut self) {
        self.history.replace(self.place());
    }

    /// Where the mail is, as the history keeps it: its route, except that search opened with its
    /// bar filled in from the list and nothing run is the search for that text.
    fn place(&self) -> Route {
        let mut place = self.route();
        if let Some(text) = self
            .search
            .as_ref()
            .filter(|search| search.query().is_none())
            .and_then(crate::search::Search::opened_with)
        {
            place.view = View::Search(Some(text.to_owned()));
        }
        place
    }
}

#[cfg(test)]
mod tests {
    use exo_store::{Filter, ListId};

    use super::*;

    fn list(list: ListId) -> Route {
        Route {
            list: Some(list),
            ..Route::home("maya@brightmoor.co".into())
        }
    }

    fn thread(list_id: ListId, id: &str) -> Route {
        Route {
            thread: Some(id.to_owned()),
            ..list(list_id)
        }
    }

    fn search(query: Option<&str>) -> Route {
        Route {
            view: View::Search(query.map(str::to_owned)),
            ..list(ListId::Important)
        }
    }

    fn week(day: i8) -> Route {
        Route {
            view: View::Week(jiff::civil::date(2026, 11, day)),
            ..list(ListId::Important)
        }
    }

    #[test]
    fn back_and_forward_walk_the_places_visited() {
        let mut history = History::default();
        history.visit(list(ListId::Important));
        history.visit(list(ListId::Done));
        history.visit(list(ListId::Starred));
        assert_eq!(history.back(), Some(list(ListId::Done)));
        assert_eq!(history.back(), Some(list(ListId::Important)));
        assert_eq!(history.back(), None, "nothing before the first");
        assert_eq!(history.forward(), Some(list(ListId::Done)));
        // Somewhere new from there drops what was ahead.
        history.visit(list(ListId::Sent));
        assert_eq!(history.forward(), None);
        assert_eq!(history.back(), Some(list(ListId::Done)));
    }

    #[test]
    fn leaving_a_conversation_goes_back_to_where_it_was_opened() {
        let mut history = History::default();
        history.visit(list(ListId::Important));
        history.visit(thread(ListId::Important, "a"));
        // J: the next conversation.
        history.visit(thread(ListId::Important, "b"));
        // Escape.
        history.visit(list(ListId::Important));
        assert_eq!(history.forward(), Some(thread(ListId::Important, "a")));
        assert_eq!(history.forward(), Some(thread(ListId::Important, "b")));
        assert_eq!(history.back(), Some(thread(ListId::Important, "a")));
        assert_eq!(history.back(), Some(list(ListId::Important)));
        // Going to the list again from elsewhere is somewhere new.
        let mut history = History::default();
        history.visit(list(ListId::Important));
        history.visit(list(ListId::Done));
        history.visit(list(ListId::Important));
        assert_eq!(history.back(), Some(list(ListId::Done)));
    }

    #[test]
    fn each_search_run_is_a_place_and_leaving_search_goes_back() {
        let mut history = History::default();
        history.visit(list(ListId::Important));
        history.visit(search(None));
        // The first search run takes the empty search's place.
        history.visit(search(Some("budget")));
        history.visit(search(Some("from:bea")));
        history.visit(Route {
            thread: Some("a".into()),
            ..search(Some("from:bea"))
        });
        // Escape from the conversation, then from search.
        history.visit(search(Some("from:bea")));
        history.visit(list(ListId::Important));
        assert_eq!(history.forward(), Some(search(Some("budget"))));
        assert_eq!(history.forward(), Some(search(Some("from:bea"))));
        assert_eq!(history.back(), Some(search(Some("budget"))));
        assert_eq!(history.back(), Some(list(ListId::Important)));
        assert_eq!(history.back(), None, "the empty search isn't a place");
    }

    #[test]
    fn the_week_is_one_place_that_whatever_follows_replaces() {
        let mut history = History::default();
        history.visit(list(ListId::Important));
        history.visit(week(15));
        history.visit(week(22));
        // Closing it goes back, and forward opens it again.
        history.visit(list(ListId::Important));
        assert_eq!(history.forward(), Some(week(22)));
        // Going elsewhere from it takes its place.
        history.visit(list(ListId::Sent));
        assert_eq!(history.back(), Some(list(ListId::Important)));
        assert_eq!(history.forward(), Some(list(ListId::Sent)));
    }

    #[test]
    fn a_filter_is_a_place() {
        let mut history = History::default();
        history.visit(list(ListId::Important));
        let unread = Route {
            filter: Some(Filter::Unread),
            ..list(ListId::Important)
        };
        history.visit(unread.clone());
        history.visit(Route {
            thread: Some("a".into()),
            ..unread.clone()
        });
        history.visit(unread.clone());
        assert_eq!(history.back(), Some(list(ListId::Important)));
        assert_eq!(history.forward(), Some(unread));
    }

    #[test]
    fn fifty_places_at_most() {
        let mut history = History::default();
        for n in 0..60 {
            history.visit(thread(ListId::Important, &n.to_string()));
        }
        let mut back = 0;
        while history.back().is_some() {
            back += 1;
        }
        assert_eq!(back, MAX_PLACES - 1);
    }

    #[test]
    fn only_command_and_a_bracket_or_arrow_go() {
        let press = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let command = Modifiers {
            mac_cmd: true,
            command: true,
            ..Modifiers::NONE
        };
        assert_eq!(direction(&press(Key::OpenBracket, command)), Some(-1));
        assert_eq!(direction(&press(Key::ArrowLeft, command)), Some(-1));
        assert_eq!(direction(&press(Key::CloseBracket, command)), Some(1));
        assert_eq!(direction(&press(Key::ArrowRight, command)), Some(1));
        assert_eq!(
            direction(&press(Key::OpenBracket, command | Modifiers::SHIFT)),
            None,
            "⌘⇧[ is the previous tab"
        );
        assert_eq!(direction(&press(Key::ArrowLeft, Modifiers::NONE)), None);
    }
}
