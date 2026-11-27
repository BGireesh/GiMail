use egui::{Rect, Ui};
use exo_commands::{Toast, ToastKind};
use exo_store::{
    CalendarChange, CalendarEvent, CalendarProviderKind, ConferenceKind, Cut, EventPatch,
    EventTime, StoredEvent,
};
use jiff::Zoned;

use super::{
    Action,
    event::Event,
    form::{EventForm, Field, Meeting, Mode, Values},
    patch::{is_one_on_one, new_event, new_id, patch_of, shift_of, values_of},
};

/// What Undo takes back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Undone {
    /// A new event (`Scheduling undone.`), whose form opens again.
    Create,
    /// An edit (`Event edit undone.`).
    Edit,
    /// A deletion (`Event deletion undone.`), after which the calendar goes to the event's day.
    Delete(jiff::civil::Date),
}

/// Which events of a series an edit or deletion covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Only,
    Following,
    All,
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Scope::Only => "Only this event",
            Scope::Following => "This and all following events",
            Scope::All => "All events",
        }
    }
}

/// An alert or dialog over the page, and what its actions lead to.
pub(super) enum Prompt {
    /// A confirmation or a message. The first action does `then`, and any other closes it.
    Confirm {
        title: &'static str,
        text: &'static str,
        actions: Vec<&'static str>,
        selected: usize,
        then: Then,
    },
    /// Saving an edit, which asks which events and then (on Google) whether to tell the guests.
    Edit {
        notify_step: bool,
        scopes: Vec<Scope>,
        choice: usize,
        selected: usize,
        ask_notify: bool,
        /// True for Outlook events with other guests (Graph always sends update emails).
        always_notify: bool,
        form: Box<EventForm>,
        /// Whether cancelling restores `form` to `self.calendar.form` (true for form saves,
        /// false for direct grid drags).
        restore_form: bool,
    },
    /// Deleting, which asks which events and then whether to tell the guests (Google) or
    /// include an optional cancellation note (Outlook).
    Delete {
        notify_step: bool,
        scopes: Vec<Scope>,
        choice: usize,
        selected: usize,
        ask_notify: bool,
        outlook_note: bool,
        note: String,
        target: Box<Target>,
    },
    /// The invite card's `RSVP` alert, where signed-out calendar account `email` signs in with the
    /// provider chosen to give `response` to `invite`.
    SignIn {
        email: String,
        invite: Box<crate::thread::invite::Invite>,
        response: crate::thread::invite::Response,
        provider: super::provider::ProviderPicker,
        selected: usize,
    },
}

impl Prompt {
    /// Its title.
    pub(super) fn title(&self) -> &'static str {
        match self {
            Prompt::Confirm { title, .. } => title,
            Prompt::Edit {
                notify_step: false, ..
            } => "Edit recurring event",
            Prompt::Edit {
                notify_step: true, ..
            } => "Send an update?",
            Prompt::Delete {
                notify_step: false, ..
            } => "Delete recurring event",
            Prompt::Delete {
                notify_step: true,
                outlook_note: true,
                ..
            } => "Include a note for attendees?",
            Prompt::Delete {
                notify_step: true,
                outlook_note: false,
                ..
            } => "Send a cancellation email?",
            Prompt::SignIn { .. } => "RSVP",
        }
    }
}

/// Outlook organizer deletion's optional attendee note prompt over `page`.
fn show_cancel_note(
    ui: &mut Ui,
    page: Rect,
    title: &str,
    note: &mut String,
    has_back: bool,
    selected: usize,
) -> Option<usize> {
    const NOTE_HEIGHT: f32 = 72.0;
    const INSET: f32 = 8.0;
    let actions: &[&str] = if has_back {
        &["Send", "Go back"]
    } else {
        &["Send", "Cancel"]
    };
    exo_ui::Alert {
        title,
        text: "",
        actions,
        selected,
    }
    .show_body(ui, page, NOTE_HEIGHT, |ui, body| {
        let box_rect = egui::Rect::from_min_size(body.min, egui::vec2(body.width(), NOTE_HEIGHT));
        let stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(0xd8, 0xdb, 0xe0));
        ui.painter().rect(
            exo_ui::pixel_snap(box_rect),
            egui::CornerRadius::same(4),
            egui::Color32::WHITE,
            stroke,
            egui::StrokeKind::Inside,
        );
        let inner = box_rect.shrink(INSET);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        child.add_sized(
            inner.size(),
            egui::TextEdit::multiline(note)
                .id(exo_ui::id(ui.ctx(), "calendar-cancel-note"))
                .hint_text("Add an optional note")
                .font(exo_ui::ALERT_TEXT.format(exo_ui::ALERT_TEXT_INK).font_id)
                .text_color(exo_ui::ALERT_TEXT_INK)
                .frame(egui::Frame::NONE),
        );
    })
}

/// The invite card's `RSVP` alert over `page`: the ask, and the provider dropdown 8px under it
/// (`display: flex; gap: 8px`). The dropdown is nudged down 3px as a dialog's dropdown button is
/// (`top: 3px`), with its text black at `.75` and its caret black at `.5` in both themes. The ask
/// wraps plainly, not as the alert's own text does (the flex item's anonymous block). Returns the
/// action clicked.
fn show_sign_in(
    ui: &mut Ui,
    page: Rect,
    title: &str,
    email: &str,
    provider: &mut super::provider::ProviderPicker,
    selected: usize,
) -> Option<usize> {
    const DROPDOWN: f32 = 20.0;
    const NUDGE: f32 = 3.0;
    let text = exo_ui::ALERT_TEXT;
    let lines = text.wrap(
        ui.ctx(),
        &super::provider::ask(email),
        exo_ui::ALERT_TEXT_WIDTH,
    );
    let asked = lines.len() as f32 * text.line_height;
    let height = asked + exo_ui::ALERT_PARAGRAPH_GAP + DROPDOWN;
    exo_ui::Alert {
        title,
        text: "",
        actions: &["Sign in", "Cancel"],
        selected,
    }
    .show_body(ui, page, height, |ui, body| {
        for (index, line) in lines.iter().enumerate() {
            text.paint_line(
                ui.painter(),
                egui::pos2(body.left(), body.top() + index as f32 * text.line_height),
                line,
                exo_ui::ALERT_TEXT_INK,
                None,
            );
        }
        let at = egui::pos2(
            body.left(),
            body.top() + asked + exo_ui::ALERT_PARAGRAPH_GAP + NUDGE,
        );
        provider.show(
            ui,
            exo_ui::id(ui.ctx(), "calendar-rsvp-provider"),
            at,
            exo_ui::ALERT_TEXT_INK,
            egui::Color32::from_black_alpha(128),
        );
    })
}

