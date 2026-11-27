//! Meetings as Outlook runs them: an invitee's responses (accept, tentatively accept, decline,
//! with a proposed new time), the organizer's cancel, and the iTIP mail (RFC 5546, as iMIP) that
//! Outlook sends for those, for invitations and for updates.
//!
//! The mail goes out through the mailbox's own sending, so it's in Sent Items (Gmail's SENT) as
//! the `eventMessage` it is. Its recipients are outside the mock, which keeps one mailbox.

use std::collections::HashMap;

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use gmail::calendar::{Event, EventAttendee, EventDateTime};
use gmail::mime::{Email, Mailbox};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::{Map, Value};

use super::error::{GraphError, Result};
use super::ics::{Attendee, Outgoing, When};
use super::{Account, Call, odata, origin};
use crate::world::World;

/// What a meeting message tells its recipients.
#[derive(Debug, Clone, Copy)]
pub enum Notice {
    /// An invitation, or an update to one.
    Request,
    /// The organizer's cancellation.
    Cancel,
    /// An invitee's answer, with the time they propose instead.
    Reply(Answer, Option<(Timestamp, Timestamp)>),
}

/// An invitee's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Accept,
    Tentative,
    Decline,
}

impl Answer {
    fn google(self) -> &'static str {
        match self {
            Self::Accept => "accepted",
            Self::Tentative => "tentative",
            Self::Decline => "declined",
        }
    }

    fn partstat(self) -> &'static str {
        match self {
            Self::Accept => "ACCEPTED",
            Self::Tentative => "TENTATIVE",
            Self::Decline => "DECLINED",
        }
    }

    /// The subject's prefix, as Outlook writes it.
    fn prefix(self) -> &'static str {
        match self {
            Self::Accept => "Accepted",
            Self::Tentative => "Tentative",
            Self::Decline => "Declined",
        }
    }

    fn action(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Tentative => "tentativelyAccept",
            Self::Decline => "decline",
        }
    }
}

/// Whether `me` organizes `event` (an event without an organizer is the calendar owner's).
pub fn organizes(me: &str, event: &Event) -> bool {
    event.organizer.as_ref().is_none_or(|organizer| {
        organizer.is_self == Some(true)
            || organizer
                .email
                .as_deref()
                .is_some_and(|email| email.eq_ignore_ascii_case(me))
    })
}

/// The attendees a meeting's invitations and cancellations go to: everyone but its organizer
/// and the user.
fn invitees<'a>(me: &str, event: &'a Event) -> impl Iterator<Item = &'a EventAttendee> {
    let organizer = event
        .organizer
        .as_ref()
        .and_then(|o| o.email.clone())
        .unwrap_or_default();
    let me = me.to_owned();
    event.attendees.iter().filter(move |attendee| {
        let email = attendee.email.as_deref().unwrap_or_default();
        attendee.organizer != Some(true)
            && !email.eq_ignore_ascii_case(&organizer)
            && !email.eq_ignore_ascii_case(&me)
    })
}

/// Whether `event` is a meeting `me` organizes: it has someone to invite.
pub fn is_meeting(me: &str, event: &Event) -> bool {
    organizes(me, event) && invitees(me, event).next().is_some()
}

/// A calendar time as iTIP's: an instant, or a day.
fn when(time: Option<&EventDateTime>, tz: &TimeZone) -> Option<When> {
    let time = time?;
    match &time.date {
        Some(date) => date.parse().ok().map(When::Day),
        None => time.timestamp(tz).map(When::At),
    }
}

