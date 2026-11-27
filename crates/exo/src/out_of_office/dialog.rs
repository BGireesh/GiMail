//! The dialog: the toggle, the schedule or first/last days, the subject (Gmail) and message(s),
//! and who gets replies, over Cancel and Save.
//!
//! It has a light look of its own, in Snow and Carbon alike: Exo Sans Alt in the light theme's
//! text colors, a purple switch and Save. The date inputs and the message keep Classic's look.

use egui::{Color32, Context, CornerRadius, Id, Mesh, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use exo_editor::{ActiveDate, BodyText, Editor, EditorStyle, FormatCommand};
use exo_store::{
    AutoReply, AutoReplyStatus, ExternalAudience, OutOfOfficeDays, OutOfOfficeResponder,
    OutlookAutoReply,
};
use exo_ui::{
    ActionKind, DateInput, DialogAction, Family, Font, Scrollbar, SliderColors, TextStyle,
};
use gmail::VacationSettings;
use jiff::{SignedDuration, Timestamp, ToSpan, Zoned, civil::Date};

use super::date_input::{self, DateEvent};

/// The light look's text colors: default, subdued and accent.
const TEXT_DEFAULT: Color32 = Color32::from_rgb(0x14, 0x14, 0x13);
const TEXT_SUBDUED: Color32 = Color32::from_rgb(0x73, 0x71, 0x6d);
const TEXT_ACCENT: Color32 = Color32::from_rgb(0x79, 0x76, 0x72);
/// The header: Exo Sans Alt 16px/20px.
const HEADER: TextStyle = TextStyle::new(Font::new(Family::SansAlt, 400), 16.0, 20.0);
/// A label: 600 12px/20px, `letter-spacing: .06px` (Classic's), `top: 3px`.
const LABEL: TextStyle =
    TextStyle::new(Font::new(Family::SansAlt, 600), 12.0, 20.0).letter_spacing(0.06);
/// The text, the subject and the checkboxes' labels: 400 12px/20px, `top: 3px`.
const TEXT: TextStyle = TextStyle::new(Font::new(Family::SansAlt, 400), 12.0, 20.0);
/// The toggle's switch: the light look's neutral background off and its activated accent on, under
/// Classic's filter.
const SWITCH: SliderColors = SliderColors {
    off: Color32::from_rgb(0x73, 0x71, 0x6d),
    on: Color32::from_rgb(0x71, 0x4c, 0xb6),
};
/// A checked box: the activated accent with a white tick; unchecked, the text colour at a quarter.
const CHECKBOX: [Color32; 3] = [
    Color32::from_rgb(0x71, 0x4c, 0xb6),
    Color32::WHITE,
    TEXT_DEFAULT,
];
/// `hr`'s and the message's `border-bottom: 1px solid rgba(0, 0, 0, .1)`.
const RULE: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 26);
/// A loading skeleton: `rgba(0, 0, 0, .08)`, 16px tall, 4px corners, a white shine crossing it
/// every 2s.
const SKELETON: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 20);
const SKELETON_HEIGHT: f32 = 16.0;

/// The body's rows, from the body's top (below the header): the toggle's line box, the switch,
/// the rule, the dates' line, the subject's label and input, the message's label, the editor
/// (moved 1px up) and its rule, the recipients' lines, and the footer's buttons.
const TOGGLE_TOP: f32 = 23.0;
const SWITCH_GAP: f32 = 12.0;
const HR_TOP: f32 = 56.0;
const DATES_TOP: f32 = 79.0;
/// The date inputs' line box: 2px under the text's (`top: -1px` on the picker, `top: 1px` and
/// a 16px line in the input).
const DATE_INPUT_TOP: f32 = 81.0;
/// A date picker's `padding: 0 4px`, with the two spans 16px apart.
const DATE_PADDING: f32 = 4.0;
const DATES_GAP: f32 = 16.0;
const SUBJECT_LABEL_TOP: f32 = 119.0;
const SUBJECT_TOP: f32 = 151.0;
const MESSAGE_LABEL_TOP: f32 = 195.0;
const EDITOR_TOP: f32 = 223.0;
/// The message editor's 164px height, less the stable gutter of its scrollbar (11px, 12px in
/// Carbon).
const EDITOR_HEIGHT: f32 = 164.0;
const MESSAGE_RULE_TOP: f32 = 395.0;
const RECIPIENTS_TOP: f32 = 419.0;
const CONTACTS_TOP: f32 = 447.0;
const DOMAIN_TOP: f32 = 475.0;
/// A checkbox's 14px and the 10px after it.
const CHECKBOX_TEXT_LEFT: f32 = 24.0;
const ACTIONS_TOP: f32 = 507.0;
/// The actions' `margin-right: -13px`, 8px between the secondary one and the next.
const ACTIONS_RIGHT: f32 = 13.0;
const ACTIONS_GAP: f32 = 8.0;
/// Down to the footer's `margin-bottom: 7px`.
const BODY_HEIGHT: f32 = 539.0;
/// While loading, skeletons stand in for the dates (20px under the rule) and the subject (12px
/// down), and the message's label sits 4px lower (`padding-top: 28px`), so everything from the
/// subject's label down ends up 1px lower.
const LOADING_DATES: f32 = 77.0;
const LOADING_SUBJECT: f32 = 149.0;
const LOADING_SHIFT: f32 = 1.0;

/// Outlook dialog row metrics (same 550px forced-light dialog shell as Gmail).
const OUTLOOK_FIRST_DAY_TOP: f32 = 79.0;
const OUTLOOK_LAST_DAY_TOP: f32 = 109.0;
const OUTLOOK_AT_GAP: f32 = 8.0;
const OUTLOOK_TIME_WIDTH: f32 = 66.0;
const OUTLOOK_INTERNAL_LABEL_TOP: f32 = 145.0;
const OUTLOOK_INTERNAL_EDITOR_TOP: f32 = 173.0;
const OUTLOOK_EDITOR_HEIGHT: f32 = 112.0;
const OUTLOOK_INTERNAL_RULE_TOP: f32 = 293.0;
const OUTLOOK_EXTERNAL_TOP: f32 = 313.0;
const OUTLOOK_CONTACTS_TOP: f32 = 345.0;
const OUTLOOK_EXTERNAL_EDITOR_TOP: f32 = 373.0;
const OUTLOOK_EXTERNAL_RULE_TOP: f32 = 493.0;
const OUTLOOK_ACTIONS_EXPANDED_TOP: f32 = 513.0;
const OUTLOOK_BODY_EXPANDED_HEIGHT: f32 = 545.0;
const OUTLOOK_ACTIONS_COMPACT_TOP: f32 = 353.0;
const OUTLOOK_BODY_COMPACT_HEIGHT: f32 = 385.0;

/// Time input styling matching `date_input.rs`.
const TIME_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 230);
const TIME_INK_DISABLED: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 102);
const TIME_RULE_FOCUSED: Color32 = Color32::from_rgb(0xae, 0xb1, 0xdd);
const TIME_RULE_ERROR: Color32 = Color32::from_rgb(0xf6, 0x7b, 0x7b);
const TIME_RULE_GAP: f32 = 3.0;

/// The subject takes 250 characters.
const SUBJECT_MAX: usize = 250;

/// The form's fields (the dialog's state).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    /// `Enable Out of Office Reply`.
    pub enabled: bool,
    /// Outlook: whether automatic replies are scheduled for a specific window (`Scheduled` vs
    /// `AlwaysEnabled`). Always `false` on Gmail.
    pub scheduled: bool,
    /// The first day's start (local midnight on Gmail; scheduled start date + time on Outlook).
    pub start: Timestamp,
    /// The last day's end (local `23:59:59` on Gmail if set; scheduled end date + time on Outlook).
    pub end: Option<Timestamp>,
    /// `Subject` (Gmail only).
    pub subject: String,
    /// `Message` (Gmail) or internal reply message (Outlook), as the editor writes it.
    pub body: String,
    /// `People in my contacts` (Gmail) or `Only send replies to people in my contacts` (Outlook).
    pub contacts: bool,
    /// `People at {domain}` (Gmail only).
    pub domain: bool,
    /// Outlook: `Send automatic replies outside of …`. Always `false` on Gmail.
    pub external: bool,
    /// Outlook: external reply message, as the editor writes it. Empty on Gmail.
    pub external_body: String,
}

impl Form {
    /// What `copy` fills the Gmail form with at `now`: its days when they're still to come or the
    /// reply is on, else today and no last day.
    pub fn of(copy: Option<&OutOfOfficeResponder>, now: &Zoned) -> Self {
        let today = midnight(now.date(), now);
        let copy = copy.cloned().unwrap_or_default();
        let kept = copy
            .start_time
            .and_then(|ms| Timestamp::from_millisecond(ms).ok())
            .filter(|start| *start >= today || copy.is_enabled());
        Self {
            enabled: copy.is_enabled(),
            scheduled: false,
            start: kept.unwrap_or(today),
            end: kept
                .and(copy.end_time)
                .and_then(|ms| Timestamp::from_millisecond(ms).ok())
                .map(|end| end - SignedDuration::from_secs(1)),
            subject: copy.google.response_subject,
            body: Editor::from_html(&copy.google.response_body_html).html(),
            contacts: copy.google.restrict_to_contacts,
            domain: copy.google.restrict_to_domain,
            external: false,
            external_body: String::new(),
        }
    }

    /// What `copy` fills the Outlook form with at `now`.
    pub fn of_outlook(
        copy: Option<&OutOfOfficeResponder>,
        outlook_personal: bool,
        now: &Zoned,
    ) -> Self {
        let (default_start, default_end) = default_outlook_window(now);
        let today = midnight(now.date(), now);
        let copy = copy.cloned().unwrap_or_default();
        let (enabled, scheduled, external, contacts, body, external_body) = match &copy.outlook {
            Some(outlook) => {
                let internal = Editor::from_html(&outlook.internal_reply_html).html();
                let external_html = Editor::from_html(&outlook.external_reply_html).html();
                if outlook_personal {
                    let body = if outlook.internal_reply_html.trim().is_empty() {
                        external_html
                    } else {
                        internal
                    };
                    (
                        outlook.status != AutoReplyStatus::Disabled,
                        outlook.status == AutoReplyStatus::Scheduled,
                        false,
                        outlook.external_audience == ExternalAudience::ContactsOnly,
                        body,
                        String::new(),
                    )
                } else {
                    (
                        outlook.status != AutoReplyStatus::Disabled,
                        outlook.status == AutoReplyStatus::Scheduled,
                        outlook.external_audience != ExternalAudience::None,
                        outlook.external_audience == ExternalAudience::ContactsOnly,
                        internal,
                        external_html,
                    )
                }
            }
            None => (false, false, false, false, String::new(), String::new()),
        };
        let kept_start = copy
            .start_time
            .and_then(|ms| Timestamp::from_millisecond(ms).ok())
            .filter(|start| *start >= today || scheduled);
        let kept_end = kept_start
            .and(copy.end_time)
            .and_then(|ms| Timestamp::from_millisecond(ms).ok());
        let start = kept_start.unwrap_or(default_start);
        let end = kept_end
            .filter(|end| *end > start)
            .or_else(|| Some(next_day_same_time(start, now)))
            .or(Some(default_end));
        Self {
            enabled,
            scheduled,
            start,
            end,
            subject: String::new(),
            body,
            contacts,
            domain: false,
            external,
            external_body,
        }
    }