/// What a confirmation's first action does.
pub(super) enum Then {
    /// Close the alert (a validation message's `OK`).
    Nothing,
    /// Throw the form away.
    Discard,
    /// Throw the form away, then open a new one (a new event's form over an open one).
    Replace(Box<EventForm>),
    /// Go on saving, past the time check (`Make History`).
    SaveFromPast,
    /// Go on saving without guests (`Save without attendee`).
    SaveAlone,
}

/// The event a deletion is about.
pub(super) struct Target {
    calendar: String,
    event: CalendarEvent,
    start: Zoned,
}

impl crate::Mail {
    /// Save: what's typed in Who, the checks, then the time and 1:1 confirmations, then the
    /// change.
    pub(super) fn calendar_save(&mut self, now: &Zoned, past_checked: bool, alone_checked: bool) {
        let Some(form) = &mut self.calendar.form else {
            return;
        };
        if form.mode == Mode::Edit && form.dirty().is_empty() {
            return;
        }
        if !form.commit_guests() {
            form.mark_guests_invalid();
            return self.calendar_alert(
                "Almost there…",
                "Please enter a valid email address for each guest.",
            );
        }
        if form.values.title.trim().is_empty() {
            form.focus(Field::Title);
            return self.calendar_alert("Almost there…", "Please enter a title for your event.");
        }
        let Some((start, end)) = form.values.when.clone() else {
            form.focus(Field::When);
            return self.calendar_alert(
                "Almost there…",
                "Please pick a date and time for your calendar event.",
            );
        };
        // In the past means a timed event starting before now, or an all-day one ending before
        // now.
        let past = if form.values.all_day {
            end < *now
        } else {
            start < *now
        };
        if past && !past_checked {
            self.calendar.prompt = Some(Prompt::Confirm {
                title: "Temporal anomaly…",
                text: "You selected a time in the past!",
                actions: vec!["Make History", "Cancel"],
                selected: 0,
                then: Then::SaveFromPast,
            });
            return;
        }
        if form.mode == Mode::Create {
            let calendar = form.values.calendar.clone();
            let title = form.values.title.trim().to_owned();
            let me = self.calendar_account(&calendar);
            let others = self.calendar.form.as_ref().is_some_and(|form| {
                form.values
                    .guests
                    .iter()
                    .any(|guest| !guest.me && !guest.email.eq_ignore_ascii_case(&me))
            });
            let one_on_one = is_one_on_one(&title);
            if one_on_one && !others && !alone_checked {
                self.calendar.prompt = Some(Prompt::Confirm {
                    title: "Did you forget to add an attendee?",
                    text: "This looks like a 1:1, but there are no other attendees on the event.",
                    actions: vec!["Save without attendee", "Add attendee"],
                    selected: 0,
                    then: Then::SaveAlone,
                });
                return;
            }
            let Some(form) = self.calendar.form.take() else {
                return;
            };
            return self.calendar_schedule(form.values);
        }
        let Some(form) = self.calendar.form.take() else {
            return;
        };
        self.calendar_edit_dialogs(form, true);
    }

    /// Holds the new event and says so, with Undo.
    fn calendar_schedule(&mut self, values: Values) {
        let Some((start, end)) = values.when.clone() else {
            return;
        };
        let me = self.calendar_account(&values.calendar);
        let provider = self.calendar_provider(&values.calendar);
        let event = new_event(&values, &start, &end, &me, provider);
        self.out_of_office_created(&values.calendar, &event);
        let token = self.account.calendar().hold(CalendarChange::Create {
            calendar: values.calendar.clone(),
            event,
        });
        self.calendar.created.insert(token, values);
        let text = if self.snapshot.connection.is_online() {
            "Event scheduled."
        } else {
            "Event will be scheduled when online."
        };
        self.commands
            .toast(
                Toast::new(text).undo(crate::commands::Action::Calendar(Action::Undo(
                    token,
                    Undone::Create,
                ))),
            );
    }

    /// Whether Calendar hasn't got `event` of `calendar` yet, because it's a new event still
    /// queued. A deletion takes such an event back from the queue, with no Undo.
    fn calendar_unsynced(&self, calendar: &str, event: &CalendarEvent) -> bool {
        self.account.calendar().data().events.iter().any(|stored| {
            stored.unsynced && stored.calendar_id == calendar && stored.event.id == event.id
        })
    }

