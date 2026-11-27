use std::sync::{
    LazyLock,
    atomic::{AtomicU64, Ordering},
};

use exo_store::{
    CalendarEvent, CalendarProviderKind, Conference, ConferenceKind, ConferencePatch,
    EventAttendee, EventPatch, EventPerson, EventTime, OnlineMeetingKind, RsvpStatus,
};
use jiff::{SignedDuration, ToSpan, Zoned};
use regex::Regex;

use super::{
    event::Event,
    form::{Field, Guest, Meeting, Values},
};

/// The 1:1 title patterns a new event without guests is asked about, compiled once.
static ONE_ON_ONE: LazyLock<[Regex; 6]> = LazyLock::new(|| {
    [
        r"(?i)\b1\s*[:x-]\s*1\b",
        r"(?i)\b1\s+on\s+1\b",
        r"(?i)\b[a-z0-9][a-z0-9.'&-]*\s*<>\s*[a-z0-9][a-z0-9.'&-]*\b",
        r"(?i)\b[a-z0-9][a-z0-9.'&-]*\s+\+\s+[a-z0-9][a-z0-9.'&-]*\b",
        r"(?i)^[a-z0-9][a-z0-9.'&-]*\s+and\s+[a-z0-9][a-z0-9.'&-]*$",
        r"(?i)^[a-z0-9][a-z0-9.'&-]*\s*:\s*[a-z0-9][a-z0-9.'&-]*(?:\s+[a-z0-9][a-z0-9.'&-]*)?$",
    ]
    .map(|pattern| Regex::new(pattern).expect("valid 1:1 pattern"))
});

/// Whether `title` looks like a 1:1 meeting (`1:1`, `A <> B`, `A and B`, ...).
pub(super) fn is_one_on_one(title: &str) -> bool {
    ONE_ON_ONE.iter().any(|pattern| pattern.is_match(title))
}

/// Counts ids, in case the system's randomness is out.
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// An id for a new event, chosen here because Calendar takes a client's own. It's 32 hex digits,
/// which are base32hex.
pub(super) fn new_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        let fallback = jiff::Timestamp::now().as_nanosecond() as u128
            ^ u128::from(NEXT_ID.fetch_add(1, Ordering::Relaxed));
        bytes = fallback.to_le_bytes();
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The form's values for `event`, with its notes as HTML for About's editor and a plain-text
/// description's lines kept as lines.
pub(super) fn values_of(event: &Event, source: &CalendarEvent, me: &str) -> Values {
    let description = source.description.clone().unwrap_or_default();
    let about = if looks_like_html(&description) {
        description
    } else {
        description
            .lines()
            .map(|line| format!("<div>{}</div>", exo_mail::escape_html_text(line)))
            .collect()
    };
    let meeting = match &source.conference {
        Some(conf) if conf.pending => match conf.kind {
            ConferenceKind::Teams => Meeting::NewTeams,
            _ => Meeting::NewMeet,
        },
        _ => super::event::video_uri_of(source).map_or(Meeting::None, Meeting::Existing),
    };
    Values {
        title: source.summary.clone().unwrap_or_default(),
        when: Some((event.start.clone(), event.end.clone())),
        all_day: source.start.as_ref().is_some_and(EventTime::is_all_day),
        guests: source
            .attendees
            .iter()
            .filter(|attendee| !attendee.resource)
            .filter_map(|attendee| {
                let email = attendee.email.clone()?;
                Some(Guest {
                    me: attendee.is_me(me),
                    name: attendee.display_name.clone(),
                    optional: attendee.optional,
                    response: Some(attendee.response_status),
                    organizer: attendee.organizer,
                    email,
                })
            })
            .collect(),
        location: source.location.clone().unwrap_or_default(),
        about,
        meeting,
        calendar: event.calendar_id.clone(),
        repeat: source.recurrence.clone(),
    }
}