    /// What Save sends Gmail: the days as Gmail's `startTime` and `endTime`, a second past the last
    /// day's last second.
    pub fn vacation(&self) -> VacationSettings {
        VacationSettings {
            enable_auto_reply: Some(self.enabled),
            response_subject: Some(self.subject.clone()),
            response_body_html: Some(self.body.clone()),
            response_body_plain_text: None,
            restrict_to_contacts: Some(self.contacts),
            restrict_to_domain: Some(self.domain),
            start_time: Some(self.start.as_millisecond()),
            end_time: self
                .end
                .map(|end| (end + SignedDuration::from_secs(1)).as_millisecond()),
        }
    }

    /// What Save sends Outlook (`mailboxSettings.automaticRepliesSetting`).
    pub fn outlook_auto_reply(&self, outlook_personal: bool) -> OutlookAutoReply {
        let status = if !self.enabled {
            AutoReplyStatus::Disabled
        } else if self.scheduled {
            AutoReplyStatus::Scheduled
        } else {
            AutoReplyStatus::AlwaysEnabled
        };
        let (external_audience, external_reply_html) = if outlook_personal {
            let audience = if self.contacts {
                ExternalAudience::ContactsOnly
            } else {
                ExternalAudience::All
            };
            (audience, self.body.clone())
        } else {
            let audience = if !self.external {
                ExternalAudience::None
            } else if self.contacts {
                ExternalAudience::ContactsOnly
            } else {
                ExternalAudience::All
            };
            (audience, self.external_body.clone())
        };
        let (start_time, end_time) = if status == AutoReplyStatus::Scheduled {
            (
                Some(self.start.as_millisecond()),
                self.end.map(Timestamp::as_millisecond),
            )
        } else {
            (None, None)
        };
        OutlookAutoReply {
            status,
            external_audience,
            internal_reply_html: self.body.clone(),
            external_reply_html,
            start_time,
            end_time,
        }
    }
}

/// A focusable control, in tab order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// `Subject` (Gmail only).
    Subject,
    /// `Message` (Gmail) or internal reply message (Outlook).
    Message,
    /// `People in my contacts` (Gmail) or `Only send replies to people in my contacts` (Outlook).
    Contacts,
    /// `People at {domain}` (Gmail only).
    Domain,
    /// Save.
    Save,
    /// Cancel.
    Cancel,
    /// The toggle's switch.
    Switch,
    /// Outlook: schedule checkbox beside the First/Last day rows.
    Schedule,
    /// `First day:`.
    Start,
    /// Outlook: `First day:` time.
    StartTime,
    /// `Last day:`.
    End,
    /// Outlook: `Last day:` time.
    EndTime,
    /// Outlook: `Send automatic replies outside of …`.
    External,
    /// Outlook: external reply message.
    ExternalMessage,
}

const TAB_ORDER: [Control; 9] = [
    Control::Subject,
    Control::Message,
    Control::Contacts,
    Control::Domain,
    Control::Save,
    Control::Cancel,
    Control::Switch,
    Control::Start,
    Control::End,
];

const OUTLOOK_TAB_ORDER: [Control; 12] = [
    Control::Switch,
    Control::Schedule,
    Control::Start,
    Control::StartTime,
    Control::End,
    Control::EndTime,
    Control::Message,
    Control::External,
    Control::Contacts,
    Control::ExternalMessage,
    Control::Save,
    Control::Cancel,
];

/// What happened in the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Cancel, the ×, the backdrop or Escape.
    Close,
    /// Save.
    Save,
    /// Tab (1) or ⇧Tab (-1).
    Tab(isize),
    /// Enter on the focused control.
    Enter,
    /// Space on the focused control.
    Space,
    /// ↓ in a date input, which opens its month.
    Down,
    /// A file was pasted in the message.
    PastedFile,
}

/// The dialog on screen, or fading out.
pub struct Dialog {
    /// It hasn't been closed.
    pub open: bool,
    /// The provider hasn't answered yet.
    pub loading: bool,
    /// Save is waiting on the provider.
    pub submitting: bool,
    /// The form.
    pub form: Form,
    /// Whether this dialog edits Outlook automatic replies instead of Gmail's vacation responder.
    outlook: bool,
    /// What the copy filled it with, for the discard check, with the toggle as the copy had it.
    initial: Form,
    /// The days the nudge opened it with.
    days: Option<OutOfOfficeDays>,
    editor: Editor,
    external_editor: Editor,
    /// The detected date the internal message's caret is on.
    date: Option<ActiveDate>,
    /// The detected date the external message's caret is on (Outlook).
    external_date: Option<ActiveDate>,
    /// A date the caret came onto, or whose text changed under it, for the app to show its day.
    day: Option<String>,
    start: DateInput,
    end: DateInput,
    /// Outlook scheduled start/end time text fields and parse errors.
    start_time_text: String,
    end_time_text: String,
    start_time_error: bool,
    end_time_error: bool,
    focus: Option<Control>,
    /// `People at {domain}` (Gmail, unless `gmail.com`), or organization domain on Outlook
    /// (`None` for personal `outlook.com` / `hotmail.com` / `live.com` / `msn.com` accounts).
    domain: Option<String>,
    scroll: f32,
    scrollbar: Scrollbar,
    external_scroll: f32,
    external_scrollbar: Scrollbar,
}

impl Dialog {
    /// The Gmail dialog for `email`'s account, loading, with `days` from the nudge.
    #[cfg(test)]
    pub fn new(email: &str, days: Option<OutOfOfficeDays>, now: &Zoned) -> Self {
        Self::new_for(email, false, days, now)
    }

    /// The Gmail or Outlook dialog for `email`'s account, loading, with `days` from the nudge.
    pub fn new_for(email: &str, outlook: bool, days: Option<OutOfOfficeDays>, now: &Zoned) -> Self {
        let domain = if outlook {
            email
                .rsplit_once('@')
                .map(|(_, domain)| domain.to_lowercase())
                .filter(|domain| {
                    !domain.is_empty()
                        && !matches!(
                            domain.as_str(),
                            "outlook.com" | "hotmail.com" | "live.com" | "msn.com"
                        )
                })
        } else {
            email
                .rsplit_once('@')
                .map(|(_, domain)| domain.to_owned())
                .filter(|domain| !domain.eq_ignore_ascii_case("gmail.com"))
        };
        let outlook_personal = outlook && domain.is_none();
        let form = if outlook {
            Form::of_outlook(None, outlook_personal, now)
        } else {
            Form::of(None, now)
        };
        let mut editor = Editor::new();
        editor.set_body_text(BodyText::SMALL);
        let mut external_editor = Editor::new();
        external_editor.set_body_text(BodyText::SMALL);
        let mut dialog = Self {
            open: true,
            loading: true,
            submitting: false,
            outlook,
            initial: form.clone(),
            start: DateInput::default(),
            end: DateInput::default(),
            start_time_text: String::new(),
            end_time_text: String::new(),
            start_time_error: false,
            end_time_error: false,
            form,
            days,
            editor,
            external_editor,
            date: None,
            external_date: None,
            day: None,
            focus: None,
            domain,
            scroll: 0.0,
            scrollbar: Scrollbar::default(),
            external_scroll: 0.0,
            external_scrollbar: Scrollbar::default(),
        };
        dialog.show_dates(now);
        dialog
    }

    /// Whether this is a personal Outlook account (`outlook.com` / `hotmail.com` / `live.com` /
    /// `msn.com`), which omits the external reply slider and editor.
    fn is_outlook_personal(&self) -> bool {
        self.outlook && self.domain.is_none()
    }

    /// The provider answered and `copy` is up to date. The form takes it, and the nudge's days with
    /// the reply on.
    pub fn loaded(&mut self, copy: Option<&OutOfOfficeResponder>, now: &Zoned) {
        self.loading = false;
        self.initial = if self.outlook {
            Form::of_outlook(copy, self.is_outlook_personal(), now)
        } else {
            Form::of(copy, now)
        };
        self.form = self.initial.clone();
        if let Some(days) = &self.days {
            self.form.start = days.start;
            self.form.end = Some(days.end);
            self.form.enabled = true;
            if self.outlook {
                self.form.scheduled = true;
            }
        }
        self.editor.set_html(&self.form.body);
        if self.outlook && !self.is_outlook_personal() {
            self.external_editor.set_html(&self.form.external_body);
        }
        self.show_dates(now);
    }

    /// The form differs from what it was filled with.
    pub fn changed(&self) -> bool {
        !self.loading && self.form != self.initial
    }

    /// Whether the scheduled Outlook window has an end at or before its start.
    fn end_before_or_eq_start(&self) -> bool {
        self.outlook
            && self.form.scheduled
            && self.form.end.is_none_or(|end| end <= self.form.start)
    }

    /// Save can't be pressed when it's waiting, when turning off an already-off reply, or (while
    /// enabled) when required fields are empty or a typed date/time is wrong. Turning off an active
    /// reply stays allowed even if disabled fields hold an empty message or date error.
    pub fn save_disabled(&self) -> bool {
        if self.loading || self.submitting {
            return true;
        }
        if !self.form.enabled {
            return !self.initial.enabled;
        }
        if self.outlook {
            let invalid_schedule = self.form.scheduled
                && (self.start.error
                    || self.end.error
                    || self.start_time_error
                    || self.end_time_error
                    || self.end_before_or_eq_start());
            let missing_external = !self.is_outlook_personal()
                && self.form.external
                && self.external_editor.is_empty();
            self.editor.is_empty() || missing_external || invalid_schedule
        } else {
            self.form.subject.is_empty()
                || self.editor.is_empty()
                || self.start.error
                || self.end.error
        }
    }

    /// What Save sends to Gmail or Outlook.
    pub fn auto_reply(&self) -> AutoReply {
        if self.outlook {
            AutoReply::Outlook(self.form.outlook_auto_reply(self.is_outlook_personal()))
        } else {
            AutoReply::Gmail(self.form.vacation())
        }
    }

    /// The control with the keyboard.
    pub fn focus(&self) -> Option<Control> {
        self.focus
    }

    /// The currently focused rich-text editor, if either the internal or external message editor
    /// has keyboard focus.
    pub fn focused_editor(&self) -> Option<&Editor> {
        match self.focus {
            Some(Control::Message) => Some(&self.editor),
            Some(Control::ExternalMessage) if self.outlook => Some(&self.external_editor),
            _ => None,
        }
    }

    /// Whether a focused message editor has something of its own for Escape.
    pub fn editor_wants_escape(&self) -> bool {
        self.focused_editor().is_some_and(Editor::wants_escape)
    }

    /// Whether the focused message's caret is on a detected date, which ⌘⇧– and ⌘⇧= move.
    pub fn on_date(&self) -> bool {
        match self.focus {
            Some(Control::Message) => self.date.is_some(),
            Some(Control::ExternalMessage) if self.outlook => self.external_date.is_some(),
            _ => false,
        }
    }

    /// The date a message's caret came onto since the last call, to show its day.
    pub fn take_day(&mut self) -> Option<String> {
        self.day.take()
    }

    /// Runs a formatting command on whichever message editor has focus and keeps `self.form` in
    /// sync.
    pub fn run_format(&mut self, command: FormatCommand) {
        match self.focus {
            Some(Control::ExternalMessage) if self.outlook => {
                command.run(&mut self.external_editor);
                self.form.external_body = self.external_editor.html();
            }
            Some(Control::Message) => {
                command.run(&mut self.editor);
                self.form.body = self.editor.html();
            }
            _ => {}
        }
    }