    /// For a recurring event, asks which events: only this one, or all of them too while its
    /// start stays on its day. For a Google event with guests, asks whether to tell them (Graph
    /// always notifies attendees on edit). Then makes the edit.
    pub(super) fn calendar_edit_dialogs(&mut self, form: EventForm, restore_form: bool) {
        let Some(edited) = &form.edited else { return };
        let dirty = form.dirty();
        let recurring = edited.event.recurring_event_id.is_some();
        let original_source = edited
            .event
            .original_start
            .as_ref()
            .or(edited.event.start.as_ref());
        let same_day = match (&form.values.when, original_source) {
            (Some((start, _)), Some(original)) => original
                .zoned(start.time_zone())
                .is_none_or(|at| at.date() == start.date()),
            _ => true,
        };
        let allow_all = !dirty.contains(&Field::When) || same_day;
        let me = self.calendar_account(&form.values.calendar);
        let has_guests = form
            .values
            .guests
            .iter()
            .any(|guest| !guest.me && !guest.email.eq_ignore_ascii_case(&me))
            || (dirty.contains(&Field::Guests) && edited.event.has_other_guests(&me));
        let outlook = edited.event.provider == CalendarProviderKind::Microsoft;
        let ask_notify = has_guests && !outlook;
        let always_notify = has_guests && outlook;
        // A new series that Calendar hasn't got yet has no instance of its own to change, so it
        // changes as a whole.
        let unsynced = recurring && self.calendar_unsynced(&form.values.calendar, &edited.event);
        let scope_step = recurring && allow_all;
        let scopes = edit_scopes(scope_step, unsynced);
        if !scope_step && !ask_notify {
            return self.calendar_apply_edit(form, scopes[0], always_notify);
        }
        self.calendar.prompt = Some(Prompt::Edit {
            notify_step: !scope_step,
            scopes,
            choice: 0,
            selected: 0,
            ask_notify,
            always_notify,
            form: Box::new(form),
            restore_form,
        });
    }

    /// Holds the changed fields for this event or its series, and says so with Undo.
    fn calendar_apply_edit(&mut self, form: EventForm, scope: Scope, notify: bool) {
        let Some(edited) = &form.edited else { return };
        let dirty = form.dirty();
        let calendar = form.values.calendar.clone();
        let me = self.calendar_account(&calendar);
        let mut patch = patch_of(&form.values, &dirty, &me, Some(&edited.event));
        let data = self.account.calendar().data();
        let instance = edited.event.clone();
        let (id, series_id, shift, shown) = match (scope, instance.recurring_event_id.clone()) {
            (Scope::All, Some(series)) => {
                let shift = dirty
                    .contains(&Field::When)
                    .then(|| shift_of(&instance, &form.values))
                    .flatten();
                let (start_template, end_template) = (patch.start.clone(), patch.end.clone());
                let mut series_patch = patch.clone();
                series_patch.start = None;
                series_patch.end = None;
                let shown = data
                    .events
                    .iter()
                    .filter(|stored| {
                        stored.calendar_id == calendar
                            && stored.event.recurring_event_id.as_deref() == Some(series.as_str())
                    })
                    .map(|stored| {
                        let mut event = stored.event.clone();
                        event.apply_patch(&series_patch);
                        if let Some((start, end)) = shift {
                            event.start = event
                                .start
                                .as_ref()
                                .and_then(|t| shift_time(t, start, start_template.as_ref()))
                                .or(event.start);
                            event.end = event
                                .end
                                .as_ref()
                                .and_then(|t| shift_time(t, end, end_template.as_ref()))
                                .or(event.end);
                        }
                        event
                    })
                    .collect();
                (series, None, shift, shown)
            }
            (Scope::Following, Some(series)) => {
                let token = self.calendar_split(&form, &instance, series, patch, notify);
                return self.calendar_edited_toast(token, notify);
            }
            _ => {
                let mut updated = instance.clone();
                updated.apply_patch(&patch);
                (
                    instance.id.clone().unwrap_or_default(),
                    instance.recurring_event_id.clone(),
                    None,
                    vec![updated],
                )
            }
        };
        if scope == Scope::All && shift.is_none() {
            patch.start = None;
            patch.end = None;
        }
        let token = self.account.calendar().hold(CalendarChange::Edit {
            calendar,
            id,
            series_id,
            patch,
            notify,
            shift,
            shown,
        });
        self.calendar_edited_toast(token, notify);
    }

    /// `This and all following events`: series `series` ends before its edited `instance`, and
    /// a new series with `patch` takes over from `instance`. Meanwhile the instances from
    /// `instance` on show the change, moved as `instance` moved. Returns the change's token.
    fn calendar_split(
        &mut self,
        form: &EventForm,
        instance: &CalendarEvent,
        series: String,
        mut patch: EventPatch,
        notify: bool,
    ) -> u64 {
        let calendar = form.values.calendar.clone();
        let original = instance
            .original_start
            .clone()
            .or_else(|| instance.start.clone());
        let from = match original {
            Some(EventTime::AllDay(day)) => Cut::Day(day),
            Some(EventTime::Timed { at, .. }) => Cut::At(at),
            None => Cut::At(jiff::Timestamp::now()),
        };
        let shift = form
            .dirty()
            .contains(&Field::When)
            .then(|| shift_of(instance, &form.values))
            .flatten();
        let following = |event: &CalendarEvent| {
            event.recurring_event_id.as_deref() == Some(series.as_str())
                && event.start.as_ref().is_none_or(|start| !from.keeps(start))
        };
        let (start_template, end_template) = (patch.start.clone(), patch.end.clone());
        let mut series_patch = patch.clone();
        series_patch.start = None;
        series_patch.end = None;
        let shown = self
            .account
            .calendar()
            .data()
            .events
            .iter()
            .filter(|stored| stored.calendar_id == calendar && following(&stored.event))
            .map(|stored| {
                let mut event = stored.event.clone();
                event.apply_patch(&series_patch);
                if let Some((start, end)) = shift {
                    event.start = event
                        .start
                        .as_ref()
                        .and_then(|t| shift_time(t, start, start_template.as_ref()))
                        .or(event.start);
                    event.end = event
                        .end
                        .as_ref()
                        .and_then(|t| shift_time(t, end, end_template.as_ref()))
                        .or(event.end);
                }
                event
            })
            .collect();
        let fallback = EventTime::timed(self.now.timestamp(), None);
        let (start, end) = (
            instance.start.clone().unwrap_or_else(|| fallback.clone()),
            instance.end.clone().unwrap_or(fallback),
        );
        patch.id = Some(new_id());
        self.account.calendar().hold(CalendarChange::Split {
            calendar,
            series,
            from,
            patch,
            start,
            end,
            notify,
            shown,
        })
    }

