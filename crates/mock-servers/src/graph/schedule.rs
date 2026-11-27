//! Scheduling: free/busy (`getSchedule`), meeting time suggestions (`findMeetingTimes`) and the
//! tenant's rooms (`/places`), all for work accounts, as Outlook.com has none of them.
//!
//! Free/busy comes from the world's calendar: the user's own primary calendar, a colleague's as
//! the user's calendars show them (the events they organize or attend), and a room's from the
//! events that book it (invite it, or are held in it).

use std::collections::HashMap;

use axum::extract::Path;
use axum::response::Response;
use gmail::calendar::Event;
use jiff::{SignedDuration, Timestamp, ToSpan};
use serde_json::{Map, Value, json};

use super::error::{GraphError, Result};
use super::events::date_time;
use super::filter::Expr;
use super::project::View;
use super::{Call, odata, origin, reply};
use crate::world::World;

/// A room's mailbox at Brightmoor's office.
struct Room {
    name: &'static str,
    email: &'static str,
    nickname: &'static str,
    capacity: i32,
    floor: i32,
    label: &'static str,
    display: Option<&'static str>,
    video: Option<&'static str>,
    audio: Option<&'static str>,
}

/// The rooms the calendar's meetings are held in.
const ROOMS: &[Room] = &[
    Room {
        name: "Hudson Room",
        email: "hudson@brightmoor.co",
        nickname: "Hudson",
        capacity: 10,
        floor: 4,
        label: "4.01",
        display: Some("Display"),
        video: Some("Camera"),
        audio: Some("Speakerphone"),
    },
    Room {
        name: "Big conference room",
        email: "bigroom@brightmoor.co",
        nickname: "Big room",
        capacity: 18,
        floor: 4,
        label: "4.10",
        display: Some("Display"),
        video: Some("Camera"),
        audio: Some("Speakerphone"),
    },
];

/// The office's room list.
const ROOM_LIST: (&str, &str) = ("Brightmoor HQ", "hq-rooms@brightmoor.co");

/// Scheduling is a work account's; personal ones are refused.
fn work_only(call: &Call) -> Result<()> {
    if call.who.personal() {
        return Err(GraphError::access_denied());
    }
    Ok(())
}

/// A stretch of someone's schedule.
#[derive(Debug, Clone)]
struct Busy {
    start: Timestamp,
    end: Timestamp,
    /// `free`, `tentative`, `busy`, `oof` or `workingElsewhere`.
    status: String,
    subject: String,
    location: String,
    private: bool,
}

impl Busy {
    /// How much it blocks, for the availability view and suggestions: free (and working
    /// elsewhere) 0, tentative 1, busy 2, away 3.
    fn weight(&self) -> u8 {
        match self.status.as_str() {
            "tentative" => 1,
            "busy" => 2,
            "oof" => 3,
            _ => 0,
        }
    }
}

fn busy_from(event: &Event, status: &str, view: &View<'_>) -> Busy {
    Busy {
        start: view.calendars.start(event),
        end: view.calendars.end(event),
        status: status.to_owned(),
        subject: event.summary.clone().unwrap_or_default(),
        location: event.location.clone().unwrap_or_default(),
        private: matches!(
            event.visibility.as_deref(),
            Some("private" | "confidential")
        ),
    }
}

