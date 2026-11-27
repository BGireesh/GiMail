//! Backward-compatible SQLite JSON serialization for cached calendars, events, and queued
//! calendar changes.
//!
//! New writes wrap provider-neutral values in a version-2 envelope (`{"v": 2, "data": ...}`).
//! Reads accept that envelope first and fall back to the legacy Google Calendar JSON shapes
//! written by earlier builds, preserving live upgrades, `store-0001.sqlite3`, and `Clear Cache`
//! without data loss.

use jiff::SignedDuration;
use jiff::civil::Date;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::google;
use super::model::{Calendar, CalendarEvent, EventTime, RsvpStatus};
use super::outbox::{CalendarChange, Cut, InviteLinks};

/// Version-2 marker that only deserializes from literal JSON integer `2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct V2Tag;

impl Serialize for V2Tag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(2)
    }
}

impl<'de> Deserialize<'de> for V2Tag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let version = u8::deserialize(deserializer)?;
        if version == 2 {
            Ok(Self)
        } else {
            Err(D::Error::custom(format!(
                "expected calendar storage version 2, got {version}"
            )))
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    v: V2Tag,
    data: T,
}

/// Storage wrapper stored inside toasty `Json<Compat<T>>` columns for `calendars.entry`,
/// `events.event`, and `calendar_changes.change`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Compat<T>(pub T);

impl<T> From<T> for Compat<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

impl<T: Serialize> Serialize for Compat<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Envelope {
            v: V2Tag,
            data: &self.0,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Compat<Calendar> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            V2(Envelope<Calendar>),
            Legacy(Box<gmail::calendar::CalendarListEntry>),
        }
        match Stored::deserialize(deserializer)? {
            Stored::V2(envelope) => Ok(Self(envelope.data)),
            Stored::Legacy(entry) => Ok(Self(google::calendar_from_google(*entry))),
        }
    }
}

impl<'de> Deserialize<'de> for Compat<CalendarEvent> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            V2(Box<Envelope<CalendarEvent>>),
            Legacy(Box<gmail::calendar::Event>),
        }
        match Stored::deserialize(deserializer)? {
            Stored::V2(envelope) => Ok(Self(envelope.data)),
            Stored::Legacy(event) => Ok(Self(google::event_from_google(*event))),
        }
    }
}

/// Deserializes a legacy Google patch [`gmail::calendar::Event`] while preserving explicit
/// `"conferenceData": null` (which `Option<serde_json::Value>` otherwise collapses to `None`).
struct LegacyGooglePatch(gmail::calendar::Event);

impl<'de> Deserialize<'de> for LegacyGooglePatch {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let clear_conference = raw
            .as_object()
            .and_then(|map| map.get("conferenceData"))
            .is_some_and(serde_json::Value::is_null);
        let mut event: gmail::calendar::Event =
            serde_json::from_value(raw).map_err(serde::de::Error::custom)?;
        if clear_conference {
            event.conference_data = Some(serde_json::Value::Null);
        }
        Ok(Self(event))
    }
}

/// Legacy `CalendarChange` JSON layout written before the provider-neutral calendar model.
#[derive(Deserialize)]
enum LegacyCalendarChange {
    Create {
        calendar: String,
        event: gmail::calendar::Event,
    },
    Edit {
        calendar: String,
        id: String,
        patch: LegacyGooglePatch,
        #[serde(default)]
        clear_attendees: bool,
        notify: bool,
        shift: Option<(SignedDuration, SignedDuration)>,
        shown: Vec<gmail::calendar::Event>,
    },
    Delete {
        calendar: String,
        id: String,
        notify: bool,
    },
    Truncate {
        calendar: String,
        series: String,
        from: Cut,
        notify: bool,
    },
    Split {
        calendar: String,
        series: String,
        from: Cut,
        patch: LegacyGooglePatch,
        #[serde(default)]
        clear_attendees: bool,
        start: gmail::calendar::EventDateTime,
        end: gmail::calendar::EventDateTime,
        notify: bool,
        shown: Vec<gmail::calendar::Event>,
    },
    Respond {
        calendar: String,
        event: gmail::calendar::Event,
        status: String,
        all: bool,
    },
    RespondToInvite {
        calendar: String,
        invite: gmail::calendar::Event,
        status: String,
        thread: Option<String>,
        links: Option<InviteLinks>,
    },
}

impl From<LegacyCalendarChange> for CalendarChange {
    fn from(legacy: LegacyCalendarChange) -> Self {
        match legacy {
            LegacyCalendarChange::Create { calendar, event } => Self::Create {
                calendar,
                event: google::event_from_google(event),
            },
            LegacyCalendarChange::Edit {
                calendar,
                id,
                patch,
                clear_attendees,
                notify,
                shift,
                shown,
            } => {
                let series_id = shown
                    .first()
                    .and_then(|ev| ev.recurring_event_id.clone())
                    .filter(|series| series != &id);
                Self::Edit {
                    calendar,
                    id,
                    series_id,
                    patch: google::patch_from_google(patch.0, clear_attendees),
                    notify,
                    shift,
                    shown: shown.into_iter().map(google::event_from_google).collect(),
                }
            }
            LegacyCalendarChange::Delete {
                calendar,
                id,
                notify,
            } => Self::Delete {
                calendar,
                id,
                series_id: None,
                notify,
                comment: None,
            },
            LegacyCalendarChange::Truncate {
                calendar,
                series,
                from,
                notify,
            } => Self::Truncate {
                calendar,
                series,
                from,
                notify,
            },
            LegacyCalendarChange::Split {
                calendar,
                series,
                from,
                patch,
                clear_attendees,
                start,
                end,
                notify,
                shown,
            } => {
                let fallback = || EventTime::AllDay(Date::constant(1970, 1, 1));
                Self::Split {
                    calendar,
                    series,
                    from,
                    patch: google::patch_from_google(patch.0, clear_attendees),
                    start: google::time_from_google(&start).unwrap_or_else(fallback),
                    end: google::time_from_google(&end).unwrap_or_else(fallback),
                    notify,
                    shown: shown.into_iter().map(google::event_from_google).collect(),
                }
            }
            LegacyCalendarChange::Respond {
                calendar,
                event,
                status,
                all,
            } => Self::Respond {
                calendar,
                event: google::event_from_google(event),
                status: RsvpStatus::from_google(&status).unwrap_or(RsvpStatus::NeedsAction),
                all,
            },
            LegacyCalendarChange::RespondToInvite {
                calendar,
                invite,
                status,
                thread,
                links,
            } => Self::RespondToInvite {
                calendar,
                account: None,
                invite: google::event_from_google(invite),
                status: RsvpStatus::from_google(&status).unwrap_or(RsvpStatus::NeedsAction),
                thread,
                links,
            },
        }
    }
}