    /// An edit's toast, with Undo of the change held under `token`.
    fn calendar_edited_toast(&mut self, token: u64, notify: bool) {
        let text = match (self.snapshot.connection.is_online(), notify) {
            (false, _) => "Event will be updated when online.",
            (true, true) => "Event updated and notification sent.",
            (true, false) => "Event updated.",
        };
        self.commands
            .toast(
                Toast::new(text).undo(crate::commands::Action::Calendar(Action::Undo(
                    token,
                    Undone::Edit,
                ))),
            );
    }

    /// The details menu's `Delete`. It asks which events for a recurring event, and whether to
    /// tell the guests (or include a note on Outlook) for my event with guests, then makes the
    /// deletion, with Undo.
    pub(super) fn calendar_delete(&mut self, key: &str, now: &Zoned) {
        let Some((event, source, _)) = self.calendar_detailed(key, now) else {
            return;
        };
        self.calendar.details = None;
        let me = self.calendar_account(&event.calendar_id);
        let organizer = source.is_me_organizer(&me);
        // Ask whether to tell them (or add an optional cancel note on Outlook) when it's mine,
        // with someone else on it, and Calendar has it.
        let ask_notify = organizer
            && !self.calendar_unsynced(&event.calendar_id, &source)
            && source.has_other_guests(&me);
        let outlook_note = source.provider == CalendarProviderKind::Microsoft;
        let target = Target {
            calendar: event.calendar_id.clone(),
            event: source.clone(),
            start: event.start.clone(),
        };
        if source.recurring_event_id.is_some() {
            let mut scopes = vec![Scope::Only];
            if organizer {
                scopes.push(Scope::Following);
            }
            scopes.push(Scope::All);
            self.calendar.prompt = Some(Prompt::Delete {
                notify_step: false,
                scopes,
                choice: 0,
                selected: 0,
                ask_notify,
                outlook_note,
                note: String::new(),
                target: Box::new(target),
            });
        } else if ask_notify {
            self.calendar.prompt = Some(Prompt::Delete {
                notify_step: true,
                scopes: vec![Scope::Only],
                choice: 0,
                selected: 0,
                ask_notify,
                outlook_note,
                note: String::new(),
                target: Box::new(target),
            });
        } else {
            self.calendar_apply_delete(&target, Scope::Only, false, None);
        }
    }

    /// Holds the deletion and says so, with Undo. A new event that Calendar hasn't got yet is
    /// simply taken back, with no Undo.
    fn calendar_apply_delete(
        &mut self,
        target: &Target,
        scope: Scope,
        notify: bool,
        comment: Option<String>,
    ) {
        let instance = target.event.id.clone().unwrap_or_default();
        let series = target.event.recurring_event_id.clone();
        let change = match (scope, series) {
            (Scope::All, Some(series)) => CalendarChange::Delete {
                calendar: target.calendar.clone(),
                id: series,
                series_id: None,
                notify,
                comment,
            },
            (Scope::Following, Some(series)) => {
                let all_day = target
                    .event
                    .start
                    .as_ref()
                    .is_some_and(EventTime::is_all_day);
                let from = if all_day {
                    Cut::Day(target.start.date())
                } else {
                    Cut::At(target.start.timestamp())
                };
                CalendarChange::Truncate {
                    calendar: target.calendar.clone(),
                    series,
                    from,
                    notify,
                }
            }
            (_, series_id) => CalendarChange::Delete {
                calendar: target.calendar.clone(),
                id: instance,
                series_id,
                notify,
                comment,
            },
        };
        if self.calendar_unsynced(&target.calendar, &target.event) {
            self.account.calendar().queue(change);
            self.commands.toast(Toast::new("Event deleted."));
            return;
        }
        let token = self.account.calendar().hold(change);
        let text = if self.snapshot.connection.is_online() {
            "Event deleted."
        } else {
            "Event will be deleted when online."
        };
        self.commands
            .toast(
                Toast::new(text).undo(crate::commands::Action::Calendar(Action::Undo(
                    token,
                    Undone::Delete(target.start.date()),
                ))),
            );
    }

    /// Undo on a toast takes the held change back while it's held. Past the hold (a hovered
    /// toast outlives it), the change has gone, so nothing is undone and the toast goes.
    pub(super) fn calendar_undo(&mut self, token: u64, undone: Undone) {
        if !self.account.calendar().undo(token) {
            self.commands.hide_toast();
            return;
        }
        match undone {
            Undone::Create => {
                self.commands.toast(Toast::new("Scheduling undone."));
                if let Some(values) = self.calendar.created.remove(&token) {
                    let week = self.calendar.week.is_some();
                    self.calendar_open_form(EventForm::create(values, week));
                }
            }
            Undone::Edit => self.commands.toast(Toast::new("Event edit undone.")),
            Undone::Delete(date) => {
                self.commands.toast(Toast::new("Event deletion undone."));
                // The calendar on the event's day.
                if let Some(week) = &mut self.calendar.week {
                    week.go_to(date);
                } else if self.calendar.day.is_some() {
                    self.calendar_open_day(date, super::Source::Other);
                }
            }
        }
    }