    /// Moves the date the focused message's caret is on by `days` from its day `today`, as compose
    /// does.
    pub fn shift_date(&mut self, days: i64, today: &Zoned) {
        match self.focus {
            Some(Control::ExternalMessage) if self.outlook => {
                if let Some(date) = self.external_editor.active_date()
                    && let Some(text) = crate::thread::dates::shift(&date.text, days, today)
                {
                    self.external_editor.shift_date(&date, &text);
                    self.form.external_body = self.external_editor.html();
                }
            }
            _ => {
                if let Some(date) = self.editor.active_date()
                    && let Some(text) = crate::thread::dates::shift(&date.text, days, today)
                {
                    self.editor.shift_date(&date, &text);
                    self.form.body = self.editor.html();
                }
            }
        }
    }

    /// The primary/internal editor, for spelling and formatting.
    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// The primary/internal editor.
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    /// The Outlook external reply editor, when this is an Outlook dialog.
    pub fn external_editor_mut(&mut self) -> Option<&mut Editor> {
        self.outlook.then_some(&mut self.external_editor)
    }

    /// The Outlook external reply editor, when this is an Outlook dialog.
    pub fn external_editor(&self) -> Option<&Editor> {
        self.outlook.then_some(&self.external_editor)
    }

    /// Flips the reply on or off.
    pub fn toggle(&mut self) {
        if !self.loading {
            self.form.enabled = !self.form.enabled;
            if !self.form.enabled {
                self.start.month = None;
                self.end.month = None;
                self.focus = self
                    .focus
                    .filter(|control| matches!(control, Control::Switch | Control::Cancel));
            }
        }
    }

    /// Toggles Outlook's scheduled time-window checkbox.
    fn toggle_scheduled(&mut self, now: &Zoned) {
        self.form.scheduled = !self.form.scheduled;
        if self.form.scheduled {
            if self.form.end.is_none_or(|end| end <= self.form.start) {
                self.form.end = Some(next_day_same_time(self.form.start, now));
            }
            self.show_dates(now);
        } else {
            self.start.month = None;
            self.end.month = None;
            if matches!(
                self.focus,
                Some(Control::Start | Control::StartTime | Control::End | Control::EndTime)
            ) {
                self.focus = Some(Control::Schedule);
            }
        }
    }

    /// Toggles Outlook's `Send automatic replies outside of …` slider.
    fn toggle_external(&mut self) {
        self.form.external = !self.form.external;
        if !self.form.external
            && matches!(
                self.focus,
                Some(Control::Contacts | Control::ExternalMessage)
            )
        {
            self.focus = Some(Control::External);
        }
    }

    /// Moves the keyboard `step` controls along the tab order, skipping disabled ones.
    pub fn tab(&mut self, step: isize, now: &Zoned) {
        // Forward Tab in a date input takes what's typed; on Start, picking advances forward
        // directly to End (Gmail) or StartTime (Outlook). Backward Shift+Tab commits without
        // jumping forward so Start never traps reverse navigation.
        let from = self.focus;
        if step > 0 {
            if self.commit(now) && from == Some(Control::Start) {
                return;
            }
        } else if let Some(control) = from {
            self.commit_control(control, now);
        }
        let order: &[Control] = if self.outlook {
            &OUTLOOK_TAB_ORDER
        } else {
            &TAB_ORDER
        };
        let available: Vec<Control> = order
            .iter()
            .copied()
            .filter(|control| self.focusable(*control))
            .collect();
        if available.is_empty() {
            return;
        }
        let at = self
            .focus
            .and_then(|focus| available.iter().position(|control| *control == focus));
        let len = available.len() as isize;
        let next = match at {
            Some(at) => (at as isize + step).rem_euclid(len),
            None if step > 0 => 0,
            None => len - 1,
        };
        self.set_focus(Some(available[next as usize]), now);
    }

    fn focusable(&self, control: Control) -> bool {
        match control {
            Control::Switch => !self.loading,
            Control::Cancel => true,
            Control::Save => !self.save_disabled(),
            Control::Subject => !self.outlook && self.form.enabled && !self.loading,
            Control::Domain => {
                !self.outlook && self.form.enabled && !self.loading && self.domain.is_some()
            }
            Control::Schedule => self.outlook && self.form.enabled && !self.loading,
            Control::External => {
                self.outlook && !self.is_outlook_personal() && self.form.enabled && !self.loading
            }
            Control::Start | Control::End => {
                self.form.enabled && !self.loading && (!self.outlook || self.form.scheduled)
            }
            Control::StartTime | Control::EndTime => {
                self.outlook && self.form.enabled && !self.loading && self.form.scheduled
            }
            Control::Contacts => {
                self.form.enabled
                    && !self.loading
                    && (!self.outlook || self.is_outlook_personal() || self.form.external)
            }
            Control::ExternalMessage => {
                self.outlook
                    && !self.is_outlook_personal()
                    && self.form.enabled
                    && !self.loading
                    && self.form.external
            }
            Control::Message => self.form.enabled && !self.loading,
        }
    }

    fn set_focus(&mut self, focus: Option<Control>, now: &Zoned) {
        if focus == self.focus {
            return;
        }
        let previous = self.focus;
        self.focus = focus;
        if let Some(control) = previous {
            self.commit_control(control, now);
        }
        // A date input's month opens when it gets the keyboard and closes when it loses it.
        let today = now.date();
        for (control, input) in [
            (Control::Start, &mut self.start),
            (Control::End, &mut self.end),
        ] {
            input.month =
                (focus == Some(control)).then(|| input.parsed.unwrap_or(today).first_of_month());
        }
    }

    /// Commits any valid typed date or time in `control` without changing focus.
    fn commit_control(&mut self, control: Control, now: &Zoned) {
        let tz = now.time_zone().clone();
        match control {
            Control::Start => {
                if let Some(date) = self.start.parsed.filter(|_| !self.start.error)
                    && date != self.form.start.to_zoned(tz).date()
                {
                    self.apply_start(date, now);
                }
            }
            Control::End => {
                let current = self.form.end.map(|end| end.to_zoned(tz).date());
                if let Some(date) = self.end.parsed.filter(|_| !self.end.error)
                    && Some(date) != current
                {
                    self.apply_end(Some(date), now);
                }
            }
            Control::StartTime if self.outlook => self.commit_time(true, now),
            Control::EndTime if self.outlook => self.commit_time(false, now),
            _ => {}
        }
    }

    /// Commits any valid typed time in the Start or End time input and formats it cleanly.
    fn commit_time(&mut self, start: bool, now: &Zoned) {
        let tz = now.time_zone().clone();
        let reference = if start {
            self.form.start.to_zoned(tz.clone())
        } else {
            self.form
                .end
                .unwrap_or(self.form.start)
                .to_zoned(tz.clone())
        };
        let text = if start {
            &self.start_time_text
        } else {
            &self.end_time_text
        };
        let Some((hour, minute)) = parse_time(text, &reference) else {
            if start {
                self.start_time_error = true;
            } else {
                self.end_time_error = true;
            }
            return;
        };
        if start {
            let date = reference.date();
            if let Ok(zoned) = date.at(hour, minute, 0, 0).to_zoned(tz) {
                self.form.start = zoned.timestamp();
                self.start_time_text = crate::format::dates::time(&zoned);
                self.start_time_error = false;
            }
        } else {
            let date = reference.date();
            if let Ok(zoned) = date.at(hour, minute, 0, 0).to_zoned(tz) {
                self.form.end = Some(zoned.timestamp());
                self.end_time_text = crate::format::dates::time(&zoned);
                self.end_time_error = false;
            }
        }
    }

    /// Commits any valid typed dates and times (called right before saving).
    pub fn commit_dates(&mut self, now: &Zoned) {
        self.commit_control(Control::Start, now);
        self.commit_control(Control::End, now);
        if self.outlook && self.form.scheduled {
            self.commit_time(true, now);
            self.commit_time(false, now);
        }
    }

    /// Enter on the focused control.
    pub fn enter(&mut self, now: &Zoned) -> Option<Event> {
        match self.focus? {
            Control::Switch => self.toggle(),
            Control::Schedule => self.toggle_scheduled(now),
            Control::External => self.toggle_external(),
            Control::Contacts => self.form.contacts = !self.form.contacts,
            Control::Domain => self.form.domain = !self.form.domain,
            Control::Save => {
                self.commit_dates(now);
                return Some(Event::Save);
            }
            Control::Cancel => return Some(Event::Close),
            Control::Start | Control::End => {
                self.commit(now);
            }
            Control::StartTime => self.commit_time(true, now),
            Control::EndTime => self.commit_time(false, now),
            Control::Subject | Control::Message | Control::ExternalMessage => {}
        }
        None
    }

    /// Space on the focused control: what Enter does, except in text/date/time inputs.
    pub fn space(&mut self, now: &Zoned) -> Option<Event> {
        match self.focus? {
            Control::Start
            | Control::End
            | Control::StartTime
            | Control::EndTime
            | Control::Subject
            | Control::Message
            | Control::ExternalMessage => None,
            _ => self.enter(now),
        }
    }

    /// ↓ in a date input opens its month.
    pub fn down(&mut self, now: &Zoned) {
        let today = now.date();
        let input = match self.focus {
            Some(Control::Start) => &mut self.start,
            Some(Control::End) => &mut self.end,
            _ => return,
        };
        if input.month.is_none() {
            input.month = Some(input.parsed.unwrap_or(today).first_of_month());
        }
    }

    /// Takes what's typed in the focused date input, when it's a date (Enter, Tab). Returns
    /// whether it did.
    fn commit(&mut self, now: &Zoned) -> bool {
        let (input, start) = match self.focus {
            Some(Control::Start) => (&self.start, true),
            Some(Control::End) => (&self.end, false),
            _ => return false,
        };
        let Some(date) = input.parsed.filter(|_| !input.error) else {
            return false;
        };
        if start {
            self.pick_start(date, now);
        } else {
            // The month closes, and the input keeps the keyboard.
            self.pick_end(Some(date), now);
            self.end.month = None;
        }
        true
    }

    /// Moves the start date to `date`, shifting an end date now before it by the same number of
    /// civil days (ending at `23:59:59` on Gmail, or preserving wall time on Outlook).
    fn apply_start(&mut self, date: Date, now: &Zoned) {
        let tz = now.time_zone().clone();
        let start = if self.outlook {
            with_date(self.form.start, date, now)
        } else {
            midnight(date, now)
        };
        if let Some(end) = self.form.end.filter(|end| {
            if self.outlook {
                start >= *end
            } else {
                start > *end
            }
        }) {
            let old_start = self.form.start.to_zoned(tz.clone()).date();
            let old_end = end.to_zoned(tz).date();
            let shifted = old_start
                .until(old_end)
                .ok()
                .filter(|span| !span.is_negative())
                .and_then(|span| date.checked_add(span).ok())
                .unwrap_or(date);
            self.form.end = if self.outlook {
                let candidate = with_date(end, shifted, now);
                if candidate > start {
                    Some(candidate)
                } else {
                    Some(next_day_same_time(start, now))
                }
            } else {
                end_of_day(shifted, now)
            };
        }
        self.form.start = start;
        self.start.error = false;
        self.show_dates(now);
    }

    /// Picks the start date, shifting end along if needed, and advances focus.
    fn pick_start(&mut self, date: Date, now: &Zoned) {
        self.apply_start(date, now);
        let next = if self.outlook {
            Control::StartTime
        } else {
            Control::End
        };
        self.set_focus(Some(next), now);
    }