impl<'de> Deserialize<'de> for Compat<CalendarChange> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            V2(Box<Envelope<CalendarChange>>),
            Legacy(Box<LegacyCalendarChange>),
        }
        match Stored::deserialize(deserializer)? {
            Stored::V2(envelope) => Ok(Self(envelope.data)),
            Stored::Legacy(change) => Ok(Self((*change).into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::calendar::model::{
        AccessRole, CalendarProviderKind, ConferenceKind, ConferencePatch, EventPatch,
        OnlineMeetingKind,
    };

    #[test]
    fn compat_deserializes_legacy_google_calendar_event_and_changes() {
        let legacy_cal = json!({
            "kind": "calendar#calendarListEntry",
            "id": "maya@brightmoor.co",
            "summary": "Maya",
            "primary": true,
            "accessRole": "owner",
            "backgroundColor": "#9fe1e7",
            "selected": true,
            "timeZone": "America/New_York"
        });
        let cal: Compat<Calendar> = serde_json::from_value(legacy_cal).expect("legacy calendar");
        assert_eq!(cal.0.id, "maya@brightmoor.co");
        assert!(cal.0.primary);
        assert_eq!(cal.0.access_role, AccessRole::Owner);
        assert_eq!(cal.0.provider, CalendarProviderKind::Google);
        let round = serde_json::to_value(&cal).expect("v2 serialize");
        assert_eq!(round["v"], 2);
        assert_eq!(
            serde_json::from_value::<Compat<Calendar>>(round).expect("v2 deserialize"),
            cal
        );

        let legacy_event = json!({
            "id": "ev1",
            "status": "confirmed",
            "summary": "Design crit",
            "start": { "dateTime": "2026-11-17T10:00:00-05:00", "timeZone": "America/New_York" },
            "end": { "dateTime": "2026-11-17T10:30:00-05:00", "timeZone": "America/New_York" },
            "recurringEventId": "series1",
            "hangoutLink": "https://meet.google.com/abc-defg-hij",
            "attendees": [{ "email": "maya@brightmoor.co", "self": true, "responseStatus": "accepted" }]
        });
        let ev: Compat<CalendarEvent> = serde_json::from_value(legacy_event).expect("legacy event");
        assert_eq!(ev.0.id.as_deref(), Some("ev1"));
        assert_eq!(ev.0.recurring_event_id.as_deref(), Some("series1"));
        assert_eq!(
            ev.0.conference.as_ref().map(|c| &c.kind),
            Some(&ConferenceKind::GoogleMeet)
        );
        assert_eq!(
            ev.0.my_rsvp("maya@brightmoor.co"),
            Some(RsvpStatus::Accepted)
        );

        let legacy_edit = json!({
            "Edit": {
                "calendar": "maya@brightmoor.co",
                "id": "ev1",
                "patch": { "summary": "Renamed", "conferenceData": null },
                "clear_attendees": true,
                "notify": false,
                "shift": null,
                "shown": []
            }
        });
        let edit: Compat<CalendarChange> =
            serde_json::from_value(legacy_edit).expect("legacy edit");
        assert_eq!(
            edit.0,
            CalendarChange::Edit {
                calendar: "maya@brightmoor.co".into(),
                id: "ev1".into(),
                series_id: None,
                patch: EventPatch {
                    summary: Some("Renamed".into()),
                    attendees: Some(Vec::new()),
                    conference: Some(ConferencePatch::Remove),
                    ..EventPatch::default()
                },
                notify: false,
                shift: None,
                shown: Vec::new(),
            }
        );

        let legacy_delete = json!({
            "Delete": {
                "calendar": "maya@brightmoor.co",
                "id": "ev1",
                "notify": true
            }
        });
        let del: Compat<CalendarChange> =
            serde_json::from_value(legacy_delete).expect("legacy delete");
        assert_eq!(
            del.0,
            CalendarChange::Delete {
                calendar: "maya@brightmoor.co".into(),
                id: "ev1".into(),
                series_id: None,
                notify: true,
                comment: None,
            }
        );

        let v2_patch = Compat(CalendarChange::Edit {
            calendar: "cal".into(),
            id: "ev2".into(),
            series_id: Some("master1".into()),
            patch: EventPatch {
                conference: Some(ConferencePatch::Create(OnlineMeetingKind::TeamsForBusiness)),
                ..EventPatch::default()
            },
            notify: true,
            shift: None,
            shown: Vec::new(),
        });
        let v2_json = serde_json::to_value(&v2_patch).expect("v2 change");
        assert_eq!(
            serde_json::from_value::<Compat<CalendarChange>>(v2_json).expect("v2 decode"),
            v2_patch
        );
    }
}