    /// A queued change that Calendar refused, and the toast or alert that says so.
    pub(super) fn calendar_failed(&mut self, failure: super::sync::Failure) {
        use super::sync::Failure;
        match failure {
            Failure::Respond => self
                .commands
                .toast(Toast::new("Sorry! Failed to save your RSVP.").kind(ToastKind::Error)),
            Failure::RespondToInvite {
                uid,
                thread,
                status,
                refusal,
                links,
            } => {
                use exo_store::Refusal;
                // The card shows the calendar's answer again.
                self.calendar.answers.1.remove(&uid);
                let elsewhere = thread
                    .clone()
                    .filter(|thread| self.thread.as_ref().is_none_or(|open| open.id() != thread));
                // An unknown sender's invitation, still open, is answered with its own link.
                let answer = links
                    .as_ref()
                    .and_then(|links| links.answer(status))
                    .filter(|_| refusal == Refusal::UnknownSender && elsewhere.is_none())
                    .map(str::to_owned);
                let mark_known = links
                    .filter(|_| refusal == Refusal::NonOrganizer)
                    .map(|links| links.mark_sender_known);
                if refusal == Refusal::Gone {
                    let mut toast = Toast::new("RSVP not saved because event has changed.");
                    if let Some(thread) = elsewhere {
                        toast = toast.action(
                            "See Message",
                            None,
                            crate::commands::Action::Calendar(Action::SeeMessage(thread)),
                        );
                    }
                    self.commands.toast(toast);
                } else if let Some(thread) = thread {
                    // The alert takes the keyboard from the card.
                    if let Some(open) = &mut self.thread {
                        open.blur_invite();
                    }
                    if let Some(link) = answer {
                        self.open_url(&link);
                        self.pickers.rsvp_faster_next_time(thread);
                    } else if let Some(url) = mark_known {
                        self.pickers.rsvp_unknown_sender(url, thread);
                    } else {
                        self.pickers.rsvp_failed(thread);
                    }
                } else {
                    self.commands.toast(
                        Toast::new("Failed to respond to a calendar invitation.")
                            .kind(ToastKind::Error),
                    );
                }
            }
            Failure::Create {
                token,
                calendar,
                event,
            } => {
                // The form opens again with what was saved, and Try Again goes back to it. A
                // change queued before Exo last quit has its values read back from the event.
                let values = self
                    .calendar
                    .created
                    .remove(&token)
                    .or_else(|| self.calendar_values_of_new(&calendar, &event));
                if self.calendar.form.is_none()
                    && let Some(values) = values
                {
                    let mut form = EventForm::create(values, self.calendar.week.is_some());
                    form.failed = true;
                    self.calendar_open_form(form);
                }
                self.commands.toast(
                    Toast::new("Scheduling failed.")
                        .kind(ToastKind::Error)
                        .action(
                            "Try Again",
                            None,
                            crate::commands::Action::Calendar(Action::FormFocus(Field::Title)),
                        ),
                );
            }
            Failure::Edit => self
                .commands
                .toast(Toast::new("Event update failed.").kind(ToastKind::Error)),
            Failure::Delete => self
                .commands
                .toast(Toast::new("Event deletion failed.").kind(ToastKind::Error)),
        }
    }

    /// The form's values for a new event of `calendar` that Calendar refused.
    fn calendar_values_of_new(&self, calendar: &str, event: &CalendarEvent) -> Option<Values> {
        let me = self.calendar_account(calendar);
        let stored = StoredEvent {
            calendar_id: calendar.to_owned(),
            event: event.clone(),
            unsynced: true,
        };
        let colors = super::event::colors(self.account.settings());
        let shown = Event::new(&stored, None, &colors, &me, self.now.time_zone())?;
        let mut values = values_of(&shown, event, &me);
        values.repeat = event.recurrence.clone();
        if let Some(conf) = &event.conference
            && conf.pending
        {
            values.meeting = match conf.kind {
                ConferenceKind::Teams => Meeting::NewTeams,
                _ => Meeting::NewMeet,
            };
        }
        Some(values)
    }

    /// Cancel, Escape, ⌘⇧,: closes the form, asking first if that loses something.
    pub(super) fn calendar_discard(&mut self) {
        let Some(form) = &self.calendar.form else {
            return;
        };
        if form.is_empty() {
            self.calendar.form = None;
            return;
        }
        let title = if form.mode == Mode::Edit {
            "Discard changes?"
        } else {
            "Discard this event?"
        };
        self.calendar.prompt = Some(Prompt::Confirm {
            title,
            text: "Your changes will not be saved.",
            actions: vec!["Discard", "Cancel"],
            selected: 0,
            then: Then::Discard,
        });
    }

    /// A message under `title` with `OK`.
    fn calendar_alert(&mut self, title: &'static str, text: &'static str) {
        self.calendar.prompt = Some(Prompt::Confirm {
            title,
            text,
            actions: vec!["OK"],
            selected: 0,
            then: Then::Nothing,
        });
    }