    /// Moves the end date to `date` (ending at `23:59:59` on Gmail, or preserving wall time on
    /// Outlook).
    fn apply_end(&mut self, date: Option<Date>, now: &Zoned) {
        self.form.end = if self.outlook {
            date.map(|date| {
                let fallback = self.form.end.unwrap_or(self.form.start);
                with_date(fallback, date, now)
            })
        } else {
            date.and_then(|date| end_of_day(date, now))
        };
        self.end.error = false;
        self.show_dates(now);
    }

    /// Picks the last day. The input keeps the keyboard, with its month closed.
    fn pick_end(&mut self, date: Option<Date>, now: &Zoned) {
        self.apply_end(date, now);
    }

    /// Refreshes the date (and Outlook time) input strings from `self.form`.
    fn show_dates(&mut self, now: &Zoned) {
        let tz = now.time_zone().clone();
        let today = now.date();
        let start_zoned = self.form.start.to_zoned(tz.clone());
        let end_zoned = self.form.end.map(|end| end.to_zoned(tz));
        let start = start_zoned.date();
        let end = end_zoned.as_ref().map(Zoned::date);
        let (start_month, end_month) = (
            self.start.month.map(|_| start.first_of_month()),
            self.end.month.and(end.map(Date::first_of_month)),
        );
        self.start = DateInput {
            month: start_month,
            ..DateInput::showing(Some(start), today)
        };
        self.end = DateInput {
            month: end_month,
            ..DateInput::showing(end, today)
        };
        if self.outlook {
            self.start_time_text = crate::format::dates::time(&start_zoned);
            self.start_time_error = false;
            self.end_time_text = end_zoned
                .as_ref()
                .map(crate::format::dates::time)
                .unwrap_or_default();
            self.end_time_error = false;
        }
    }

    /// Whether Outlook's expanded external section (contacts checkbox + external message editor)
    /// is shown this frame.
    fn show_outlook_external_section(&self) -> bool {
        self.outlook && !self.is_outlook_personal() && !self.loading && self.form.external
    }

    /// Draws the dialog over `area` and returns what happened.
    pub fn show(&mut self, ctx: &Context, area: Rect, now: &Zoned) -> Option<Option<Event>> {
        let carbon = exo_ui::theme(ctx) == exo_ui::Theme::Carbon;
        let height = if self.outlook {
            if self.show_outlook_external_section() {
                OUTLOOK_BODY_EXPANDED_HEIGHT
            } else {
                OUTLOOK_BODY_COMPACT_HEIGHT
            }
        } else if self.loading {
            BODY_HEIGHT + LOADING_SHIFT
        } else {
            BODY_HEIGHT
        };
        let response = exo_ui::Dialog::new("out-of-office-reply", "Out of Office Reply", height)
            .header_style(HEADER, TEXT_DEFAULT)
            .show(ctx, area, self.open, |ui, body| {
                if self.outlook {
                    self.body_outlook(ui, body, now, carbon)
                } else {
                    self.body_gmail(ui, body, now, carbon)
                }
            });
        let inner = response.inner?;
        Some(if response.close {
            Some(Event::Close)
        } else {
            inner
        })
    }