/// `address`'s schedule from `min` to `max`, or `None` for someone the mock can't see.
fn schedule(
    view: &View<'_>,
    world: &World,
    address: &str,
    min: Timestamp,
    max: Timestamp,
) -> Option<Vec<Busy>> {
    let calendars = view.calendars;
    if address.eq_ignore_ascii_case(&calendars.primary) {
        let showing = vec!["showAs".to_owned()];
        return Some(
            view.occurrences(&calendars.primary, min, max)
                .iter()
                .map(|event| {
                    let status = view
                        .event_json(&calendars.primary, event, Some(&showing))
                        .ok()
                        .and_then(|json| {
                            json.get("showAs")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .unwrap_or_else(|| "busy".into());
                    busy_from(event, &status, view)
                })
                .collect(),
        );
    }
    if let Some(room) = ROOMS
        .iter()
        .find(|room| room.email.eq_ignore_ascii_case(address))
    {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for entry in calendars.entries.iter().filter_map(|e| e.id.clone()) {
            for event in view.occurrences(&entry, min, max) {
                let invited = event.attendees.iter().any(|a| {
                    a.email
                        .as_deref()
                        .is_some_and(|e| e.eq_ignore_ascii_case(room.email))
                        && a.response_status.as_deref() != Some("declined")
                });
                let held = event
                    .location
                    .as_deref()
                    .is_some_and(|l| l.to_lowercase().contains(&room.name.to_lowercase()));
                if (invited || held) && seen.insert(event.id.clone()) {
                    out.push(busy_from(&event, "busy", view));
                }
            }
        }
        return Some(out);
    }
    let colleague = crate::calendar::routes::colleague(world, address)
        && super::people::is_user(world, address);
    if !colleague {
        return None;
    }
    Some(
        crate::calendar::routes::colleague_occurrences(world, address, min, max)
            .into_iter()
            .filter(|event| event.status.as_deref() != Some("cancelled"))
            .map(|event| {
                let theirs =
                    |email: Option<&str>| email.is_some_and(|e| e.eq_ignore_ascii_case(address));
                let organizes = theirs(event.organizer.as_ref().and_then(|o| o.email.as_deref()));
                let response = event
                    .attendees
                    .iter()
                    .find(|a| theirs(a.email.as_deref()))
                    .and_then(|a| a.response_status.clone());
                let status = if event.event_type.as_deref() == Some("outOfOffice") {
                    "oof"
                } else if event.transparency.as_deref() == Some("transparent")
                    || response.as_deref() == Some("declined")
                {
                    "free"
                } else if organizes || response.as_deref() == Some("accepted") {
                    "busy"
                } else {
                    "tentative"
                };
                busy_from(&event, status, view)
            })
            .collect(),
    )
}

/// A weight's free/busy status.
const STATUSES: [&str; 4] = ["free", "tentative", "busy", "oof"];

/// How busy `schedule` is over `[start, end)`: the heaviest thing in it.
fn weight_in(schedule: &[Busy], start: Timestamp, end: Timestamp) -> u8 {
    schedule
        .iter()
        .filter(|busy| busy.start < end && busy.end > start)
        .map(Busy::weight)
        .max()
        .unwrap_or(0)
}

/// A request body's properties by name in any case.
fn body_of(body: &[u8]) -> Result<HashMap<String, Value>> {
    let request: Map<String, Value> = super::folders::body_json(body, "event")?;
    Ok(request
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value))
        .collect())
}

/// `POST /me/calendar/getSchedule`: free/busy for users, rooms and the user, with the
/// availability view (a digit per interval: 0 free, 1 tentative, 2 busy, 3 away) and the
/// schedule's items in the preferred zone.
pub async fn get_schedule(call: Call, body: axum::body::Bytes) -> Result<Response> {
    work_only(&call)?;
    let request = body_of(&body)?;
    let schedules: Vec<String> = request
        .get("schedules")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .filter(|list: &Vec<String>| !list.is_empty())
        .ok_or_else(|| GraphError::invalid_request("schedules needs at least one address."))?;
    let time = |name: &str| {
        request
            .get(name)
            .and_then(odata::parse_date_time_zone)
            .ok_or_else(|| GraphError::invalid_request(format!("{name} is required.")))
    };
    let (start, end) = (time("starttime")?, time("endtime")?);
    if end <= start {
        return Err(GraphError::invalid_request(
            "endTime must be after startTime.",
        ));
    }
    let interval = match request.get("availabilityviewinterval") {
        None | Some(Value::Null) => 30,
        Some(value) => value
            .as_i64()
            .filter(|minutes| (5..=1440).contains(minutes))
            .ok_or_else(|| {
                GraphError::invalid_request(
                    "availabilityViewInterval must be from 5 to 1440 minutes.",
                )
            })?,
    };
    let world = call.calendar_world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let zone = view.prefer.zone.clone();
    let step = SignedDuration::from_mins(interval);
    let mut value = Vec::new();
    for address in schedules {
        let Some(items) = schedule(&view, &world, &address, start, end) else {
            value.push(json!({
                "scheduleId": address,
                "availabilityView": "",
                "error": {
                    "message": format!("The specified recipient '{address}' was not found."),
                    "responseCode": "ErrorMailRecipientNotFound",
                },
            }));
            continue;
        };
        let mut view_digits = String::new();
        let mut at = start;
        while at < end {
            let next = (at + step).min(end);
            view_digits.push(char::from(b'0' + weight_in(&items, at, next)));
            at = next;
        }
        let entries: Vec<Value> = items
            .iter()
            .map(|busy| {
                let mut item = json!({
                    "isPrivate": busy.private,
                    "status": busy.status,
                    "start": date_time(busy.start, zone.as_deref()),
                    "end": date_time(busy.end, zone.as_deref()),
                });
                if !busy.private {
                    item["subject"] = busy.subject.clone().into();
                    item["location"] = busy.location.clone().into();
                }
                item
            })
            .collect();
        let hours = if address.eq_ignore_ascii_case(&view.calendars.primary) {
            super::settings::working_hours(&world)
        } else {
            super::settings::default_hours(view.tz.iana_name().unwrap_or("UTC"))
        };
        value.push(json!({
            "scheduleId": address,
            "availabilityView": view_digits,
            "scheduleItems": entries,
            "workingHours": hours,
        }));
    }
    Ok(reply(
        200,
        json!({
            "@odata.context": odata::context(&origin, "Collection(microsoft.graph.scheduleInformation)"),
            "value": value,
        }),
    ))
}