    /// An action of the prompt, by index (Enter: the selected one; Escape: the last).
    pub(super) fn calendar_prompt_action(&mut self, index: usize, now: &Zoned) {
        let Some(prompt) = self.calendar.prompt.take() else {
            return;
        };
        match prompt {
            Prompt::Confirm { then, actions, .. } => {
                let confirmed = index == 0 && actions.len() > 1;
                match then {
                    Then::Nothing => {}
                    Then::Discard if confirmed => self.calendar.form = None,
                    Then::Replace(form) if confirmed => {
                        self.calendar.form = None;
                        self.calendar_open_form(*form);
                    }
                    Then::SaveFromPast if confirmed => self.calendar_save(now, true, false),
                    Then::SaveAlone if confirmed => self.calendar_save(now, true, true),
                    // `Add attendee` takes the keyboard to Who.
                    Then::SaveAlone => {
                        if let Some(form) = &mut self.calendar.form {
                            form.focus(Field::Guests);
                        }
                    }
                    Then::Discard | Then::Replace(_) | Then::SaveFromPast => {}
                }
            }
            // Scope step: `Ok` (0) / `Cancel` (1). Notify step: `Send` (0) / `Don't send` (1) /
            // `Go back` (2).
            Prompt::Edit {
                notify_step,
                scopes,
                choice,
                ask_notify,
                always_notify,
                form,
                restore_form,
                ..
            } => match step_edit_prompt(
                notify_step,
                scopes,
                choice,
                ask_notify,
                always_notify,
                form,
                restore_form,
                index,
            ) {
                EditOutcome::Prompt(prompt) => self.calendar.prompt = Some(prompt),
                EditOutcome::Apply {
                    form,
                    scope,
                    notify,
                } => self.calendar_apply_edit(*form, scope, notify),
                EditOutcome::Restore(form) => self.calendar.form = Some(*form),
                EditOutcome::Dismiss => {}
            },
            Prompt::Delete {
                notify_step: false,
                scopes,
                choice,
                ask_notify,
                outlook_note,
                note,
                target,
                ..
            } => {
                if index != 0 {
                    return;
                }
                match step_delete_scope(scopes[choice], ask_notify, outlook_note) {
                    DeleteScopeOutcome::NotifyStep => {
                        self.calendar.prompt = Some(Prompt::Delete {
                            notify_step: true,
                            scopes,
                            choice,
                            selected: 0,
                            ask_notify,
                            outlook_note,
                            note,
                            target,
                        });
                    }
                    DeleteScopeOutcome::Apply { scope, notify } => {
                        self.calendar_apply_delete(&target, scope, notify, None);
                    }
                }
            }
            Prompt::Delete {
                notify_step: true,
                scopes,
                choice,
                ask_notify,
                outlook_note: true,
                note,
                target,
                ..
            } => match index {
                0 => self.calendar_apply_delete(&target, scopes[choice], true, Some(note)),
                _ if scopes.len() > 1 => {
                    self.calendar.prompt = Some(Prompt::Delete {
                        notify_step: false,
                        scopes,
                        choice,
                        selected: 0,
                        ask_notify,
                        outlook_note: true,
                        note,
                        target,
                    });
                }
                _ => {}
            },
            Prompt::Delete {
                notify_step: true,
                scopes,
                choice,
                ask_notify,
                outlook_note: false,
                note,
                target,
                ..
            } => match index {
                0 | 1 => self.calendar_apply_delete(&target, scopes[choice], index == 0, None),
                _ if scopes.len() > 1 => {
                    self.calendar.prompt = Some(Prompt::Delete {
                        notify_step: false,
                        scopes,
                        choice,
                        selected: 0,
                        ask_notify,
                        outlook_note: false,
                        note,
                        target,
                    });
                }
                _ => {}
            },
            // `Sign in` does nothing until a provider is chosen. The alert stays up while the
            // browser signs in. `Cancel` (and Escape, once the list is closed) closes it.
            Prompt::SignIn {
                email,
                invite,
                response,
                mut provider,
                selected,
            } => {
                if index == 0
                    && let Some(chosen) = provider.chosen
                {
                    self.calendar_sign_in(
                        chosen,
                        Some(&email),
                        super::accounts::SignInFor::Invite(invite.clone(), response),
                    );
                }
                if index == 0 || provider.close() {
                    self.calendar.prompt = Some(Prompt::SignIn {
                        email,
                        invite,
                        response,
                        provider,
                        selected,
                    });
                }
            }
        }
    }

    /// Closes the invite card's `RSVP` alert, if it's up, once its sign-in has come back.
    pub(super) fn calendar_close_sign_in_prompt(&mut self) {
        if matches!(self.calendar.prompt, Some(Prompt::SignIn { .. })) {
            self.calendar.prompt = None;
        }
    }

    /// ↑/↓ in a dialog's options.
    pub(super) fn calendar_prompt_choose(&mut self, delta: isize) {
        if let Some(
            Prompt::Edit {
                notify_step: false,
                scopes,
                choice,
                ..
            }
            | Prompt::Delete {
                notify_step: false,
                scopes,
                choice,
                ..
            },
        ) = &mut self.calendar.prompt
        {
            *choice = choice.saturating_add_signed(delta).min(scopes.len() - 1);
        }
    }

    /// ←/→ and Tab/⇧Tab across a prompt's buttons.
    pub(super) fn calendar_prompt_select(&mut self, delta: isize) {
        let Some((count, _)) = self.calendar_prompt_actions() else {
            return;
        };
        if let Some(
            Prompt::Confirm { selected, .. }
            | Prompt::Edit { selected, .. }
            | Prompt::Delete { selected, .. }
            | Prompt::SignIn { selected, .. },
        ) = &mut self.calendar.prompt
        {
            *selected = exo_ui::Alert::move_selection(*selected, count, delta, true);
        }
    }

    /// Whether the Outlook cancel-note textarea currently owns keyboard focus.
    pub(super) fn calendar_cancel_note_focused(&self) -> bool {
        matches!(
            self.calendar.prompt,
            Some(Prompt::Delete {
                notify_step: true,
                outlook_note: true,
                ..
            })
        ) && self.ctx.memory(|memory| {
            memory.focused() == Some(exo_ui::id(&self.ctx, "calendar-cancel-note"))
        })
    }

    /// The prompt's actions and which button is selected, for its keys.
    pub(super) fn calendar_prompt_actions(&self) -> Option<(usize, usize)> {
        Some(match self.calendar.prompt.as_ref()? {
            Prompt::Confirm {
                actions, selected, ..
            } => (actions.len(), *selected),
            Prompt::Edit {
                notify_step: true,
                selected,
                ..
            }
            | Prompt::Delete {
                notify_step: true,
                outlook_note: false,
                selected,
                ..
            } => (3, *selected),
            Prompt::Edit { selected, .. }
            | Prompt::Delete { selected, .. }
            | Prompt::SignIn { selected, .. } => (2, *selected),
        })
    }