    /// Draws a labelled slider switch row at `label_top` and returns whether it was clicked while
    /// enabled.
    #[expect(
        clippy::too_many_arguments,
        reason = "slider row geometry, label, and state"
    )]
    fn show_slider_row(
        &self,
        ui: &mut Ui,
        id: Id,
        control: Control,
        left: f32,
        label_top: f32,
        label: &str,
        checked: bool,
        disabled: bool,
    ) -> bool {
        let painter = ui.painter();
        let label_ink = if disabled { TEXT_SUBDUED } else { TEXT_DEFAULT };
        let label_width = LABEL.paint_line(painter, pos2(left, label_top), label, label_ink, None);
        // The switch's filtered layer lands on a whole CSS pixel (632.66 paints at 633).
        let switch = Rect::from_min_size(
            pos2((left + label_width + SWITCH_GAP).round(), label_top + 1.0),
            exo_ui::SLIDER_SIZE,
        );
        if self.focus == Some(control) {
            // Focused: Classic's `box-shadow: 0 0 0 3px rgba(0,0,0,.1)`.
            painter.rect_filled(
                switch.expand(3.0),
                CornerRadius::same(11),
                Color32::from_black_alpha(26),
            );
        }
        exo_ui::paint_slider(
            painter,
            switch,
            checked,
            disabled,
            id.with(("slider", control as u8)),
            SWITCH,
        );
        !disabled
            && ui
                .interact(
                    switch,
                    id.with(("slider-hit", control as u8)),
                    Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
    }

    /// Draws the top toggle row (`Enable Out of Office Reply` + slider) and divider `<hr>`.
    /// Returns whether the switch was clicked.
    fn show_toggle_header(&mut self, ui: &mut Ui, id: Id, body: Rect, now: &Zoned) -> bool {
        let (left, top, width) = (body.left(), body.top(), body.width());
        let clicked = self.show_slider_row(
            ui,
            id,
            Control::Switch,
            left,
            top + TOGGLE_TOP,
            "Enable Out of Office Reply",
            self.form.enabled,
            self.loading,
        );
        if clicked {
            self.toggle();
            self.set_focus(Some(Control::Switch), now);
        }
        ui.painter().rect_filled(
            Rect::from_min_size(pos2(left, top + HR_TOP), vec2(width, 1.0)),
            0.0,
            RULE,
        );
        clicked
    }

    /// Draws Cancel and Save at `actions_top` (measured from `body.top()`).
    fn show_footer(
        &mut self,
        ui: &mut Ui,
        id: Id,
        body: Rect,
        actions_top: f32,
        now: &Zoned,
    ) -> (Option<Event>, bool) {
        let ctx = ui.ctx().clone();
        let save = DialogAction::new("Save", ActionKind::Primary)
            .origin_light()
            .enabled(!self.save_disabled())
            .focused(self.focus == Some(Control::Save));
        let cancel = DialogAction::new("Cancel", ActionKind::Plain)
            .origin_light()
            .focused(self.focus == Some(Control::Cancel));
        let right = body.right() + ACTIONS_RIGHT;
        let top = body.top() + actions_top;
        let save_rect = Rect::from_min_max(
            pos2(right - save.width(&ctx), top),
            pos2(right, top + exo_ui::DIALOG_ACTION_HEIGHT),
        );
        let cancel_right = save_rect.left() - ACTIONS_GAP;
        let cancel_rect = Rect::from_min_max(
            pos2(cancel_right - cancel.width(&ctx), save_rect.top()),
            pos2(cancel_right, save_rect.bottom()),
        );
        let mut event = None;
        let mut claimed = false;
        if cancel.show(ui, id.with("cancel"), cancel_rect).clicked() {
            event = Some(Event::Close);
            claimed = true;
        }
        if save.show(ui, id.with("save"), save_rect).clicked() {
            self.commit_dates(now);
            event = Some(Event::Save);
            claimed = true;
        }
        (event, claimed)
    }

    /// Draws one checkbox row at `line_top` and returns whether it was clicked while enabled.
    #[expect(
        clippy::too_many_arguments,
        reason = "checkbox row geometry, label, and state"
    )]
    fn show_checkbox_row(
        &self,
        ui: &mut Ui,
        id: Id,
        control: Control,
        left: f32,
        line_top: f32,
        nudge: f32,
        label: &str,
        checked: bool,
        disabled: bool,
    ) -> bool {
        let painter = ui.painter();
        let hit_width = if label.is_empty() {
            18.0
        } else {
            let text_width = TEXT.paint_line(
                painter,
                pos2(left + CHECKBOX_TEXT_LEFT, line_top),
                label,
                if disabled { TEXT_SUBDUED } else { TEXT_DEFAULT },
                None,
            );
            CHECKBOX_TEXT_LEFT + text_width
        };
        let mark = pos2(left, line_top + 1.0 + nudge);
        if self.focus == Some(control) {
            // Focused: Classic's `outline: 5px solid rgba(0,0,0,.05)`, 3px corners.
            let ring = Rect::from_min_size(mark, vec2(14.0, 14.0)).expand(5.0);
            painter.rect_stroke(
                ring.shrink(2.5),
                CornerRadius::same(5),
                Stroke::new(5.0, Color32::from_black_alpha(13)),
                egui::StrokeKind::Middle,
            );
        }
        let inks = CHECKBOX.map(|ink| {
            if disabled {
                ink.gamma_multiply(0.5)
            } else {
                ink
            }
        });
        exo_ui::paint_checkbox_inks(painter, mark, checked, inks);
        let row = Rect::from_min_size(pos2(left, line_top), vec2(hit_width, TEXT.line_height));
        !disabled
            && ui
                .interact(row, id.with(("checkbox", control as u8)), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
    }

    fn body_gmail(&mut self, ui: &mut Ui, body: Rect, now: &Zoned, carbon: bool) -> Option<Event> {
        let ctx = ui.ctx().clone();
        let id = exo_ui::id(&ctx, "out-of-office-reply");
        let today = now.date();
        let enabled = self.form.enabled;
        let disabled = !enabled;
        let (left, top, width) = (body.left(), body.top(), body.width());
        let painter = ui.painter().clone();
        let mut event = None;
        let subdued_if = |off: bool| if off { TEXT_SUBDUED } else { TEXT_DEFAULT };
        let shift = if self.loading { LOADING_SHIFT } else { 0.0 };
        // A click that no control takes gives the keyboard back to the form.
        let clicked = ui.input(|input| input.pointer.any_click())
            && ctx
                .pointer_interact_pos()
                .is_some_and(|at| body.contains(at));
        let focus_before = self.focus;
        let mut claimed = self.show_toggle_header(ui, id, body, now);

        // The first and last day.
        if self.loading {
            skeleton(
                ui,
                Rect::from_min_size(
                    pos2(left, top + LOADING_DATES),
                    vec2(width, SKELETON_HEIGHT),
                ),
            );
        } else {
            let first_width = TEXT.paint_line(
                &painter,
                pos2(left, top + DATES_TOP),
                "First day:",
                subdued_if(disabled),
                None,
            );
            let mut start_field = date_input::Field {
                id: id.with("start"),
                input: &mut self.start,
                focused: self.focus == Some(Control::Start),
                disabled,
                error: false,
                placeholder: "",
                min: today,
                today,
            };
            let start_left = left + first_width + DATE_PADDING;
            let start_width = start_field.width(&ctx);
            let start_event = start_field.show(ui, pos2(start_left, top + DATE_INPUT_TOP));
            let last_left = start_left + start_width + DATE_PADDING + DATES_GAP;
            let last_width = TEXT.paint_line(
                &painter,
                pos2(last_left, top + DATES_TOP),
                "Last day:",
                subdued_if(disabled),
                None,
            );
            let start_date = self.form.start.to_zoned(now.time_zone().clone()).date();
            let mut end_field = date_input::Field {
                id: id.with("end"),
                input: &mut self.end,
                focused: self.focus == Some(Control::End),
                disabled,
                error: false,
                placeholder: "Select date",
                min: start_date,
                today,
            };
            let end_event = end_field.show(
                ui,
                pos2(last_left + last_width + DATE_PADDING, top + DATE_INPUT_TOP),
            );
            claimed |= start_event.is_some() || end_event.is_some();
            self.date_event(Control::Start, start_event, now);
            self.date_event(Control::End, end_event, now);
        }

        // The subject.
        LABEL.paint_line(
            &painter,
            pos2(left, top + SUBJECT_LABEL_TOP + shift),
            "Subject",
            subdued_if(disabled),
            None,
        );
        if self.loading {
            skeleton(
                ui,
                Rect::from_min_size(
                    pos2(left, top + LOADING_SUBJECT),
                    vec2(width, SKELETON_HEIGHT),
                ),
            );
        } else {
            let line =
                Rect::from_min_size(pos2(left, top + SUBJECT_TOP), vec2(width, TEXT.line_height));
            // The text input has white (`#f5f5f5` in Carbon) behind it.
            painter.rect_filled(
                line,
                0.0,
                if carbon {
                    Color32::from_rgb(0xf5, 0xf5, 0xf5)
                } else {
                    Color32::WHITE
                },
            );
            if disabled {
                // Disabled: the accent text color at `.4`.
                let ink = TEXT_ACCENT.gamma_multiply(0.4);
                let text = if self.form.subject.is_empty() {
                    "Enter subject"
                } else {
                    &self.form.subject
                };
                TEXT.paint_line(&painter, line.min, text, ink, Some(width));
            } else {
                if self.form.subject.is_empty() {
                    TEXT.paint_line(&painter, line.min, "Enter subject", TEXT_SUBDUED, None);
                }
                let field_id = id.with("subject");
                let field = crate::spelling::text_field(
                    ui,
                    field_id,
                    &mut self.form.subject,
                    line,
                    &TEXT,
                    TEXT_DEFAULT,
                    exo_ui::palette(&ctx).text_selection_default,
                );
                if self.form.subject.chars().count() > SUBJECT_MAX {
                    self.form.subject = self.form.subject.chars().take(SUBJECT_MAX).collect();
                }
                if field.clicked() || field.gained_focus() {
                    self.set_focus(Some(Control::Subject), now);
                    claimed = true;
                }
                hold_focus(ui, self.focus == Some(Control::Subject), field_id);
            }
        }

        // The message.
        LABEL.paint_line(
            &painter,
            pos2(left, top + MESSAGE_LABEL_TOP + shift),
            "Message",
            subdued_if(disabled),
            None,
        );
        let editor_area = Rect::from_min_size(
            pos2(left, top + EDITOR_TOP + 1.0 + shift),
            vec2(width, EDITOR_HEIGHT),
        );
        if self.loading {
            skeleton_lines(ui, editor_area.min, width);
        } else {
            let (pasted, focused) =
                self.show_editor(ui, Control::Message, editor_area, carbon, disabled, now);
            claimed |= focused;
            event = pasted.or(event);
        }
        painter.rect_filled(
            Rect::from_min_size(pos2(left, top + MESSAGE_RULE_TOP + shift), vec2(width, 1.0)),
            0.0,
            RULE,
        );

        // Who gets replies.
        TEXT.paint_line(
            &painter,
            pos2(left, top + RECIPIENTS_TOP + shift),
            "Only send automatic replies to:",
            subdued_if(disabled),
            None,
        );
        if self.show_checkbox_row(
            ui,
            id,
            Control::Contacts,
            left,
            top + CONTACTS_TOP + shift,
            -1.0,
            "People in my contacts",
            self.form.contacts,
            disabled,
        ) {
            self.form.contacts = !self.form.contacts;
            self.set_focus(Some(Control::Contacts), now);
            claimed = true;
        }
        if let Some(domain) = self.domain.clone() {
            let label = format!("People at {domain}");
            if self.show_checkbox_row(
                ui,
                id,
                Control::Domain,
                left,
                top + DOMAIN_TOP + shift,
                0.0,
                &label,
                self.form.domain,
                disabled,
            ) {
                self.form.domain = !self.form.domain;
                self.set_focus(Some(Control::Domain), now);
                claimed = true;
            }
        }

        // Cancel and Save.
        let (footer_event, footer_claimed) =
            self.show_footer(ui, id, body, ACTIONS_TOP + shift, now);
        claimed |= footer_claimed;
        event = footer_event.or(event);
        if clicked && !claimed && self.focus == focus_before {
            self.set_focus(None, now);
        }
        event
    }

    fn body_outlook(
        &mut self,
        ui: &mut Ui,
        body: Rect,
        now: &Zoned,
        carbon: bool,
    ) -> Option<Event> {
        let ctx = ui.ctx().clone();
        let id = exo_ui::id(&ctx, "out-of-office-reply");
        let today = now.date();
        let enabled = self.form.enabled;
        let disabled = !enabled;
        let schedule_disabled = disabled || !self.form.scheduled;
        let personal = self.is_outlook_personal();
        let (left, top, width) = (body.left(), body.top(), body.width());
        let painter = ui.painter().clone();
        let mut event = None;
        let subdued_if = |off: bool| if off { TEXT_SUBDUED } else { TEXT_DEFAULT };
        let clicked = ui.input(|input| input.pointer.any_click())
            && ctx
                .pointer_interact_pos()
                .is_some_and(|at| body.contains(at));
        let focus_before = self.focus;
        let mut claimed = self.show_toggle_header(ui, id, body, now);

        // Schedule checkbox directly beside the First day / Last day date & time rows.
        if self.loading {
            skeleton(
                ui,
                Rect::from_min_size(
                    pos2(left, top + OUTLOOK_FIRST_DAY_TOP + 2.0),
                    vec2(260.0, SKELETON_HEIGHT),
                ),
            );
            skeleton(
                ui,
                Rect::from_min_size(
                    pos2(left, top + OUTLOOK_LAST_DAY_TOP + 2.0),
                    vec2(260.0, SKELETON_HEIGHT),
                ),
            );
        } else {
            if self.show_checkbox_row(
                ui,
                id,
                Control::Schedule,
                left,
                top + OUTLOOK_FIRST_DAY_TOP,
                0.0,
                "",
                self.form.scheduled,
                disabled,
            ) {
                self.toggle_scheduled(now);
                self.set_focus(Some(Control::Schedule), now);
                claimed = true;
            }
            let row_left = left + CHECKBOX_TEXT_LEFT;
            let start_date = self.form.start.to_zoned(now.time_zone().clone()).date();
            let range_error = self.end_before_or_eq_start();

            let first_width = TEXT.paint_line(
                &painter,
                pos2(row_left, top + OUTLOOK_FIRST_DAY_TOP),
                "First day:",
                subdued_if(schedule_disabled),
                None,
            );
            let last_width = TEXT.paint_line(
                &painter,
                pos2(row_left, top + OUTLOOK_LAST_DAY_TOP),
                "Last day:",
                subdued_if(schedule_disabled),
                None,
            );
            let date_left = row_left + first_width.max(last_width) + DATE_PADDING;

            let start_width = {
                let field = date_input::Field {
                    id: id.with("start"),
                    input: &mut self.start,
                    focused: self.focus == Some(Control::Start),
                    disabled: schedule_disabled,
                    error: false,
                    placeholder: "",
                    min: today,
                    today,
                };
                field.width(&ctx)
            };
            let end_width = {
                let field = date_input::Field {
                    id: id.with("end"),
                    input: &mut self.end,
                    focused: self.focus == Some(Control::End),
                    disabled: schedule_disabled,
                    error: range_error,
                    placeholder: "",
                    min: start_date,
                    today,
                };
                field.width(&ctx)
            };
            let at_left = date_left + start_width.max(end_width) + DATE_PADDING + OUTLOOK_AT_GAP;
            let at_width = TEXT.paint_line(
                &painter,
                pos2(at_left, top + OUTLOOK_FIRST_DAY_TOP),
                "at",
                subdued_if(schedule_disabled),
                None,
            );
            TEXT.paint_line(
                &painter,
                pos2(at_left, top + OUTLOOK_LAST_DAY_TOP),
                "at",
                subdued_if(schedule_disabled),
                None,
            );
            let time_left = at_left + at_width + OUTLOOK_AT_GAP;

            let mut start_field = date_input::Field {
                id: id.with("start"),
                input: &mut self.start,
                focused: self.focus == Some(Control::Start),
                disabled: schedule_disabled,
                error: false,
                placeholder: "",
                min: today,
                today,
            };
            let start_event =
                start_field.show(ui, pos2(date_left, top + OUTLOOK_FIRST_DAY_TOP + 2.0));

            let mut end_field = date_input::Field {
                id: id.with("end"),
                input: &mut self.end,
                focused: self.focus == Some(Control::End),
                disabled: schedule_disabled,
                error: range_error,
                placeholder: "",
                min: start_date,
                today,
            };
            let end_event = end_field.show(ui, pos2(date_left, top + OUTLOOK_LAST_DAY_TOP + 2.0));

            claimed |= start_event.is_some() || end_event.is_some();
            self.date_event(Control::Start, start_event, now);
            self.date_event(Control::End, end_event, now);

            claimed |= self.show_time_input(
                ui,
                id.with("start-time"),
                true,
                pos2(time_left, top + OUTLOOK_FIRST_DAY_TOP + 2.0),
                schedule_disabled,
                false,
                now,
            );
            claimed |= self.show_time_input(
                ui,
                id.with("end-time"),
                false,
                pos2(time_left, top + OUTLOOK_LAST_DAY_TOP + 2.0),
                schedule_disabled,
                range_error,
                now,
            );
        }

        // Internal (or personal) automatic reply message.
        let internal_label = match &self.domain {
            Some(domain) => format!("Send automatic replies to people at {domain}:"),
            None => "Message".to_owned(),
        };
        LABEL.paint_line(
            &painter,
            pos2(left, top + OUTLOOK_INTERNAL_LABEL_TOP),
            &internal_label,
            subdued_if(disabled),
            None,
        );
        let internal_area = Rect::from_min_size(
            pos2(left, top + OUTLOOK_INTERNAL_EDITOR_TOP),
            vec2(width, OUTLOOK_EDITOR_HEIGHT),
        );
        if self.loading {
            skeleton_lines(ui, internal_area.min, width);
        } else {
            let (pasted, focused) =
                self.show_editor(ui, Control::Message, internal_area, carbon, disabled, now);
            claimed |= focused;
            event = pasted.or(event);
        }
        painter.rect_filled(
            Rect::from_min_size(
                pos2(left, top + OUTLOOK_INTERNAL_RULE_TOP),
                vec2(width, 1.0),
            ),
            0.0,
            RULE,
        );

        // External / contacts section.
        if personal {
            if self.loading {
                skeleton(
                    ui,
                    Rect::from_min_size(
                        pos2(left, top + OUTLOOK_EXTERNAL_TOP + 2.0),
                        vec2(240.0, SKELETON_HEIGHT),
                    ),
                );
            } else if self.show_checkbox_row(
                ui,
                id,
                Control::Contacts,
                left,
                top + OUTLOOK_EXTERNAL_TOP,
                0.0,
                "Only send replies to people in my contacts",
                self.form.contacts,
                disabled,
            ) {
                self.form.contacts = !self.form.contacts;
                self.set_focus(Some(Control::Contacts), now);
                claimed = true;
            }
        } else if let Some(domain) = self.domain.clone() {
            let external_label = format!("Send automatic replies outside of {domain}");
            if self.loading {
                skeleton(
                    ui,
                    Rect::from_min_size(
                        pos2(left, top + OUTLOOK_EXTERNAL_TOP + 2.0),
                        vec2(280.0, SKELETON_HEIGHT),
                    ),
                );
            } else {
                if self.show_slider_row(
                    ui,
                    id,
                    Control::External,
                    left,
                    top + OUTLOOK_EXTERNAL_TOP,
                    &external_label,
                    self.form.external,
                    disabled,
                ) {
                    self.toggle_external();
                    self.set_focus(Some(Control::External), now);
                    claimed = true;
                }
                if self.form.external {
                    if self.show_checkbox_row(
                        ui,
                        id,
                        Control::Contacts,
                        left,
                        top + OUTLOOK_CONTACTS_TOP,
                        0.0,
                        "Only send replies to people in my contacts",
                        self.form.contacts,
                        disabled,
                    ) {
                        self.form.contacts = !self.form.contacts;
                        self.set_focus(Some(Control::Contacts), now);
                        claimed = true;
                    }
                    let external_area = Rect::from_min_size(
                        pos2(left, top + OUTLOOK_EXTERNAL_EDITOR_TOP),
                        vec2(width, OUTLOOK_EDITOR_HEIGHT),
                    );
                    let (pasted, focused) = self.show_editor(
                        ui,
                        Control::ExternalMessage,
                        external_area,
                        carbon,
                        disabled,
                        now,
                    );
                    claimed |= focused;
                    event = pasted.or(event);
                    painter.rect_filled(
                        Rect::from_min_size(
                            pos2(left, top + OUTLOOK_EXTERNAL_RULE_TOP),
                            vec2(width, 1.0),
                        ),
                        0.0,
                        RULE,
                    );
                }
            }
        }

        // Cancel and Save.
        let actions_top = if self.show_outlook_external_section() {
            OUTLOOK_ACTIONS_EXPANDED_TOP
        } else {
            OUTLOOK_ACTIONS_COMPACT_TOP
        };
        let (footer_event, footer_claimed) = self.show_footer(ui, id, body, actions_top, now);
        claimed |= footer_claimed;
        event = footer_event.or(event);
        if clicked && !claimed && self.focus == focus_before {
            self.set_focus(None, now);
        }
        event
    }

    /// Draws an Outlook Start/End time input matching `date_input::Field`'s typography and
    /// underline. Returns whether the input took a click or focus this frame.
    #[expect(
        clippy::too_many_arguments,
        reason = "time input geometry, error state, and clock"
    )]
    fn show_time_input(
        &mut self,
        ui: &mut Ui,
        field_id: Id,
        start: bool,
        at: Pos2,
        disabled: bool,
        range_error: bool,
        now: &Zoned,
    ) -> bool {
        let control = if start {
            Control::StartTime
        } else {
            Control::EndTime
        };
        let focused = self.focus == Some(control);
        let style = date_input::TEXT.without_ligatures();
        let line = Rect::from_min_size(at, vec2(OUTLOOK_TIME_WIDTH, style.line_height));
        let invalid = !disabled
            && (range_error
                || if start {
                    self.start_time_error
                } else {
                    self.end_time_error
                });
        let painter = ui.painter();
        let (rule_gap, rule_thickness, rule_color) = if invalid {
            (TIME_RULE_GAP - 1.0, 2.0, TIME_RULE_ERROR)
        } else if focused && !disabled {
            (TIME_RULE_GAP - 1.0, 2.0, TIME_RULE_FOCUSED)
        } else {
            (TIME_RULE_GAP, 1.0, RULE)
        };
        painter.rect_filled(
            Rect::from_min_size(
                pos2(line.left(), line.bottom() + rule_gap),
                vec2(line.width(), rule_thickness),
            ),
            0.0,
            rule_color,
        );
        if disabled {
            let text = if start {
                self.start_time_text.as_str()
            } else {
                self.end_time_text.as_str()
            };
            style.paint_line(
                painter,
                line.min,
                text,
                TIME_INK_DISABLED,
                Some(line.width()),
            );
            ui.memory_mut(|memory| memory.surrender_focus(field_id));
            return false;
        }
        let ink = if invalid { TIME_RULE_ERROR } else { TIME_INK };
        let (before, response) = {
            let text = if start {
                &mut self.start_time_text
            } else {
                &mut self.end_time_text
            };
            let before = text.clone();
            let response = exo_ui::text_field(
                ui,
                field_id,
                text,
                line,
                &style,
                ink,
                ui.visuals().selection.bg_fill,
            );
            (before, response)
        };
        let mut claimed = false;
        if response.clicked() || (response.gained_focus() && !focused) {
            self.set_focus(Some(control), now);
            claimed = true;
        } else {
            hold_focus(ui, focused, field_id);
        }
        let current = if start {
            self.start_time_text.as_str()
        } else {
            self.end_time_text.as_str()
        };
        if current != before {
            let tz = now.time_zone().clone();
            let reference = if start {
                self.form.start.to_zoned(tz.clone())
            } else {
                self.form
                    .end
                    .unwrap_or(self.form.start)
                    .to_zoned(tz.clone())
            };
            match parse_time(current, &reference) {
                Some((hour, minute)) => {
                    let date = reference.date();
                    if let Ok(zoned) = date.at(hour, minute, 0, 0).to_zoned(tz) {
                        if start {
                            self.form.start = zoned.timestamp();
                            self.start_time_error = false;
                        } else {
                            self.form.end = Some(zoned.timestamp());
                            self.end_time_error = false;
                        }
                    }
                }
                None => {
                    if start {
                        self.start_time_error = true;
                    } else {
                        self.end_time_error = true;
                    }
                }
            }
        }
        claimed
    }

    /// Draws the internal or external message editor in `area`. Returns a file pasted in it, and
    /// whether it took the keyboard.
    fn show_editor(
        &mut self,
        ui: &mut Ui,
        control: Control,
        area: Rect,
        carbon: bool,
        disabled: bool,
        now: &Zoned,
    ) -> (Option<Event>, bool) {
        let external = control == Control::ExternalMessage;
        let (child_salt, scroll_salt, off_id, bar_id, spelling_target) = if external {
            (
                "out-of-office-external-message",
                "out-of-office-external-message-scroll",
                "out-of-office-external-message-off",
                "out-of-office-external-scrollbar",
                crate::spelling::EditorTarget::OutOfOfficeExternalMessage,
            )
        } else {
            (
                "out-of-office-message",
                "out-of-office-message-scroll",
                "out-of-office-message-off",
                "out-of-office-scrollbar",
                crate::spelling::EditorTarget::OutOfOfficeMessage,
            )
        };
        // `scrollbar-gutter: stable`: the text stops short of the scrollbar's track.
        let gutter = if carbon { 12.0 } else { 11.0 };
        let text = Rect::from_min_size(
            area.min - vec2(0.0, 1.0),
            vec2(area.width() - gutter, area.height()),
        );
        let pasted_file = !disabled
            && self.focus == Some(control)
            && ui.input(|input| {
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Paste(_)))
            })
            && (!crate::clipboard::read_files(ui.ctx()).is_empty()
                || crate::clipboard::read_image(ui.ctx()).is_some());
        if pasted_file {
            ui.input_mut(|input| {
                input
                    .events
                    .retain(|event| !matches!(event, egui::Event::Paste(_)))
            });
        }
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text).id_salt(child_salt));
        child.set_clip_rect(area.intersect(ui.clip_rect()));
        // The message at .4 while off.
        if disabled {
            child.set_opacity(0.4);
        }
        let output = egui::ScrollArea::vertical()
            .id_salt(scroll_salt)
            .max_height(area.height())
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(&mut child, |ui| {
                let style = EditorStyle {
                    placeholder: Some("Enter message".into()),
                    min_height: area.height(),
                };
                let (editor, active_date) = if external {
                    (&mut self.external_editor, &mut self.external_date)
                } else {
                    (&mut self.editor, &mut self.date)
                };
                let edited = editor.ui(ui, &style);
                crate::spelling::note_editor(ui.ctx(), spelling_target, &edited);
                if edited.date != *active_date {
                    self.day = edited
                        .date
                        .as_ref()
                        .map(|date| date.text.clone())
                        .or(self.day.take());
                    *active_date = edited.date;
                }
                (
                    edited.response.id,
                    edited.response.clicked() || edited.response.gained_focus(),
                    edited.changed,
                )
            });
        let (editor_id, focused, changed) = output.inner;
        let scroll = output.state.offset.y;
        if external {
            self.external_scroll = scroll;
            if changed {
                self.form.external_body = self.external_editor.html();
            }
        } else {
            self.scroll = scroll;
            if changed {
                self.form.body = self.editor.html();
            }
        }
        if disabled {
            // Clicks on a disabled message are swallowed.
            ui.interact(area, exo_ui::id(ui.ctx(), off_id), Sense::click_and_drag());
            ui.memory_mut(|memory| memory.surrender_focus(editor_id));
        } else {
            if focused {
                self.set_focus(Some(control), now);
            }
            hold_focus(ui, self.focus == Some(control), editor_id);
        }
        let content = output.content_size.y;
        let scrollbar = if external {
            &mut self.external_scrollbar
        } else {
            &mut self.scrollbar
        };
        scrollbar.show(ui, exo_ui::id(ui.ctx(), bar_id), area, scroll, content);
        (pasted_file.then_some(Event::PastedFile), focused)
    }

    /// Applies what a date input reported.
    fn date_event(&mut self, control: Control, event: Option<DateEvent>, now: &Zoned) {
        let Some(event) = event else { return };
        let today = now.date();
        match event {
            DateEvent::Focused => {
                self.set_focus(Some(control), now);
                let input = if control == Control::Start {
                    &mut self.start
                } else {
                    &mut self.end
                };
                input
                    .month
                    .get_or_insert(input.parsed.unwrap_or(today).first_of_month());
            }
            DateEvent::MonthChanged => self.set_focus(Some(control), now),
            DateEvent::Picked(date) => match control {
                Control::Start => self.pick_start(date, now),
                _ => {
                    self.pick_end(Some(date), now);
                    self.end.month = None;
                }
            },
            DateEvent::Cleared => {
                let tz = now.time_zone().clone();
                let start = self.form.start.to_zoned(tz.clone()).date();
                if control == Control::Start {
                    self.start.typed(start, today, Some(today), false);
                } else {
                    let reference = self.form.end.map_or(today, |end| end.to_zoned(tz).date());
                    // Gmail's last day is optional; Outlook's scheduled end date is required.
                    let nullable = !self.outlook;
                    if self.end.typed(reference, today, Some(start), nullable) && nullable {
                        self.form.end = None;
                    }
                }
                let input = if control == Control::Start {
                    &mut self.start
                } else {
                    &mut self.end
                };
                input
                    .month
                    .get_or_insert(input.parsed.unwrap_or(today).first_of_month());
            }
        }
    }
}