/// Who a suggestion is for: an attendee's address, name and type.
struct Invitee {
    address: String,
    name: Option<String>,
    kind: String,
}

/// `POST /me/findMeetingTimes`: times in the window when the organizer and enough attendees
/// are free, best first (by confidence, then time), with a room when one is asked for.
pub async fn find_meeting_times(call: Call, body: axum::body::Bytes) -> Result<Response> {
    work_only(&call)?;
    let request = body_of(&body)?;
    let flag = |name: &str| match request.get(name) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(text)) => text.eq_ignore_ascii_case("true"),
        _ => false,
    };
    let invitees: Vec<Invitee> = request
        .get("attendees")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|attendee| {
            let address = attendee["emailAddress"]["address"].as_str()?.to_owned();
            Some(Invitee {
                address,
                name: attendee["emailAddress"]["name"].as_str().map(str::to_owned),
                kind: attendee["type"].as_str().unwrap_or("required").to_owned(),
            })
        })
        .collect();
    let duration = match request.get("meetingduration").and_then(Value::as_str) {
        Some(text) => text
            .parse::<SignedDuration>()
            .ok()
            .filter(|d| d.is_positive())
            .ok_or_else(|| {
                GraphError::invalid_request(format!("'{text}' isn't a meeting duration."))
            })?,
        None => SignedDuration::from_mins(30),
    };
    let minimum = request
        .get("minimumattendeepercentage")
        .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
        .unwrap_or(50.0);
    let max = request
        .get("maxcandidates")
        .and_then(Value::as_u64)
        .map_or(5, |n| usize::try_from(n).unwrap_or(usize::MAX));
    let organizer_optional = flag("isorganizeroptional");
    let reasons = flag("returnsuggestionreasons");
    let constraint = request
        .get("timeconstraint")
        .cloned()
        .unwrap_or(Value::Null);
    let domain = constraint
        .get("activityDomain")
        .and_then(Value::as_str)
        .unwrap_or("work")
        .to_owned();
    let location = request
        .get("locationconstraint")
        .cloned()
        .unwrap_or(Value::Null);

    let world = call.calendar_world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let tz = view.tz.clone();
    let now = world.clock.now();
    // Without time slots, the next two days from the next half hour.
    let slots: Vec<(Timestamp, Timestamp)> =
        match constraint.get("timeSlots").and_then(Value::as_array) {
            Some(slots) if !slots.is_empty() => slots
                .iter()
                .filter_map(|slot| {
                    Some((
                        odata::parse_date_time_zone(&slot["start"])?,
                        odata::parse_date_time_zone(&slot["end"])?,
                    ))
                })
                .collect(),
            _ => {
                let half_hour = 30 * 60;
                let from = Timestamp::from_second(
                    (now.as_second() + half_hour - 1) / half_hour * half_hour,
                )
                .unwrap_or(now);
                vec![(from, from + SignedDuration::from_hours(48))]
            }
        };
    let window = (
        slots.iter().map(|s| s.0).min().unwrap_or(now),
        slots.iter().map(|s| s.1).max().unwrap_or(now),
    );
    let me = view.calendars.primary.clone();
    let mine = schedule(&view, &world, &me, window.0, window.1).unwrap_or_default();
    let theirs: Vec<Option<Vec<Busy>>> = invitees
        .iter()
        .map(|i| schedule(&view, &world, &i.address, window.0, window.1))
        .collect();
    let hours = super::settings::working_hours(&world);
    let (open, close) = (
        hours["startTime"]
            .as_str()
            .and_then(|t| t[..8].parse::<jiff::civil::Time>().ok()),
        hours["endTime"]
            .as_str()
            .and_then(|t| t[..8].parse::<jiff::civil::Time>().ok()),
    );
    let (open, close) = (
        open.unwrap_or(jiff::civil::time(8, 0, 0, 0)),
        close.unwrap_or(jiff::civil::time(17, 0, 0, 0)),
    );
    let workdays: Vec<String> = hours["daysOfWeek"]
        .as_array()
        .map(|days| {
            days.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let in_hours = |start: Timestamp, end: Timestamp| -> bool {
        if domain == "unrestricted" {
            return true;
        }
        let (from, to) = (start.to_zoned(tz.clone()), end.to_zoned(tz.clone()));
        let day = super::recurrence::day_name(from.weekday());
        let on_day = domain == "personal" || workdays.iter().any(|d| d == day);
        on_day
            && from.date() == (to.clone() - 1.nanosecond()).date()
            && from.time() >= open
            && to.time() <= close
    };
    let rooms: Vec<&Room> = match location.get("locations").and_then(Value::as_array) {
        Some(asked) if !asked.is_empty() => Vec::new(),
        _ if location.get("suggestLocation") == Some(&Value::Bool(true)) => ROOMS.iter().collect(),
        _ => Vec::new(),
    };
    let asked_places: Vec<Value> = location
        .get("locations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let location_required = location.get("isRequired") == Some(&Value::Bool(true));

    let mut organizer_blocked = false;
    let mut unknown = false;
    let mut no_room = false;
    let mut found: Vec<(f64, Timestamp, Value)> = Vec::new();
    let step = SignedDuration::from_mins(30);
    for (slot_start, slot_end) in &slots {
        let mut at = *slot_start;
        while at + duration <= *slot_end {
            let end = at + duration;
            if !in_hours(at, end) {
                at += step;
                continue;
            }
            let organizer = weight_in(&mine, at, end);
            if !organizer_optional && organizer >= 2 {
                organizer_blocked = true;
                at += step;
                continue;
            }
            let mut chances = Vec::new();
            let mut availability = Vec::new();
            for (invitee, busy) in invitees.iter().zip(&theirs) {
                let status = match busy {
                    Some(busy) => STATUSES[usize::from(weight_in(busy, at, end))],
                    None => {
                        unknown = true;
                        "unknown"
                    }
                };
                if invitee.kind != "resource" {
                    chances.push(match status {
                        "free" | "tentative" => 100.0,
                        "unknown" => 49.0,
                        _ => 0.0,
                    });
                }
                let mut attendee =
                    json!({ "type": invitee.kind, "emailAddress": { "address": invitee.address } });
                if let Some(name) = &invitee.name {
                    attendee["emailAddress"]["name"] = name.clone().into();
                }
                availability.push(json!({ "attendee": attendee, "availability": status }));
            }
            let confidence = if chances.is_empty() {
                100.0
            } else {
                chances.iter().sum::<f64>() / chances.len() as f64
            };
            if confidence < minimum {
                at += step;
                continue;
            }
            let places: Vec<Value> = if !asked_places.is_empty() {
                asked_places
                    .iter()
                    .filter(|place| {
                        let resolve = place.get("resolveAvailability") == Some(&Value::Bool(true));
                        let email = place.get("locationEmailAddress").and_then(Value::as_str);
                        !resolve
                            || email.is_none_or(|email| {
                                schedule(&view, &world, email, at, end)
                                    .is_none_or(|busy| weight_in(&busy, at, end) < 2)
                            })
                    })
                    .map(|place| {
                        let mut out = json!({ "displayName": place.get("displayName").cloned().unwrap_or_default() });
                        if let Some(email) = place.get("locationEmailAddress") {
                            out["locationEmailAddress"] = email.clone();
                        }
                        out
                    })
                    .collect()
            } else {
                rooms
                    .iter()
                    .find(|room| {
                        schedule(&view, &world, room.email, at, end)
                            .is_none_or(|busy| weight_in(&busy, at, end) < 2)
                    })
                    .map(|room| {
                        vec![
                            json!({ "displayName": room.name, "locationEmailAddress": room.email }),
                        ]
                    })
                    .unwrap_or_default()
            };
            if location_required && places.is_empty() {
                no_room = true;
                at += step;
                continue;
            }
            let zone = view.prefer.zone.as_deref();
            let mut suggestion = json!({
                "confidence": (confidence * 100.0).round() / 100.0,
                "organizerAvailability": STATUSES[usize::from(organizer)],
                "attendeeAvailability": availability,
                "locations": places,
                "meetingTimeSlot": { "start": date_time(at, zone), "end": date_time(end, zone) },
            });
            if reasons {
                suggestion["suggestionReason"] = if confidence >= 100.0 {
                    "Suggested because it is one of the nearest times when all attendees are available."
                } else {
                    "Suggested because it is one of the nearest times when most attendees are available."
                }
                .into();
            }
            found.push((confidence, at, suggestion));
            at += step;
        }
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let suggestions: Vec<Value> = found
        .into_iter()
        .take(max)
        .enumerate()
        .map(|(order, (_, _, mut suggestion))| {
            suggestion["order"] = (order + 1).into();
            suggestion
        })
        .collect();
    let empty = if !suggestions.is_empty() {
        ""
    } else if no_room {
        "locationsUnavailable"
    } else if organizer_blocked && invitees.is_empty() {
        "organizerUnavailable"
    } else if unknown {
        "attendeesUnavailableOrUnknown"
    } else if !invitees.is_empty() {
        "attendeesUnavailable"
    } else if organizer_blocked {
        "organizerUnavailable"
    } else {
        "unknown"
    };
    Ok(reply(
        200,
        json!({
            "@odata.context": odata::context(&origin, "microsoft.graph.meetingTimeSuggestionsResult"),
            "emptySuggestionsReason": empty,
            "meetingTimeSuggestions": suggestions,
        }),
    ))
}

/// Every property of `room`, with `place`'s.
const ROOM_PROPERTIES: &[&str] = &[
    "id",
    "displayName",
    "emailAddress",
    "address",
    "geoCoordinates",
    "phone",
    "nickname",
    "label",
    "capacity",
    "building",
    "floorNumber",
    "floorLabel",
    "isWheelChairAccessible",
    "bookingType",
    "tags",
    "audioDeviceName",
    "videoDeviceName",
    "displayDeviceName",
    "placeId",
    "parentId",
    "teamsEnabledState",
];

/// Every property of `roomList`, with `place`'s.
const ROOM_LIST_PROPERTIES: &[&str] = &[
    "id",
    "displayName",
    "emailAddress",
    "address",
    "geoCoordinates",
    "phone",
    "placeId",
    "tags",
];

fn room_json(room: &Room) -> Map<String, Value> {
    let list = super::ids::guid(&format!("place/{}", ROOM_LIST.1));
    let json = json!({
        "id": super::ids::guid(&format!("room/{}", room.email)).to_uppercase(),
        "displayName": room.name,
        "emailAddress": room.email,
        "address": null,
        "geoCoordinates": null,
        "phone": null,
        "nickname": room.nickname,
        "label": room.label,
        "capacity": room.capacity,
        "building": ROOM_LIST.0,
        "floorNumber": room.floor,
        "floorLabel": room.floor.to_string(),
        "isWheelChairAccessible": true,
        "bookingType": "standard",
        "tags": [],
        "audioDeviceName": room.audio,
        "videoDeviceName": room.video,
        "displayDeviceName": room.display,
        "placeId": super::ids::guid(&format!("place/{}", room.email)),
        "parentId": list,
        "teamsEnabledState": "unknown",
    });
    json.as_object().cloned().unwrap_or_default()
}

fn room_list_json() -> Map<String, Value> {
    let json = json!({
        "id": super::ids::guid(&format!("roomList/{}", ROOM_LIST.1)).to_uppercase(),
        "displayName": ROOM_LIST.0,
        "emailAddress": ROOM_LIST.1,
        "address": null,
        "geoCoordinates": null,
        "phone": null,
        "placeId": super::ids::guid(&format!("place/{}", ROOM_LIST.1)),
        "tags": [],
    });
    json.as_object().cloned().unwrap_or_default()
}

/// A page of places, with `$filter`, `$select`, `$top` (100 by default), `$skip` and `$count`.
fn list_places(
    call: &Call,
    path: &str,
    places: Vec<Map<String, Value>>,
    known: &[&str],
    kind: &str,
) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let filter = call
        .options
        .filter
        .as_deref()
        .map(Expr::parse)
        .transpose()?;
    let all: Vec<Map<String, Value>> = places
        .into_iter()
        .filter(|json| {
            filter
                .as_ref()
                .is_none_or(|f| f.matches(&Value::Object(json.clone())))
        })
        .collect();
    let total = all.len();
    let top = call.options.top.unwrap_or(100).clamp(1, 1000);
    let skip = call.options.skip;
    let value = all
        .into_iter()
        .skip(skip)
        .take(top)
        .map(|json| {
            odata::select(json, call.options.select.as_deref(), known, kind).map(Value::Object)
        })
        .collect::<Result<Vec<_>>>()?;
    let next = (skip + top < total)
        .then(|| odata::link(&origin, path, &odata::next_page(&call.query, skip + top)));
    Ok(reply(
        200,
        odata::collection(
            odata::context(&origin, path),
            value,
            call.options.count.then_some(total),
            next,
        ),
    ))
}

/// `GET /places/microsoft.graph.room`.
pub async fn rooms(call: Call) -> Result<Response> {
    work_only(&call)?;
    let rooms = ROOMS.iter().map(room_json).collect();
    list_places(
        &call,
        "places/microsoft.graph.room",
        rooms,
        ROOM_PROPERTIES,
        "room",
    )
}

/// `GET /places/microsoft.graph.roomList`.
pub async fn room_lists(call: Call) -> Result<Response> {
    work_only(&call)?;
    list_places(
        &call,
        "places/microsoft.graph.roomList",
        vec![room_list_json()],
        ROOM_LIST_PROPERTIES,
        "roomList",
    )
}

/// `GET /places/{room list's address}/microsoft.graph.roomList/rooms`.
pub async fn rooms_in(call: Call, Path(params): super::Params) -> Result<Response> {
    work_only(&call)?;
    if !params["place"].eq_ignore_ascii_case(ROOM_LIST.1) {
        return Err(GraphError::not_found());
    }
    let rooms = ROOMS.iter().map(room_json).collect();
    list_places(
        &call,
        &format!("places/{}/microsoft.graph.roomList/rooms", ROOM_LIST.1),
        rooms,
        ROOM_PROPERTIES,
        "room",
    )
}

/// `GET /places/{id or address}`: a room or the room list, typed.
pub async fn place(call: Call, Path(params): super::Params) -> Result<Response> {
    work_only(&call)?;
    let wanted = super::percent(&params["place"]);
    let matches = |json: &Map<String, Value>| {
        ["id", "emailAddress"].iter().any(|key| {
            json.get(*key)
                .and_then(Value::as_str)
                .is_some_and(|v| v.eq_ignore_ascii_case(&wanted))
        })
    };
    let world = call.world();
    let origin = origin(&world);
    let found = ROOMS
        .iter()
        .map(|room| ("room", room_json(room), ROOM_PROPERTIES))
        .chain(std::iter::once((
            "roomList",
            room_list_json(),
            ROOM_LIST_PROPERTIES,
        )))
        .find(|(_, json, _)| matches(json))
        .ok_or_else(GraphError::not_found)?;
    let (kind, json, known) = found;
    let mut out = Map::new();
    out.insert(
        "@odata.context".into(),
        odata::context(&origin, "places/$entity").into(),
    );
    out.insert(
        "@odata.type".into(),
        format!("#microsoft.graph.{kind}").into(),
    );
    out.extend(odata::select(
        json,
        call.options.select.as_deref(),
        known,
        kind,
    )?);
    Ok(reply(200, Value::Object(out)))
}