    /// The prompt over the dimmed `page`, if one is up.
    pub(crate) fn calendar_show_prompt(&mut self, ui: &Ui, page: Rect) {
        self.calendar_show_options(ui.ctx(), page);
        let week_start = self.calendar_week_start_day();
        if let Some(form) = &mut self.calendar.form {
            form.show_custom(ui.ctx(), page, &self.now, week_start);
        }
        let Some(prompt) = &mut self.calendar.prompt else {
            return;
        };
        let title = prompt.title();
        let (clicked, _) =
            exo_ui::Alert::modal(ui.ctx(), "calendar-prompt", page, |area| match prompt {
                Prompt::Confirm {
                    text,
                    actions,
                    selected,
                    ..
                } => exo_ui::Alert {
                    title,
                    text,
                    actions,
                    selected: *selected,
                }
                .show(area, page),
                Prompt::Edit {
                    notify_step: false,
                    scopes,
                    choice,
                    selected,
                    ..
                } => {
                    let labels: Vec<&str> = scopes.iter().map(|scope| scope.label()).collect();
                    exo_ui::Alert {
                        title,
                        text: "",
                        actions: &["Ok", "Cancel"],
                        selected: *selected,
                    }
                    .show_options(area, page, &labels, choice)
                }
                Prompt::Delete {
                    notify_step: false,
                    scopes,
                    choice,
                    selected,
                    ..
                } => {
                    let labels: Vec<&str> = scopes.iter().map(|scope| scope.label()).collect();
                    exo_ui::Alert {
                        title,
                        text: "",
                        actions: &["Delete", "Cancel"],
                        selected: *selected,
                    }
                    .show_options(area, page, &labels, choice)
                }
                Prompt::Edit {
                    notify_step: true,
                    selected,
                    ..
                } => exo_ui::Alert {
                    title,
                    text: "Existing guests will receive an update email.",
                    actions: &["Send", "Don't send", "Go back"],
                    selected: *selected,
                }
                .show(area, page),
                Prompt::Delete {
                    notify_step: true,
                    outlook_note: true,
                    scopes,
                    selected,
                    note,
                    ..
                } => show_cancel_note(area, page, title, note, scopes.len() > 1, *selected),
                Prompt::Delete {
                    notify_step: true,
                    outlook_note: false,
                    selected,
                    ..
                } => exo_ui::Alert {
                    title,
                    text: "Would you like to send a cancellation email to Google Calendar guests?",
                    actions: &["Send", "Don't send", "Go back"],
                    selected: *selected,
                }
                .show(area, page),
                Prompt::SignIn {
                    email,
                    provider,
                    selected,
                    ..
                } => show_sign_in(area, page, title, email, provider, *selected),
            });
        if let Some(index) = clicked {
            let now = self.now.clone();
            self.calendar_prompt_action(index, &now);
        }
    }
}

/// Shifts `time` by civil wall-clock delta `by`, adopting all-day/zone from `template` if set.
fn shift_time(
    time: &EventTime,
    by: jiff::SignedDuration,
    template: Option<&EventTime>,
) -> Option<EventTime> {
    time.shift_wall_clock(
        by,
        template.map(EventTime::is_all_day),
        template.and_then(EventTime::time_zone),
    )
}

/// The scopes offered for an edit. When `scope_step` is false (non-recurring, or a recurring
/// instance moved to another day), only a single scope applies: `Scope::All` for an unsynced
/// series (which has no server instance id yet) or `Scope::Only` otherwise.
fn edit_scopes(scope_step: bool, unsynced: bool) -> Vec<Scope> {
    match (scope_step, unsynced) {
        (true, false) => vec![Scope::Only, Scope::Following, Scope::All],
        (true, true) => vec![Scope::Following, Scope::All],
        (false, true) => vec![Scope::All],
        (false, false) => vec![Scope::Only],
    }
}

/// Result of confirming the scope step on [`Prompt::Delete`].
#[derive(Debug, PartialEq, Eq)]
enum DeleteScopeOutcome {
    NotifyStep,
    Apply { scope: Scope, notify: bool },
}

/// Decides whether confirming `scope` on [`Prompt::Delete`] proceeds to the notification/note
/// step or deletes immediately. On Outlook, `Scope::Following` truncates the series (which has no
/// cancellation comment field), so it skips the optional note prompt and notifies directly.
fn step_delete_scope(scope: Scope, ask_notify: bool, outlook_note: bool) -> DeleteScopeOutcome {
    if ask_notify && !(outlook_note && scope == Scope::Following) {
        DeleteScopeOutcome::NotifyStep
    } else {
        DeleteScopeOutcome::Apply {
            scope,
            notify: ask_notify,
        }
    }
}

/// Result of handling an action on [`Prompt::Edit`].
enum EditOutcome {
    Prompt(Prompt),
    Apply {
        form: Box<EventForm>,
        scope: Scope,
        notify: bool,
    },
    Restore(Box<EventForm>),
    Dismiss,
}