/// Default Outlook scheduled window: today at the current whole hour through tomorrow at the same
/// wall hour (`start <= now < end` at any time of day).
fn default_outlook_window(now: &Zoned) -> (Timestamp, Timestamp) {
    let start = now
        .date()
        .at(now.hour(), 0, 0, 0)
        .to_zoned(now.time_zone().clone())
        .map_or_else(|_| now.timestamp(), |zoned| zoned.timestamp());
    let end = next_day_same_time(start, now);
    (start, end)
}

/// Replaces `current`'s civil date with `date` in `now`'s zone, preserving its local wall hour and
/// minute.
fn with_date(current: Timestamp, date: Date, now: &Zoned) -> Timestamp {
    let tz = now.time_zone().clone();
    let local = current.to_zoned(tz.clone());
    date.at(local.hour(), local.minute(), 0, 0)
        .to_zoned(tz)
        .map_or_else(|_| midnight(date, now), |zoned| zoned.timestamp())
}

/// One civil day after `instant` at the same local wall hour and minute.
fn next_day_same_time(instant: Timestamp, now: &Zoned) -> Timestamp {
    let tz = now.time_zone().clone();
    let local = instant.to_zoned(tz.clone());
    local
        .date()
        .checked_add(1.day())
        .ok()
        .and_then(|next| {
            next.at(local.hour(), local.minute(), 0, 0)
                .to_zoned(tz)
                .ok()
        })
        .map_or_else(
            || instant + SignedDuration::from_hours(24),
            |zoned| zoned.timestamp(),
        )
}