/// Sends `notice` about `event` from `who`, as Outlook does: an invitation or cancellation to
/// its invitees, an answer to its organizer. `stored` holds what Outlook keeps beside the
/// event (whether it takes responses and proposals).
pub fn notify(
    world: &mut World,
    who: &Account,
    event: &Event,
    stored: &Map<String, Value>,
    notice: Notice,
    comment: Option<&str>,
) -> Result<()> {
    let tz = world.calendar.tz.clone();
    let me = world.calendar.primary.clone();
    let (Some(start), Some(end)) = (
        when(event.start.as_ref(), &tz),
        when(event.end.as_ref(), &tz),
    ) else {
        return Ok(());
    };
    let organizer_email = event
        .organizer
        .as_ref()
        .and_then(|o| o.email.clone())
        .unwrap_or_else(|| me.clone());
    let organizer_name = event
        .organizer
        .as_ref()
        .and_then(|o| o.display_name.clone())
        .unwrap_or_else(|| {
            if organizer_email.eq_ignore_ascii_case(&me) {
                who.name.clone()
            } else {
                organizer_email.clone()
            }
        });
    let rsvp = stored.get("responseRequested") != Some(&Value::Bool(false));
    let summary = event.summary.clone().unwrap_or_default();
    let description =
        mail_parser::decoders::html::html_to_text(event.description.as_deref().unwrap_or_default());
    let (method, subject, attendees, recipients, start, end) = match notice {
        Notice::Request | Notice::Cancel => {
            let attendees: Vec<Attendee> = invitees(&me, event)
                .map(|attendee| Attendee {
                    name: attendee
                        .display_name
                        .clone()
                        .unwrap_or_else(|| attendee.email.clone().unwrap_or_default()),
                    address: attendee.email.clone().unwrap_or_default(),
                    partstat: match attendee.response_status.as_deref() {
                        Some("accepted") => "ACCEPTED",
                        Some("tentative") => "TENTATIVE",
                        Some("declined") => "DECLINED",
                        _ => "NEEDS-ACTION",
                    },
                    role: if attendee.resource == Some(true) {
                        "NON-PARTICIPANT"
                    } else if attendee.optional == Some(true) {
                        "OPT-PARTICIPANT"
                    } else {
                        "REQ-PARTICIPANT"
                    },
                    rsvp,
                })
                .collect();
            let recipients: Vec<Mailbox> = attendees
                .iter()
                .map(|a| Mailbox::new(a.name.clone(), a.address.clone()))
                .collect();
            let (method, subject) = match notice {
                Notice::Cancel => ("CANCEL", format!("Canceled: {summary}")),
                _ => ("REQUEST", summary.clone()),
            };
            (method, subject, attendees, recipients, start, end)
        }
        Notice::Reply(answer, proposed) => {
            let attendees = vec![Attendee {
                name: who.name.clone(),
                address: me.clone(),
                partstat: answer.partstat(),
                role: "REQ-PARTICIPANT",
                rsvp: false,
            }];
            let recipients = vec![Mailbox::new(
                organizer_name.clone(),
                organizer_email.clone(),
            )];
            match proposed {
                Some((from, to)) => (
                    "COUNTER",
                    format!("New Time Proposed: {summary}"),
                    attendees,
                    recipients,
                    When::At(from),
                    When::At(to),
                ),
                None => (
                    "REPLY",
                    format!("{}: {summary}", answer.prefix()),
                    attendees,
                    recipients,
                    start,
                    end,
                ),
            }
        }
    };
    if recipients.is_empty() {
        return Ok(());
    }
    let now = world.clock.now();
    let ics = Outgoing {
        method,
        uid: event.ical_uid.as_deref().unwrap_or_default(),
        sequence: event.sequence.unwrap_or_default(),
        summary: &summary,
        location: event.location.as_deref().unwrap_or_default(),
        description: if matches!(notice, Notice::Request) {
            &description
        } else {
            ""
        },
        start,
        end,
        recurrence: &event.recurrence,
        recurrence_id: when(event.original_start_time.as_ref(), &tz),
        organizer: (&organizer_name, &organizer_email),
        attendees,
        no_counter: stored.get("allowNewTimeProposals") == Some(&Value::Bool(false)),
        stamp: now,
    }
    .write();
    let text = match (notice, comment) {
        (_, Some(comment)) if !comment.is_empty() => comment.to_owned(),
        (Notice::Request, _) => description.clone(),
        _ => String::new(),
    };
    let email = Email {
        from: Some(Mailbox::new(who.name.clone(), me.clone())),
        to: recipients,
        subject,
        text: Some(text),
        calendar: Some(ics),
        ..Email::default()
    };
    world
        .mail
        .send(email.to_rfc822(), None, now.as_millisecond())?;
    let world = &mut *world;
    world.graph.catch_up(&mut world.mail, now);
    Ok(())
}

/// An action's body, its parameters by name in any case, as Graph reads them. A parameter the
/// action doesn't take is refused.
fn parameters(body: &[u8], action: &str, known: &[&str]) -> Result<HashMap<String, Value>> {
    let request: Map<String, Value> = super::folders::body_json(body, "event")?;
    let mut out = HashMap::new();
    for (name, value) in request {
        let known = known
            .iter()
            .find(|known| known.eq_ignore_ascii_case(&name))
            .ok_or_else(|| {
                GraphError::new(
                    400,
                    "RequestBodyRead",
                    format!(
                        "The parameter '{name}' in the request payload is not a valid parameter for the operation '{action}'."
                    ),
                )
            })?;
        out.insert((*known).to_owned(), value);
    }
    Ok(out)
}

/// A Boolean parameter, which Graph also takes as `"true"` or `"false"`.
fn flag(value: &Value) -> Result<bool> {
    match value {
        Value::Bool(flag) => Ok(*flag),
        Value::String(text) if text.eq_ignore_ascii_case("true") => Ok(true),
        Value::String(text) if text.eq_ignore_ascii_case("false") => Ok(false),
        _ => Err(GraphError::unreadable_body("expected a Boolean")),
    }
}

/// A `timeSlot`, its properties in any case.
fn time_slot(value: &Value) -> Result<(Timestamp, Timestamp)> {
    let lower = lowercase(value);
    let at = |name: &str| {
        odata::parse_date_time_zone(&lower[name])
            .ok_or_else(|| GraphError::unreadable_body(format!("proposedNewTime needs its {name}")))
    };
    Ok((at("start")?, at("end")?))
}