/// Advances [`Prompt::Edit`] by action `index` (Scope step: 0 = Ok, 1 = Cancel; Notify step:
/// 0 = Send, 1 = Don't send, 2 = Go back).
#[expect(
    clippy::too_many_arguments,
    reason = "pure state transition for Prompt::Edit fields"
)]
fn step_edit_prompt(
    notify_step: bool,
    scopes: Vec<Scope>,
    choice: usize,
    ask_notify: bool,
    always_notify: bool,
    form: Box<EventForm>,
    restore_form: bool,
    index: usize,
) -> EditOutcome {
    if !notify_step {
        if index != 0 {
            return if restore_form {
                EditOutcome::Restore(form)
            } else {
                EditOutcome::Dismiss
            };
        }
        if ask_notify {
            return EditOutcome::Prompt(Prompt::Edit {
                notify_step: true,
                scopes,
                choice,
                selected: 0,
                ask_notify,
                always_notify,
                form,
                restore_form,
            });
        }
        return EditOutcome::Apply {
            scope: scopes[choice],
            notify: always_notify,
            form,
        };
    }
    match index {
        0 | 1 => EditOutcome::Apply {
            scope: scopes[choice],
            notify: index == 0,
            form,
        },
        // `Go back` returns to the scope step only when there were multiple scopes to choose from.
        _ if scopes.len() > 1 => EditOutcome::Prompt(Prompt::Edit {
            notify_step: false,
            scopes,
            choice,
            selected: 0,
            ask_notify,
            always_notify,
            form,
            restore_form,
        }),
        _ if restore_form => EditOutcome::Restore(form),
        _ => EditOutcome::Dismiss,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::form::Edited;

    fn edited_form(recurring: bool, title: &str) -> Box<EventForm> {
        let mut values = Values {
            title: "Original".into(),
            when: None,
            all_day: false,
            guests: Vec::new(),
            location: String::new(),
            about: String::new(),
            meeting: Meeting::None,
            calendar: "maya@brightmoor.co".into(),
            repeat: None,
        };
        let edited = Edited {
            key: "k1".into(),
            event: CalendarEvent {
                id: Some("e1".into()),
                recurring_event_id: recurring.then(|| "series1".into()),
                ..CalendarEvent::default()
            },
            color: exo_ui::CalendarColor::BLUE,
        };
        let mut form = EventForm::edit(values.clone(), edited, false, Field::Title);
        values.title = title.into();
        form.values = values;
        Box::new(form)
    }

    #[test]
    fn edit_scopes_covers_unsynced_single_choice_and_non_recurring() {
        assert_eq!(
            edit_scopes(true, false),
            vec![Scope::Only, Scope::Following, Scope::All]
        );
        assert_eq!(edit_scopes(true, true), vec![Scope::Following, Scope::All]);
        assert_eq!(
            edit_scopes(false, true),
            vec![Scope::All],
            "unsynced recurring event moved to another day patches the series"
        );
        assert_eq!(edit_scopes(false, false), vec![Scope::Only]);
    }

    #[test]
    fn cancelling_scope_step_restores_form_edits_and_dismisses_grid_drags() {
        let form = edited_form(true, "Edited title");
        let scopes = edit_scopes(true, false);
        match step_edit_prompt(false, scopes.clone(), 0, true, false, form, true, 1) {
            EditOutcome::Restore(restored) => assert_eq!(restored.values.title, "Edited title"),
            _ => panic!("cancelling scope step from form must restore form"),
        }

        let drag_form = edited_form(true, "Moved on grid");
        assert!(matches!(
            step_edit_prompt(false, scopes, 0, true, false, drag_form, false, 1),
            EditOutcome::Dismiss
        ));
    }

    #[test]
    fn go_back_from_non_scoped_notify_step_restores_form_without_reopening_scope_alert() {
        // Moved recurring event: single scope [Scope::Only], notify_step started directly at true.
        // Clicking "Go back" (index 2) must NOT reopen a single-choice scope alert even though
        // recurring_event_id is Some; it must restore the form instead.
        let form = edited_form(true, "Moved recurring");
        let scopes = edit_scopes(false, false);
        assert_eq!(scopes.len(), 1);
        match step_edit_prompt(true, scopes, 0, true, false, form, true, 2) {
            EditOutcome::Restore(restored) => {
                assert_eq!(restored.values.title, "Moved recurring");
            }
            _ => panic!("Go back on single-scope recurring notify step must restore form"),
        }

        // Unsynced moved recurring event: single scope [Scope::All], applying uses Scope::All.
        let unsynced_form = edited_form(true, "Unsynced moved");
        let unsynced_scopes = edit_scopes(false, true);
        match step_edit_prompt(
            true,
            unsynced_scopes,
            0,
            true,
            false,
            unsynced_form,
            true,
            0,
        ) {
            EditOutcome::Apply { scope, notify, .. } => {
                assert_eq!(scope, Scope::All);
                assert!(notify);
            }
            _ => panic!("Send on unsynced moved series must apply with Scope::All"),
        }

        // Non-recurring event with guests: Go back restores the form.
        let single_form = edited_form(false, "Edited lunch");
        let single_scopes = edit_scopes(false, false);
        match step_edit_prompt(true, single_scopes, 0, true, false, single_form, true, 2) {
            EditOutcome::Restore(restored) => assert_eq!(restored.values.title, "Edited lunch"),
            _ => panic!("Go back on non-recurring notify step must restore form"),
        }
    }

    #[test]
    fn go_back_from_multi_scope_notify_step_returns_to_scope_step_and_preserves_restore() {
        let form = edited_form(true, "Series title");
        let scopes = edit_scopes(true, false);
        let EditOutcome::Prompt(Prompt::Edit {
            notify_step,
            scopes,
            choice,
            ask_notify,
            always_notify,
            form,
            restore_form,
            ..
        }) = step_edit_prompt(true, scopes, 2, true, false, form, true, 2)
        else {
            panic!("expected return to scope step");
        };
        assert!(!notify_step);
        assert_eq!(choice, 2);
        assert!(restore_form);
        // Cancelling from the reopened scope step still restores the edited form.
        match step_edit_prompt(
            notify_step,
            scopes,
            choice,
            ask_notify,
            always_notify,
            form,
            restore_form,
            1,
        ) {
            EditOutcome::Restore(restored) => assert_eq!(restored.values.title, "Series title"),
            _ => panic!("expected restored form after cancelling returned scope step"),
        }
    }

    #[test]
    fn outlook_recurring_edit_skips_notify_prompt_and_preserves_always_notify() {
        let form = edited_form(true, "Outlook standup");
        let scopes = edit_scopes(true, false);
        match step_edit_prompt(false, scopes, 2, false, true, form, true, 0) {
            EditOutcome::Apply { scope, notify, .. } => {
                assert_eq!(scope, Scope::All);
                assert!(notify);
            }
            _ => panic!("Outlook edit must apply immediately without Send an update? step"),
        }
    }

    #[test]
    fn outlook_recurring_delete_skips_cancel_note_for_following_only() {
        assert_eq!(
            step_delete_scope(Scope::Following, true, true),
            DeleteScopeOutcome::Apply {
                scope: Scope::Following,
                notify: true,
            },
            "Outlook series truncation skips cancel-note prompt instead of silently dropping it"
        );
        assert_eq!(
            step_delete_scope(Scope::Only, true, true),
            DeleteScopeOutcome::NotifyStep,
            "Outlook single-occurrence cancel prompts for optional note"
        );
        assert_eq!(
            step_delete_scope(Scope::All, true, true),
            DeleteScopeOutcome::NotifyStep,
            "Outlook whole-series cancel prompts for optional note"
        );
        assert_eq!(
            step_delete_scope(Scope::Following, true, false),
            DeleteScopeOutcome::NotifyStep,
            "Google recurring delete still asks Send cancellation? for Following"
        );
        assert_eq!(
            step_delete_scope(Scope::Only, false, true),
            DeleteScopeOutcome::Apply {
                scope: Scope::Only,
                notify: false,
            }
        );
    }
}