/// Clock formats accepted by Outlook's time inputs, tried in order.
const TIME_FORMATS: [&str; 5] = ["hmm", "hmma", "ha", "HHmm", "HH"];

/// Strictly parses a typed clock time (`1:00 PM`, `9:30 AM`, `2p`, `930`, `14:30`, `9`, etc.),
/// using `reference`'s half of the day when a 12-hour input omits `am`/`pm`. Rejects non-time text.
fn parse_time(text: &str, reference: &Zoned) -> Option<(i8, i8)> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some((hour_part, rest)) = trimmed.split_once(':') {
        return parse_colon_time(hour_part.trim(), rest.trim(), reference);
    }
    let compact: String = trimmed.chars().filter(|ch| *ch != ' ').collect();
    if compact.is_empty() {
        return None;
    }
    let read = TIME_FORMATS
        .iter()
        .find_map(|format| read_clock(&compact, format))?;
    let hour = resolve_hour(read.hour, read.twelve, read.pm, reference);
    Some((i8::try_from(hour).ok()?, i8::try_from(read.minute).ok()?))
}

/// Parses `H:MM` / `HH:MM` with an optional trailing `am`/`pm` indicator.
fn parse_colon_time(hour_part: &str, rest: &str, reference: &Zoned) -> Option<(i8, i8)> {
    if !(1..=2).contains(&hour_part.len()) || !hour_part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (minute, after_min) = take_exact_digits(rest, 2)?;
    (minute <= 59).then_some(())?;
    let tail = after_min.trim_start();
    let pm = if tail.is_empty() {
        None
    } else {
        let (pm, left) = take_meridiem(tail)?;
        left.is_empty().then_some(())?;
        Some(pm)
    };
    let raw_hour: u8 = hour_part.parse().ok()?;
    let (twelve, hour) = if pm.is_some() {
        (1..=12).contains(&raw_hour).then_some(())?;
        (true, raw_hour)
    } else if raw_hour == 0 || raw_hour >= 13 || hour_part.starts_with('0') {
        (raw_hour <= 23).then_some(())?;
        (false, raw_hour)
    } else {
        (true, raw_hour)
    };
    let hour = resolve_hour(hour, twelve, pm, reference);
    Some((i8::try_from(hour).ok()?, i8::try_from(minute).ok()?))
}

fn resolve_hour(hour: u8, twelve: bool, pm: Option<bool>, reference: &Zoned) -> u8 {
    if twelve {
        match (pm.unwrap_or(reference.hour() >= 12), hour) {
            (true, 12) => 12,
            (true, h) => h + 12,
            (false, 12) => 0,
            (false, h) => h,
        }
    } else {
        hour
    }
}

#[derive(Clone, Copy)]
struct ClockRead {
    hour: u8,
    minute: u8,
    twelve: bool,
    pm: Option<bool>,
}

fn read_clock(text: &str, format: &str) -> Option<ClockRead> {
    let mut rest = text;
    let mut read = ClockRead {
        hour: 0,
        minute: 0,
        twelve: false,
        pm: None,
    };
    let mut tokens = format;
    while !tokens.is_empty() {
        if let Some(after) = tokens.strip_prefix("HH") {
            let (value, left) = take_digits(rest, 2)?;
            (value <= 23).then_some(())?;
            read.hour = value;
            (rest, tokens) = (left, after);
        } else if let Some(after) = tokens.strip_prefix("mm") {
            let (value, left) = take_exact_digits(rest, 2)?;
            (value <= 59).then_some(())?;
            read.minute = value;
            (rest, tokens) = (left, after);
        } else if let Some(after) = tokens.strip_prefix('h') {
            let need_two_for_mm = after.starts_with("mm");
            let len = match rest.as_bytes() {
                [b'1', b'0'..=b'2', ..] | [b'0', b'0'..=b'9', ..]
                    if !need_two_for_mm
                        || rest
                            .as_bytes()
                            .get(2..4)
                            .is_some_and(|digits| digits.iter().all(u8::is_ascii_digit)) =>
                {
                    2
                }
                [b'0'..=b'9', ..] => 1,
                _ => return None,
            };
            let value: u8 = rest[..len].parse().ok()?;
            (1..=12).contains(&value).then_some(())?;
            (read.hour, read.twelve) = (value, true);
            (rest, tokens) = (&rest[len..], after);
        } else {
            let after = tokens.strip_prefix('a')?;
            let (pm, left) = take_meridiem(rest)?;
            read.pm = Some(pm);
            (rest, tokens) = (left, after);
        }
    }
    rest.is_empty().then_some(read)
}

fn take_meridiem(text: &str) -> Option<(bool, &str)> {
    let lower = text.to_ascii_lowercase();
    let pm = match lower.as_bytes().first() {
        Some(b'a') => false,
        Some(b'p') => true,
        _ => return None,
    };
    let long = [".m.", ".m", "m.", "m"]
        .iter()
        .find(|suffix| lower[1..].starts_with(**suffix))
        .map_or(0, |suffix| suffix.len());
    Some((pm, &text[1 + long..]))
}

fn take_exact_digits(text: &str, count: usize) -> Option<(u8, &str)> {
    let bytes = text.as_bytes().get(..count)?;
    bytes.iter().all(u8::is_ascii_digit).then_some(())?;
    Some((text[..count].parse().ok()?, &text[count..]))
}

fn take_digits(text: &str, max: usize) -> Option<(u8, &str)> {
    let len = text
        .bytes()
        .take(max)
        .take_while(u8::is_ascii_digit)
        .count();
    (len > 0).then_some(())?;
    Some((text[..len].parse().ok()?, &text[len..]))
}

/// Three loading skeleton lines (the last 40% wide, 4px apart).
fn skeleton_lines(ui: &Ui, origin: Pos2, width: f32) {
    for (index, share) in [1.0, 1.0, 0.4].into_iter().enumerate() {
        skeleton(
            ui,
            Rect::from_min_size(
                origin + vec2(0.0, index as f32 * (SKELETON_HEIGHT + 4.0)),
                vec2(width * share, SKELETON_HEIGHT),
            ),
        );
    }
}

/// Gives egui's keyboard focus to `id` while it's `wanted`, and takes it away when not.
pub(super) fn hold_focus(ui: &Ui, wanted: bool, id: Id) {
    let has = ui.memory(|memory| memory.has_focus(id));
    if wanted && !has {
        ui.memory_mut(|memory| memory.request_focus(id));
    } else if !wanted && has {
        ui.memory_mut(|memory| memory.surrender_focus(id));
    }
}

/// `date`'s local midnight in `now`'s zone.
fn midnight(date: Date, now: &Zoned) -> Timestamp {
    date.to_zoned(now.time_zone().clone())
        .map_or_else(|_| now.timestamp(), |zoned| zoned.timestamp())
}

/// `date`'s last local second (`23:59:59`) in `now`'s zone.
fn end_of_day(date: Date, now: &Zoned) -> Option<Timestamp> {
    date.at(23, 59, 59, 0)
        .to_zoned(now.time_zone().clone())
        .ok()
        .map(|end| end.timestamp())
}