/// `value` with its objects' keys lowercased, as Graph matches them.
fn lowercase(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let key = match key.to_ascii_lowercase().as_str() {
                        "datetime" => "dateTime".to_owned(),
                        "timezone" => "timeZone".to_owned(),
                        other => other.to_owned(),
                    };
                    (key, lowercase(value))
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// `POST /me/events/{id}/accept`.
pub async fn accept(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    respond(&call, &params, &body, Answer::Accept)
}

/// `POST /me/events/{id}/tentativelyAccept`.
pub async fn tentatively_accept(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    respond(&call, &params, &body, Answer::Tentative)
}

/// `POST /me/events/{id}/decline`.
pub async fn decline(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    respond(&call, &params, &body, Answer::Decline)
}

/// An invitee's answer (202): their response on the event, which the calendar keeps (an
/// occurrence's makes it an exception), and with `sendResponse` (the default) the answer to the
/// organizer: a `COUNTER` when it proposes a new time.
fn respond(
    call: &Call,
    params: &HashMap<String, String>,
    body: &[u8],
    answer: Answer,
) -> Result<Response> {
    let known: &[&str] = match answer {
        Answer::Accept => &["comment", "sendResponse"],
        _ => &["comment", "sendResponse", "proposedNewTime"],
    };
    let request = parameters(body, answer.action(), known)?;
    let send = request
        .get("sendResponse")
        .map(flag)
        .transpose()?
        .unwrap_or(true);
    let comment = request
        .get("comment")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let proposed = request
        .get("proposedNewTime")
        .filter(|v| !v.is_null())
        .map(time_slot)
        .transpose()?;
    let mut world = call.calendar_world();
    let origin = origin(&world);
    let (calendar, mut event) = {
        let view = call.view(&world, &origin);
        let (calendar, event) = view.event_of(params)?;
        if !view.can_edit(&calendar) {
            return Err(GraphError::access_denied());
        }
        (calendar, event)
    };
    let me = world.calendar.primary.clone();
    if organizes(&me, &event) {
        return Err(GraphError::invalid_request(
            "Your request can't be completed. You can't respond to a meeting you organized.",
        ));
    }
    let stored = super::events::stored(&world.graph, &calendar, &event);
    if proposed.is_some() {
        if !send {
            return Err(GraphError::invalid_request(
                "A proposed new time is sent to the organizer, which needs sendResponse.",
            ));
        }
        if stored.get("allowNewTimeProposals") == Some(&Value::Bool(false)) {
            return Err(GraphError::invalid_request(
                "The organizer doesn't take new time proposals for this meeting.",
            ));
        }
    }
    match event.attendees.iter_mut().find(|attendee| {
        attendee.is_self == Some(true)
            || attendee
                .email
                .as_deref()
                .is_some_and(|email| email.eq_ignore_ascii_case(&me))
    }) {
        Some(attendee) => {
            attendee.response_status = Some(answer.google().into());
            attendee.comment = comment.clone().or(attendee.comment.take());
        }
        None => event.attendees.push(EventAttendee {
            email: Some(me.clone()),
            is_self: Some(true),
            response_status: Some(answer.google().into()),
            comment: comment.clone(),
            ..EventAttendee::default()
        }),
    }
    let event = crate::calendar::routes::touch(&world, event);
    world.calendar.put(&calendar, event.clone());
    if send {
        notify(
            &mut world,
            &call.who,
            &event,
            &stored,
            Notice::Reply(answer, proposed),
            comment.as_deref(),
        )?;
    }
    Ok(StatusCode::ACCEPTED.into_response())
}

/// `POST /me/events/{id}/cancel` (202): the organizer cancels the meeting (or one occurrence)
/// and its attendees get the cancellation, with the comment.
pub async fn cancel(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let request = parameters(&body, "cancel", &["comment"])?;
    let comment = request
        .get("comment")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut world = call.calendar_world();
    let origin = origin(&world);
    let (calendar, event) = {
        let view = call.view(&world, &origin);
        let (calendar, event) = view.event_of(&params)?;
        if !view.can_edit(&calendar) {
            return Err(GraphError::access_denied());
        }
        (calendar, event)
    };
    if !organizes(&world.calendar.primary, &event) {
        return Err(GraphError::invalid_request(
            "Your request can't be completed. You need to be an organizer to cancel a meeting.",
        ));
    }
    let stored = super::events::stored(&world.graph, &calendar, &event);
    crate::calendar::routes::cancel_event(&mut world, &calendar, event.clone());
    notify(
        &mut world,
        &call.who,
        &event,
        &stored,
        Notice::Cancel,
        comment.as_deref(),
    )?;
    Ok(StatusCode::ACCEPTED.into_response())
}