/// Whether `text` appears to contain HTML markup rather than plain text (which may still use `<`
/// in inequalities like `"Budget < $1000"` or angle-bracketed emails/URLs).
fn looks_like_html(text: &str) -> bool {
    const TAGS: &[&str] = &[
        "a",
        "b",
        "blockquote",
        "br",
        "code",
        "del",
        "div",
        "em",
        "font",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "hr",
        "i",
        "img",
        "li",
        "ol",
        "p",
        "pre",
        "s",
        "span",
        "strike",
        "strong",
        "sub",
        "sup",
        "table",
        "tbody",
        "td",
        "th",
        "thead",
        "tr",
        "u",
        "ul",
    ];
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(rel) = text[i..].find('<') {
        let start = i + rel + 1;
        let name_start = if bytes.get(start) == Some(&b'/') {
            start + 1
        } else {
            start
        };
        let mut end = name_start;
        while bytes.get(end).is_some_and(u8::is_ascii_alphanumeric) {
            end += 1;
        }
        if end > name_start {
            let name = &text[name_start..end];
            let boundary = bytes
                .get(end)
                .is_some_and(|b| matches!(b, b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n'));
            if boundary
                && TAGS.iter().any(|tag| tag.eq_ignore_ascii_case(name))
                && text[end..].contains('>')
            {
                return true;
            }
        }
        i = start;
    }
    false
}

/// The guests as Calendar's attendees. When there's anyone else (or a booked room), preserves
/// `existing` room/resource attendees, `organizer` flags, `mine.email`, and untouched attendee
/// metadata (`id`, `comment`, `additional_guests`).
fn attendees(values: &Values, me: &str, existing: &[EventAttendee]) -> Vec<EventAttendee> {
    let others: Vec<&Guest> = values
        .guests
        .iter()
        .filter(|guest| !guest.me && !guest.email.eq_ignore_ascii_case(me))
        .collect();
    let resources: Vec<EventAttendee> = existing
        .iter()
        .filter(|attendee| attendee.resource)
        .cloned()
        .collect();
    if others.is_empty() && resources.is_empty() {
        return Vec::new();
    }
    let mine = values.guests.iter().find(|guest| guest.me).or_else(|| {
        values
            .guests
            .iter()
            .find(|guest| guest.email.eq_ignore_ascii_case(me))
    });
    let mut list = Vec::new();
    if !others.is_empty() || mine.is_some() {
        let my_email = mine.map_or(me, |guest| guest.email.as_str());
        let prior_mine = existing
            .iter()
            .find(|attendee| !attendee.resource && attendee.is_me(my_email));
        list.push(EventAttendee {
            id: prior_mine.and_then(|attendee| attendee.id.clone()),
            email: Some(my_email.to_owned()),
            display_name: mine
                .and_then(|guest| guest.name.clone())
                .or_else(|| prior_mine.and_then(|attendee| attendee.display_name.clone())),
            organizer: match mine {
                Some(guest) if guest.organizer => true,
                Some(_) => prior_mine.is_some_and(|attendee| attendee.organizer),
                None => true,
            },
            is_self: prior_mine.is_some_and(|attendee| attendee.is_self),
            resource: false,
            optional: mine.is_some_and(|guest| guest.optional),
            response_status: mine
                .and_then(|guest| guest.response)
                .unwrap_or(RsvpStatus::Accepted),
            comment: prior_mine.and_then(|attendee| attendee.comment.clone()),
            additional_guests: prior_mine.and_then(|attendee| attendee.additional_guests),
        });
    }
    list.extend(others.into_iter().map(|guest| {
        let prior = existing.iter().find(|attendee| {
            !attendee.resource
                && attendee
                    .email
                    .as_deref()
                    .is_some_and(|email| email.eq_ignore_ascii_case(&guest.email))
        });
        EventAttendee {
            id: prior.and_then(|attendee| attendee.id.clone()),
            email: Some(guest.email.clone()),
            display_name: guest.name.clone(),
            organizer: guest.organizer || prior.is_some_and(|attendee| attendee.organizer),
            is_self: false,
            resource: false,
            optional: guest.optional,
            response_status: guest.response.unwrap_or(RsvpStatus::NeedsAction),
            comment: prior.and_then(|attendee| attendee.comment.clone()),
            additional_guests: prior.and_then(|attendee| attendee.additional_guests),
        }
    }));
    list.extend(resources);
    list
}

/// A new event from the form, its id chosen here so it shows before Calendar has it.
pub(super) fn new_event(
    values: &Values,
    start: &Zoned,
    end: &Zoned,
    me: &str,
    provider: CalendarProviderKind,
) -> CalendarEvent {
    let me_person = EventPerson {
        email: Some(me.to_owned()),
        display_name: None,
        is_self: true,
    };
    // A series starts on its rule's first day, with its end moved by as many days.
    let (start, end) = match values.repeat.as_ref().and_then(|rule| rule.first(start)) {
        Some(first) => {
            let days = start
                .date()
                .until(first.date())
                .map_or(0, |span| span.get_days());
            (
                first,
                end.checked_add(i64::from(days).days())
                    .unwrap_or_else(|_| end.clone()),
            )
        }
        None => (start.clone(), end.clone()),
    };
    let id = new_id();
    let conference = match values.meeting {
        Meeting::NewMeet => Some(Conference::pending(OnlineMeetingKind::GoogleMeet)),
        Meeting::NewTeams => Some(Conference::pending(OnlineMeetingKind::TeamsForBusiness)),
        Meeting::None | Meeting::Existing(_) => None,
    };
    CalendarEvent {
        id: Some(id.clone()),
        provider,
        summary: Some(values.title.trim().to_owned()),
        description: (!values.about.trim().is_empty()).then(|| values.about.clone()),
        location: (!values.location.trim().is_empty()).then(|| values.location.trim().to_owned()),
        start: Some(EventTime::from_zoned(&start, values.all_day)),
        end: Some(EventTime::from_zoned(&end, values.all_day)),
        attendees: attendees(values, me, &[]),
        conference,
        recurrence: values.repeat.clone(),
        organizer: Some(me_person.clone()),
        creator: Some(me_person),
        transaction_id: (provider == CalendarProviderKind::Microsoft).then_some(id),
        ..CalendarEvent::default()
    }
}

/// The changed fields as an [`EventPatch`]. When `original` is given, untouched attendee metadata,
/// room resources, and organizer flags are preserved across guest edits.
pub(super) fn patch_of(
    values: &Values,
    dirty: &[Field],
    me: &str,
    original: Option<&CalendarEvent>,
) -> EventPatch {
    let existing: &[EventAttendee] = original.map_or(&[], |event| event.attendees.as_slice());
    let mut patch = EventPatch::default();
    for field in dirty {
        match field {
            Field::Title => patch.summary = Some(values.title.trim().to_owned()),
            Field::When => {
                if let Some((start, end)) = &values.when {
                    patch.start = Some(EventTime::from_zoned(start, values.all_day));
                    patch.end = Some(EventTime::from_zoned(end, values.all_day));
                }
            }
            Field::Location => patch.location = Some(values.location.trim().to_owned()),
            Field::About => patch.description = Some(values.about.clone()),
            Field::Guests => {
                patch.attendees = Some(attendees(values, me, existing));
            }
            Field::Meeting => {
                patch.conference = match &values.meeting {
                    Meeting::NewMeet => {
                        Some(ConferencePatch::Create(OnlineMeetingKind::GoogleMeet))
                    }
                    Meeting::NewTeams => {
                        Some(ConferencePatch::Create(OnlineMeetingKind::TeamsForBusiness))
                    }
                    Meeting::None => {
                        // Microsoft Graph ignores `isOnlineMeeting` removal post-create.
                        let outlook_existing = original.is_some_and(|event| {
                            event.provider == CalendarProviderKind::Microsoft
                                && event.conference.as_ref().is_some_and(|conf| !conf.pending)
                        });
                        (!outlook_existing).then_some(ConferencePatch::Remove)
                    }
                    Meeting::Existing(_) => None,
                };
            }
            Field::Calendar | Field::Repeat => {}
        }
    }
    patch
}

/// How far an instance's civil wall-clock start and end moved, for its series (`All events` and
/// `Following`).
pub(super) fn shift_of(
    instance: &CalendarEvent,
    values: &Values,
) -> Option<(SignedDuration, SignedDuration)> {
    let (start, end) = values.when.as_ref()?;
    let tz = start.time_zone();
    let from = instance.start.as_ref()?.civil(tz)?;
    let to = instance.end.as_ref()?.civil(tz)?;
    Some((
        start.datetime().duration_since(from),
        end.datetime().duration_since(to),
    ))
}

#[cfg(test)]
mod tests {
    use jiff::{civil::Date, tz::TimeZone};

    use super::*;

    fn at(text: &str) -> Zoned {
        text.parse::<jiff::civil::DateTime>()
            .expect("time")
            .to_zoned(TimeZone::get("America/New_York").expect("zone"))
            .expect("exists")
    }

    fn values() -> Values {
        Values {
            title: "Lunch".into(),
            when: Some((at("2026-11-20T12:00"), at("2026-11-20T13:00"))),
            all_day: false,
            guests: Vec::new(),
            location: String::new(),
            about: String::new(),
            meeting: Meeting::None,
            calendar: "maya@brightmoor.co".into(),
            repeat: None,
        }
    }

    #[test]
    fn a_new_event_carries_its_times_and_guests() {
        let mut values = values();
        let (start, end) = values.when.clone().expect("a time");
        let event = new_event(
            &values,
            &start,
            &end,
            "maya@brightmoor.co",
            CalendarProviderKind::Google,
        );
        assert_eq!(event.start, Some(EventTime::from_zoned(&start, false)));
        assert!(event.attendees.is_empty(), "alone, no attendee list");
        assert!(event.id.is_some_and(|id| {
            id.bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'v'))
        }));
        values.guests.push(Guest {
            email: "lena@brightmoor.co".into(),
            name: None,
            optional: true,
            response: None,
            me: false,
            organizer: false,
        });
        values.all_day = true;
        let event = new_event(
            &values,
            &start,
            &end,
            "maya@brightmoor.co",
            CalendarProviderKind::Google,
        );
        let emails: Vec<_> = event
            .attendees
            .iter()
            .map(|attendee| {
                (
                    attendee.email.clone().unwrap_or_default(),
                    attendee.response_status,
                )
            })
            .collect();
        assert_eq!(
            emails,
            [
                ("maya@brightmoor.co".to_owned(), RsvpStatus::Accepted),
                ("lena@brightmoor.co".to_owned(), RsvpStatus::NeedsAction)
            ]
        );
        assert_eq!(
            event.start,
            Some(EventTime::all_day(Date::constant(2026, 11, 20)))
        );
    }

    #[test]
    fn outlook_new_event_sets_transaction_id_and_pending_teams() {
        let mut values = values();
        values.meeting = Meeting::NewTeams;
        let (start, end) = values.when.clone().expect("a time");
        let event = new_event(
            &values,
            &start,
            &end,
            "maya@brightmoor.co",
            CalendarProviderKind::Microsoft,
        );
        assert_eq!(event.provider, CalendarProviderKind::Microsoft);
        assert_eq!(event.transaction_id, event.id);
        let conf = event.conference.expect("pending teams");
        assert_eq!(conf.kind, ConferenceKind::Teams);
        assert!(conf.pending);
    }

    #[test]
    fn a_patch_has_only_what_changed() {
        let values = values();
        let patch = patch_of(&values, &[Field::Title], "maya@brightmoor.co", None);
        assert_eq!(
            patch,
            EventPatch {
                summary: Some("Lunch".into()),
                ..EventPatch::default()
            }
        );
        let removed = patch_of(
            &Values {
                meeting: Meeting::None,
                ..values
            },
            &[Field::Meeting],
            "maya@brightmoor.co",
            None,
        );
        assert_eq!(
            removed.conference,
            Some(ConferencePatch::Remove),
            "a removed link is cleared"
        );
    }

    #[test]
    fn one_on_ones_are_recognized() {
        assert!(
            is_one_on_one("Priya 1:1")
                && is_one_on_one("Maya <> Lena")
                && is_one_on_one("Maya and Lena")
                && is_one_on_one("Sync: Lena")
        );
        assert!(!is_one_on_one("Design crit"));
    }

    #[test]
    fn removing_all_guests_clears_attendees_locally() {
        let mut vals = values();
        vals.guests.clear();
        let patch = patch_of(&vals, &[Field::Guests], "maya@brightmoor.co", None);
        assert_eq!(patch.attendees, Some(Vec::new()));

        let mut after = CalendarEvent {
            id: Some("e1".into()),
            attendees: vec![EventAttendee {
                email: Some("lena@brightmoor.co".into()),
                ..EventAttendee::default()
            }],
            ..CalendarEvent::default()
        };
        after.apply_patch(&patch);
        assert!(after.attendees.is_empty(), "cleared locally in apply_patch");
    }

    #[test]
    fn patch_preserves_rooms_organizer_mine_email_and_attendee_metadata() {
        let original = CalendarEvent {
            id: Some("e1".into()),
            attendees: vec![
                EventAttendee {
                    id: Some("p-self".into()),
                    email: Some("alias@brightmoor.co".into()),
                    display_name: Some("Maya Alias".into()),
                    is_self: true,
                    organizer: false,
                    response_status: RsvpStatus::Accepted,
                    comment: Some("Joining remotely".into()),
                    additional_guests: Some(1),
                    ..EventAttendee::default()
                },
                EventAttendee {
                    id: Some("p-org".into()),
                    email: Some("boss@brightmoor.co".into()),
                    display_name: Some("Boss".into()),
                    organizer: true,
                    response_status: RsvpStatus::Accepted,
                    comment: Some("Bringing deck".into()),
                    additional_guests: Some(2),
                    ..EventAttendee::default()
                },
                EventAttendee {
                    id: Some("p-room".into()),
                    email: Some("oak-room@resource.calendar.google.com".into()),
                    display_name: Some("Oak Room".into()),
                    resource: true,
                    response_status: RsvpStatus::Accepted,
                    ..EventAttendee::default()
                },
            ],
            ..CalendarEvent::default()
        };
        let event = Event::draft(
            "e1".into(),
            "Review".into(),
            at("2026-11-20T12:00"),
            at("2026-11-20T13:00"),
            false,
            exo_ui::CalendarColor::BLUE,
        );
        let mut vals = values_of(&event, &original, "maya@brightmoor.co");
        vals.guests.push(Guest {
            email: "newguest@brightmoor.co".into(),
            name: Some("New Guest".into()),
            optional: true,
            response: None,
            me: false,
            organizer: false,
        });
        let patch = patch_of(
            &vals,
            &[Field::Guests],
            "maya@brightmoor.co",
            Some(&original),
        );
        let list = patch.attendees.expect("attendees");
        assert_eq!(list.len(), 4);

        let me_entry = &list[0];
        assert_eq!(me_entry.email.as_deref(), Some("alias@brightmoor.co"));
        assert_eq!(me_entry.id.as_deref(), Some("p-self"));
        assert!(me_entry.is_self);
        assert!(!me_entry.organizer);
        assert_eq!(me_entry.comment.as_deref(), Some("Joining remotely"));
        assert_eq!(me_entry.additional_guests, Some(1));

        let org_entry = &list[1];
        assert_eq!(org_entry.email.as_deref(), Some("boss@brightmoor.co"));
        assert_eq!(org_entry.id.as_deref(), Some("p-org"));
        assert!(org_entry.organizer);
        assert_eq!(org_entry.comment.as_deref(), Some("Bringing deck"));
        assert_eq!(org_entry.additional_guests, Some(2));

        let added = &list[2];
        assert_eq!(added.email.as_deref(), Some("newguest@brightmoor.co"));
        assert!(added.optional);
        assert_eq!(added.response_status, RsvpStatus::NeedsAction);

        let room = &list[3];
        assert_eq!(
            room.email.as_deref(),
            Some("oak-room@resource.calendar.google.com")
        );
        assert!(room.resource);
        assert_eq!(room.id.as_deref(), Some("p-room"));

        // Removing all human guests still preserves booked rooms.
        vals.guests.retain(|guest| guest.me);
        let room_only_patch = patch_of(
            &vals,
            &[Field::Guests],
            "maya@brightmoor.co",
            Some(&original),
        );
        let list = room_only_patch.attendees.expect("attendees");
        assert!(
            list.iter().any(|attendee| attendee.resource),
            "room booking survives removing human guests"
        );
    }

    #[test]
    fn plain_text_descriptions_with_angle_brackets_keep_line_breaks() {
        let event = Event::draft(
            "e1".into(),
            "Budget".into(),
            at("2026-11-20T12:00"),
            at("2026-11-20T13:00"),
            false,
            exo_ui::CalendarColor::BLUE,
        );
        let plain = CalendarEvent {
            description: Some("Budget < $1000\nContact <alice@example.com> & team".into()),
            ..CalendarEvent::default()
        };
        let vals = values_of(&event, &plain, "maya@brightmoor.co");
        assert_eq!(
            vals.about,
            "<div>Budget &lt; $1000</div><div>Contact &lt;alice@example.com&gt; &amp; team</div>"
        );

        let html = CalendarEvent {
            description: Some("<p>Agenda</p><ul><li>Item 1</li></ul>".into()),
            ..CalendarEvent::default()
        };
        assert_eq!(
            values_of(&event, &html, "maya@brightmoor.co").about,
            "<p>Agenda</p><ul><li>Item 1</li></ul>"
        );
        assert!(looks_like_html("Line 1<br>Line 2"));
        assert!(looks_like_html(
            "See <A href=\"https://example.com\">doc</A>"
        ));
        assert!(!looks_like_html("Budget < $1000"));
        assert!(!looks_like_html("Maya <> Lena"));
        assert!(!looks_like_html("<https://meet.google.com/abc-defg-hij>"));
    }

    #[test]
    fn apply_patch_sets_and_clears_conference() {
        let before = CalendarEvent {
            id: Some("e1".into()),
            conference: Some(Conference {
                kind: ConferenceKind::GoogleMeet,
                join_url: Some("https://meet.google.com/abc-defg-hij".into()),
                label: Some("meet.google.com/abc-defg-hij".into()),
                pending: false,
                dial_in: None,
            }),
            ..CalendarEvent::default()
        };
        let remove_patch = patch_of(
            &Values {
                meeting: Meeting::None,
                ..values()
            },
            &[Field::Meeting],
            "maya@brightmoor.co",
            Some(&before),
        );
        let mut cleared = before.clone();
        cleared.apply_patch(&remove_patch);
        assert_eq!(cleared.conference, None);
        assert_eq!(super::super::event::video_uri_of(&cleared), None);

        let add_patch = patch_of(
            &Values {
                meeting: Meeting::NewMeet,
                ..values()
            },
            &[Field::Meeting],
            "maya@brightmoor.co",
            Some(&cleared),
        );
        let mut added = cleared.clone();
        added.apply_patch(&add_patch);
        assert!(added.conference.as_ref().is_some_and(|c| c.pending));
    }

    #[test]
    fn shift_of_uses_wall_clock_across_dst_and_supports_all_day_and_toggles() {
        let tz = TimeZone::get("America/New_York").expect("zone");
        // 25h fall-back day (2026-11-01 in America/New_York): moving 00:30 EDT -> 09:00 EST is an
        // 8h30m wall-clock move (not 9h30m elapsed), so an EDT instance lands at 09:00 EDT.
        let instance = CalendarEvent {
            start: Some(EventTime::timed(
                "2026-11-01T00:30:00-04:00".parse().expect("ts"),
                Some("America/New_York".into()),
            )),
            end: Some(EventTime::timed(
                "2026-11-01T01:00:00-04:00".parse().expect("ts"),
                Some("America/New_York".into()),
            )),
            ..CalendarEvent::default()
        };
        let mut vals = values();
        vals.when = Some((at("2026-11-01T09:00"), at("2026-11-01T09:30")));
        let (ds, de) = shift_of(&instance, &vals).expect("shift");
        assert_eq!(ds, SignedDuration::from_mins(8 * 60 + 30));
        assert_eq!(de, SignedDuration::from_mins(8 * 60 + 30));

        let oct_instance = EventTime::timed(
            "2026-10-25T00:30:00-04:00".parse().expect("ts"),
            Some("America/New_York".into()),
        );
        let shifted = oct_instance
            .shift_wall_clock(ds, None, None)
            .expect("shifted");
        assert_eq!(
            shifted
                .zoned(&tz)
                .expect("zoned")
                .strftime("%Y-%m-%dT%H:%M:%S%:z")
                .to_string(),
            "2026-10-25T09:00:00-04:00"
        );

        // All-day instance toggling to timed 09:00–10:00 on Scope::All.
        let all_day_inst = CalendarEvent {
            start: Some(EventTime::all_day(Date::constant(2026, 11, 17))),
            end: Some(EventTime::all_day(Date::constant(2026, 11, 18))),
            ..CalendarEvent::default()
        };
        vals.all_day = false;
        vals.when = Some((at("2026-11-17T09:00"), at("2026-11-17T10:00")));
        let patch = patch_of(&vals, &[Field::When], "maya@brightmoor.co", None);
        let (ds, de) = shift_of(&all_day_inst, &vals).expect("all-day to timed shift");
        let earlier = EventTime::all_day(Date::constant(2026, 10, 27));
        let shifted_start = earlier
            .shift_wall_clock(
                ds,
                Some(false),
                patch.start.as_ref().and_then(EventTime::time_zone),
            )
            .expect("start");
        assert_eq!(
            shifted_start
                .zoned(&tz)
                .expect("zoned")
                .strftime("%Y-%m-%dT%H:%M:%S%:z")
                .to_string(),
            "2026-10-27T09:00:00-04:00"
        );
        let earlier_end = EventTime::all_day(Date::constant(2026, 10, 28));
        let shifted_end = earlier_end
            .shift_wall_clock(
                de,
                Some(false),
                patch.end.as_ref().and_then(EventTime::time_zone),
            )
            .expect("end");
        assert_eq!(
            shifted_end
                .zoned(&tz)
                .expect("zoned")
                .strftime("%Y-%m-%dT%H:%M:%S%:z")
                .to_string(),
            "2026-10-27T10:00:00-04:00"
        );
    }
}