/// A loading skeleton in `rect`, with its shine where the 2s animation has it.
fn skeleton(ui: &Ui, rect: Rect) {
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(4), SKELETON);
    // `background-position` from `left -100px` to `right -100px` over 2s, `ease`, repeating.
    let time = ui.input(|input| input.time);
    let progress = exo_ui::motion::CubicBezier::EASE.at((time % 2.0) as f32 / 2.0);
    let band = 100.0;
    let x = rect.left() - band + (rect.width() + band) * progress;
    let stops = [(0.0, 0.0), (0.2, 0.2), (0.6, 0.5), (1.0, 0.0)];
    let mut mesh = Mesh::default();
    for (index, (at, alpha)) in stops.iter().enumerate() {
        let column = x + band * at;
        let color = Color32::from_white_alpha((alpha * 255.0) as u8);
        mesh.colored_vertex(
            Pos2::new(column.clamp(rect.left(), rect.right()), rect.top()),
            color,
        );
        mesh.colored_vertex(
            Pos2::new(column.clamp(rect.left(), rect.right()), rect.bottom()),
            color,
        );
        if index > 0 {
            let base = (index as u32 - 1) * 2;
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base + 1, base + 2, base + 3);
        }
    }
    painter.with_clip_rect(rect).add(egui::Shape::mesh(mesh));
    ui.ctx().request_repaint();
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn ny(text: &str) -> Zoned {
        text.parse().expect("valid zoned")
    }

    fn loaded_active_dialog(now: &Zoned) -> Dialog {
        let mut dialog = Dialog::new("maya@brightmoor.co", None, now);
        let vacation = VacationSettings {
            enable_auto_reply: Some(true),
            response_subject: Some("Out of office".into()),
            response_body_html: Some("<div>Back soon</div>".into()),
            start_time: Some(midnight(now.date(), now).as_millisecond()),
            end_time: end_of_day(now.date(), now)
                .map(|end| (end + SignedDuration::from_secs(1)).as_millisecond()),
            ..VacationSettings::default()
        };
        let copy = OutOfOfficeResponder::of(&vacation, now.timestamp());
        dialog.loaded(Some(&copy), now);
        dialog
    }

    #[test]
    fn turning_off_active_ooo_allows_save_despite_empty_fields_or_date_errors() {
        let now = ny("2026-11-17T09:00[America/New_York]");
        let mut dialog = loaded_active_dialog(&now);
        assert!(!dialog.save_disabled());

        // While enabled, empty subject or date error disables Save.
        dialog.form.subject.clear();
        dialog.start.error = true;
        assert!(dialog.save_disabled());

        // Turning off an active OOO lets the user save despite disabled-field errors.
        dialog.toggle();
        assert!(!dialog.form.enabled && dialog.initial.enabled);
        assert!(!dialog.save_disabled());

        // Loading / submitting still disable Save.
        dialog.submitting = true;
        assert!(dialog.save_disabled());
        dialog.submitting = false;
        dialog.loading = true;
        assert!(dialog.save_disabled());

        // Turning off when already initially off keeps Save disabled.
        let mut off = Dialog::new("maya@brightmoor.co", None, &now);
        off.loaded(None, &now);
        assert!(!off.form.enabled && !off.initial.enabled);
        assert!(off.save_disabled());
    }

    #[test]
    fn pick_start_shifts_end_by_civil_days_across_dst_boundaries() {
        // Spring forward in America/New_York is 2026-03-08 (a 23-hour day).
        let spring = ny("2026-03-06T09:00[America/New_York]");
        let mut dialog = loaded_active_dialog(&spring);
        // Set range: Mar 6 .. Mar 7 (1-day span, 2 calendar days).
        dialog.pick_end(Some(date(2026, 3, 7)), &spring);
        // Pick start = Mar 8 -> end should shift to Mar 9 at 23:59:59 EDT.
        dialog.pick_start(date(2026, 3, 8), &spring);
        let shifted_end = dialog
            .form
            .end
            .expect("end stays set")
            .to_zoned(spring.time_zone().clone());
        assert_eq!(shifted_end.date(), date(2026, 3, 9));
        assert_eq!(
            (
                shifted_end.hour(),
                shifted_end.minute(),
                shifted_end.second()
            ),
            (23, 59, 59),
            "spring-forward DST must keep end at 23:59:59: {shifted_end}"
        );

        // Fall back in America/New_York is 2026-11-01 (a 25-hour day).
        let fall = ny("2026-10-31T09:00[America/New_York]");
        let mut dialog = loaded_active_dialog(&fall);
        dialog.pick_end(Some(date(2026, 10, 31)), &fall);
        dialog.pick_start(date(2026, 11, 1), &fall);
        let shifted_end = dialog
            .form
            .end
            .expect("end stays set")
            .to_zoned(fall.time_zone().clone());
        assert_eq!(shifted_end.date(), date(2026, 11, 1));
        assert_eq!(
            (
                shifted_end.hour(),
                shifted_end.minute(),
                shifted_end.second()
            ),
            (23, 59, 59),
            "fall-back DST must keep end at 23:59:59: {shifted_end}"
        );
    }

    #[test]
    fn valid_typed_dates_commit_on_focus_blur_and_before_save() {
        let now = ny("2026-11-17T09:00[America/New_York]");
        let mut dialog = loaded_active_dialog(&now);

        // Type a new start date and blur to Subject -> start date commits without stealing focus.
        dialog.set_focus(Some(Control::Start), &now);
        dialog.start.text = "Nov 20, 2026".into();
        dialog.start.parsed = Some(date(2026, 11, 20));
        dialog.start.error = false;
        dialog.set_focus(Some(Control::Subject), &now);
        assert_eq!(dialog.focus(), Some(Control::Subject));
        assert_eq!(
            dialog.form.start.to_zoned(now.time_zone().clone()).date(),
            date(2026, 11, 20)
        );

        // Type a new end date and save directly without blurring -> end date commits before Save.
        dialog.set_focus(Some(Control::End), &now);
        dialog.end.text = "Nov 25, 2026".into();
        dialog.end.parsed = Some(date(2026, 11, 25));
        dialog.end.error = false;
        dialog.commit_dates(&now);
        let end = dialog
            .form
            .end
            .expect("end committed")
            .to_zoned(now.time_zone().clone());
        assert_eq!(end.date(), date(2026, 11, 25));
        assert_eq!((end.hour(), end.minute(), end.second()), (23, 59, 59));
    }

    #[test]
    fn outlook_form_round_trips_and_validates_without_subject() {
        let now = ny("2026-11-17T09:00[America/New_York]");
        let mut dialog = Dialog::new_for("maya@brightmoor.co", true, None, &now);
        assert_eq!(dialog.domain.as_deref(), Some("brightmoor.co"));
        assert!(!dialog.is_outlook_personal());

        dialog.loaded(None, &now);
        assert!(!dialog.form.enabled && dialog.save_disabled());

        // Enabling Outlook OOO requires only the internal message (no Gmail subject).
        dialog.toggle();
        assert!(
            dialog.save_disabled(),
            "empty internal message disables Save"
        );
        dialog.editor_mut().set_html("<div>Out of office</div>");
        dialog.form.body = dialog.editor().html();
        assert!(
            !dialog.save_disabled(),
            "AlwaysEnabled without subject is saveable"
        );
        assert_eq!(
            dialog.auto_reply(),
            AutoReply::Outlook(OutlookAutoReply {
                status: AutoReplyStatus::AlwaysEnabled,
                external_audience: ExternalAudience::None,
                internal_reply_html: dialog.form.body.clone(),
                external_reply_html: String::new(),
                start_time: None,
                end_time: None,
            })
        );

        // Enabling external replies requires an external message; ContactsOnly maps cleanly.
        dialog.toggle_external();
        assert!(
            dialog.save_disabled(),
            "empty external message disables Save"
        );
        dialog
            .external_editor_mut()
            .expect("outlook external editor")
            .set_html("<div>External OOO</div>");
        dialog.form.external_body = dialog
            .external_editor()
            .expect("outlook external editor")
            .html();
        dialog.form.contacts = true;
        assert!(!dialog.save_disabled());

        // Scheduling checks start < end and populates exact timestamps.
        dialog.toggle_scheduled(&now);
        assert!(!dialog.save_disabled());
        let saved = dialog.auto_reply();
        let AutoReply::Outlook(reply) = &saved else {
            panic!("expected outlook auto reply");
        };
        assert_eq!(reply.status, AutoReplyStatus::Scheduled);
        assert_eq!(reply.external_audience, ExternalAudience::ContactsOnly);
        assert!(reply.start_time.is_some() && reply.end_time > reply.start_time);

        // Reloading from OutOfOfficeResponder::from_auto_reply preserves every Outlook field.
        let copy = OutOfOfficeResponder::from_auto_reply(&saved, now.timestamp());
        let mut reloaded = Dialog::new_for("maya@brightmoor.co", true, None, &now);
        reloaded.loaded(Some(&copy), &now);
        assert_eq!(reloaded.auto_reply(), saved);

        // End at or before start disables Save while scheduled without mutating end.error, so
        // restoring end > start re-enables Save cleanly; turning OOO off stays allowed either way.
        reloaded.form.end = Some(reloaded.form.start);
        assert!(reloaded.save_disabled());
        assert!(!reloaded.end.error);
        reloaded.form.end = Some(next_day_same_time(reloaded.form.start, &now));
        assert!(!reloaded.save_disabled());
        reloaded.form.end = Some(reloaded.form.start);
        reloaded.toggle();
        assert!(!reloaded.save_disabled());
    }

    #[test]
    fn outlook_personal_omits_external_editor_and_keeps_contacts_enabled() {
        let now = ny("2026-11-17T09:00[America/New_York]");
        let mut dialog = Dialog::new_for("alex@outlook.com", true, None, &now);
        assert!(dialog.is_outlook_personal());
        dialog.loaded(None, &now);
        dialog.toggle();
        dialog.editor_mut().set_html("<div>Away personally</div>");
        dialog.form.body = dialog.editor().html();
        assert!(
            !dialog.save_disabled(),
            "outlook.com must not require external_editor"
        );
        assert!(
            dialog.focusable(Control::Contacts),
            "outlook.com contacts checkbox is enabled whenever OOO is on"
        );
        assert!(!dialog.focusable(Control::External));
        assert!(!dialog.focusable(Control::ExternalMessage));
        assert!(!dialog.show_outlook_external_section());

        // Default unchecked contacts sends ExternalAudience::All with body mirrored to external.
        let AutoReply::Outlook(all_reply) = dialog.auto_reply() else {
            panic!("expected outlook auto reply");
        };
        assert_eq!(all_reply.external_audience, ExternalAudience::All);
        assert_eq!(all_reply.external_reply_html, dialog.form.body);

        dialog.form.contacts = true;
        let AutoReply::Outlook(contacts_reply) = dialog.auto_reply() else {
            panic!("expected outlook auto reply");
        };
        assert_eq!(
            contacts_reply.external_audience,
            ExternalAudience::ContactsOnly
        );
    }

    #[test]
    fn outlook_tab_order_skips_gmail_only_and_disabled_controls() {
        let now = ny("2026-11-17T09:00[America/New_York]");
        let mut dialog = Dialog::new_for("maya@brightmoor.co", true, None, &now);
        dialog.loaded(None, &now);
        dialog.toggle();
        dialog.editor_mut().set_html("<div>Away</div>");
        dialog.form.body = dialog.editor().html();

        // With scheduled and external off, Tab walks Switch -> Schedule -> Message -> External ->
        // Save -> Cancel, skipping Subject, Domain, Start/End dates/times, Contacts, ExternalMessage.
        let mut visited = Vec::new();
        for _ in 0..6 {
            dialog.tab(1, &now);
            visited.push(dialog.focus().expect("focused"));
        }
        assert_eq!(
            visited,
            [
                Control::Switch,
                Control::Schedule,
                Control::Message,
                Control::External,
                Control::Save,
                Control::Cancel,
            ]
        );
    }

    #[test]
    fn shift_tab_moves_backward_past_start_date_without_trapping() {
        let now = ny("2026-11-17T09:00[America/New_York]");
        let mut outlook = Dialog::new_for("maya@brightmoor.co", true, None, &now);
        outlook.loaded(None, &now);
        outlook.toggle();
        outlook.toggle_scheduled(&now);
        outlook.set_focus(Some(Control::StartTime), &now);
        outlook.tab(-1, &now);
        assert_eq!(outlook.focus(), Some(Control::Start));
        outlook.tab(-1, &now);
        assert_eq!(outlook.focus(), Some(Control::Schedule));
        outlook.tab(-1, &now);
        assert_eq!(outlook.focus(), Some(Control::Switch));

        let mut gmail = loaded_active_dialog(&now);
        gmail.set_focus(Some(Control::End), &now);
        gmail.tab(-1, &now);
        assert_eq!(gmail.focus(), Some(Control::Start));
        gmail.tab(-1, &now);
        assert_eq!(gmail.focus(), Some(Control::Switch));
    }

    #[test]
    fn outlook_time_parser_accepts_clock_formats_and_rejects_phrases() {
        let am = ny("2026-11-17T09:00[America/New_York]");
        let pm = ny("2026-11-17T15:00[America/New_York]");
        assert_eq!(parse_time("1:00 PM", &am), Some((13, 0)));
        assert_eq!(parse_time("1:15 PM", &am), Some((13, 15)));
        assert_eq!(parse_time("1:05 AM", &pm), Some((1, 5)));
        assert_eq!(parse_time("1:20 AM", &pm), Some((1, 20)));
        assert_eq!(parse_time("10:00 AM", &pm), Some((10, 0)));
        assert_eq!(parse_time("11:30 PM", &am), Some((23, 30)));
        assert_eq!(parse_time("100", &am), Some((1, 0)));
        assert_eq!(parse_time("1000", &am), Some((10, 0)));
        assert_eq!(parse_time("9:30 AM", &am), Some((9, 30)));
        assert_eq!(parse_time("2p", &am), Some((14, 0)));
        assert_eq!(parse_time("14:30", &am), Some((14, 30)));
        assert_eq!(parse_time("715", &pm), Some((19, 15)));
        assert_eq!(parse_time("12:00 PM", &am), Some((12, 0)));
        assert_eq!(parse_time("12:00 AM", &pm), Some((0, 0)));

        // Every minute of the day formatted by `dates::time` must round-trip losslessly.
        for hour in 0..24 {
            for minute in 0..60 {
                let zoned = date(2026, 11, 17)
                    .at(hour, minute, 0, 0)
                    .to_zoned(am.time_zone().clone())
                    .expect("valid time");
                let formatted = crate::format::dates::time(&zoned);
                assert_eq!(
                    parse_time(&formatted, &zoned),
                    Some((hour, minute)),
                    "round-trip failed for {formatted:?}"
                );
            }
        }

        for invalid in [
            "",
            "soon",
            "tomorrow",
            "next friday",
            "25:00",
            "9:75 AM",
            "1:5 AM",
            "13:00 PM",
        ] {
            assert_eq!(parse_time(invalid, &am), None, "must reject {invalid:?}");
        }
    }
}
