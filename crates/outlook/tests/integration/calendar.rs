//! Calendars, events, calendar views, deltas, recurrence, online meetings, meeting actions and
//! free/busy scheduling against mock-servers' Graph.

use crate::support;

use jiff::Timestamp;
use jiff::civil::{date, datetime, time};
use mock_servers::DEV_ACCESS_TOKEN;
use outlook::calendar::{
    AcceptRequest, Attendee, AttendeeType, Calendar, CalendarColor, DayOfWeek, Event, EventPatch,
    FreeBusyStatus, OnlineMeetingProviderType, PatternedRecurrence, RecurrencePattern,
    RecurrencePatternType, RecurrenceRange, RecurrenceRangeType, RespondRequest, ResponseType,
    ScheduleInformation, ScheduleRequest, TimeSlot, WeekIndex,
};
use outlook::odata::RemovedReason;
use outlook::{
    BodyType, Client, DateTimeTimeZone, Endpoints, EventType, Importance, ItemBody, Location,
    MeetingMessageType, MeetingRequestType, Message, StaticToken, well_known,
};
use serde_json::Value;
use support::{EMAIL, Mock};

/// The week of the mock's clock (16–23 November 2026 in New York).
fn week() -> (Timestamp, Timestamp) {
    (
        "2026-11-16T05:00:00Z".parse().unwrap(),
        "2026-11-23T05:00:00Z".parse().unwrap(),
    )
}

fn subjects(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .map(|event| event.subject.as_deref().unwrap_or_default())
        .collect()
}

fn find<'a>(events: &'a [Event], subject: &str) -> &'a Event {
    events
        .iter()
        .find(|event| event.subject.as_deref() == Some(subject))
        .unwrap_or_else(|| panic!("no {subject:?} in {:?}", subjects(events)))
}

async fn calendar_id(client: &Client, name: &str) -> String {
    client
        .calendar()
        .calendars()
        .all()
        .await
        .unwrap()
        .value
        .into_iter()
        .find(|calendar| calendar.name.as_deref() == Some(name))
        .and_then(|calendar| calendar.id)
        .unwrap_or_else(|| panic!("no calendar named {name:?}"))
}

fn lunch_event() -> Event {
    Event {
        subject: Some("Lunch with Ines".into()),
        body: Some(ItemBody {
            content_type: Some(BodyType::Html),
            content: Some("Does noon work?".into()),
        }),
        start: Some(DateTimeTimeZone::new(
            datetime(2026, 11, 18, 12, 0, 0, 0),
            "Pacific Standard Time",
        )),
        end: Some(DateTimeTimeZone::new(
            datetime(2026, 11, 18, 13, 0, 0, 0),
            "Pacific Standard Time",
        )),
        location: Some(Location::new("Harry's Bar")),
        attendees: vec![Attendee::new(
            "ines@brightmoor.co",
            Some("Ines"),
            AttendeeType::Required,
        )],
        categories: vec!["Red category".into()],
        transaction_id: Some("7E163156-7762-4BEB-A1C6-729EA81755A7".into()),
        ..Event::default()
    }
}

async fn sent_with_subject(client: &Client, subject: &str) -> Vec<Message> {
    client
        .mail()
        .folder_messages(well_known::SENT_ITEMS)
        .filter(&format!("subject eq {}", outlook::odata::quote(subject)))
        .expand(Message::EXPAND_EVENT)
        .await
        .unwrap()
        .value
}

#[tokio::test]
async fn calendars_and_default_calendar() {
    let mock = Mock::start().await;
    let cal = mock.client.calendar();

    let calendars = cal.calendars().all().await.unwrap().value;
    let names: Vec<&str> = calendars
        .iter()
        .filter_map(|calendar| calendar.name.as_deref())
        .collect();
    assert_eq!(
        names,
        ["Calendar", "Brightmoor Team", "Holidays in United States"]
    );

    let primary = &calendars[0];
    assert_eq!(primary.is_default_calendar, Some(true));
    assert_eq!(primary.can_edit, Some(true));
    assert_eq!(primary.is_removable, Some(false));
    assert_eq!(primary.color, Some(CalendarColor::Auto));
    assert_eq!(primary.hex_color.as_deref(), Some("#9fe1e7"));
    assert_eq!(
        primary.owner.as_ref().and_then(|o| o.address.as_deref()),
        Some(EMAIL)
    );
    assert_eq!(
        primary.allowed_online_meeting_providers,
        [OnlineMeetingProviderType::TeamsForBusiness]
    );
    assert_eq!(
        primary.default_online_meeting_provider,
        Some(OnlineMeetingProviderType::TeamsForBusiness)
    );

    let default = cal.default_calendar().await.unwrap();
    assert_eq!(default.id, primary.id);

    let team_id = calendars[1].id.as_deref().unwrap();
    let selected = cal.calendar(team_id).select("name").await.unwrap();
    assert_eq!(selected.name.as_deref(), Some("Brightmoor Team"));
    assert_eq!(selected.can_edit, None);

    let read_only = cal
        .calendars()
        .filter("canEdit eq false")
        .await
        .unwrap()
        .value;
    assert_eq!(read_only.len(), 1);
    assert_eq!(
        read_only[0].name.as_deref(),
        Some("Holidays in United States")
    );

    let bad = cal.calendar("nope").await.unwrap_err();
    assert_eq!(bad.status(), Some(400));
    assert_eq!(bad.code(), Some("ErrorInvalidIdMalformed"));
}

#[tokio::test]
async fn events_views_timezones_and_instances() {
    let mock = Mock::start().await;
    let cal = mock.client.calendar();
    let (start, end) = week();

    // `events` lists single events and series masters (never expanded occurrences).
    let list = cal.events(None).top(100).all().await.unwrap().value;
    assert!(list.iter().all(|e| e.kind != Some(EventType::Occurrence)));
    let sync = find(&list, "Weekly product sync");
    assert_eq!(sync.kind, Some(EventType::SeriesMaster));
    let recurrence = sync.recurrence.as_ref().expect("series recurrence");
    let pattern = recurrence.pattern.as_ref().unwrap();
    assert_eq!(pattern.kind, Some(RecurrencePatternType::Weekly));
    assert_eq!(pattern.days_of_week, [DayOfWeek::Monday]);
    let range = recurrence.range.as_ref().unwrap();
    assert_eq!(range.kind, Some(RecurrenceRangeType::NoEnd));
    assert_eq!(range.effective_end_date(), None);

    let oof = find(&list, "Out of office");
    assert_eq!(oof.is_all_day, Some(true));
    assert_eq!(oof.show_as, Some(FreeBusyStatus::Oof));

    // `calendarView` expands occurrences and supports `Prefer: outlook.timezone` across pages.
    let pacific = cal
        .calendar_view(None, start, end)
        .top(3)
        .timezone("Pacific Standard Time")
        .all()
        .await
        .unwrap()
        .value;
    assert_eq!(pacific.len(), 8);
    assert!(pacific.iter().all(|event| {
        event
            .start
            .as_ref()
            .is_some_and(|s| s.time_zone == "Pacific Standard Time")
    }));
    let sync_occ = find(&pacific, "Weekly product sync");
    assert_eq!(sync_occ.kind, Some(EventType::Occurrence));
    assert_eq!(sync_occ.series_master_id, sync.id);
    assert_eq!(
        sync_occ.start.as_ref().map(|s| s.date_time),
        Some(datetime(2026, 11, 16, 7, 0, 0, 0))
    );
    assert_eq!(
        sync_occ.original_start,
        Some("2026-11-16T15:00:00Z".parse().unwrap())
    );
    assert_eq!(
        sync_occ.original_start_time_zone.as_deref(),
        Some("Eastern Standard Time")
    );

    // IANA zone names work in `Prefer: outlook.timezone` too.
    let ny = cal
        .calendar_view(None, start, end)
        .top(1)
        .timezone("America/New_York")
        .await
        .unwrap()
        .value;
    assert_eq!(
        ny[0].start.as_ref().map(|s| s.date_time),
        Some(datetime(2026, 11, 16, 10, 0, 0, 0))
    );

    // `instances` returns occurrences for one series master; `$expand=calendar` attaches it.
    let master_id = sync.id.as_deref().unwrap();
    let instances = cal
        .instances(None, master_id, start, end)
        .expand("calendar")
        .all()
        .await
        .unwrap()
        .value;
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].series_master_id.as_deref(), Some(master_id));
    assert_eq!(
        instances[0]
            .calendar
            .as_deref()
            .and_then(|c| c.is_default_calendar),
        Some(true)
    );

    // `events_by_ical_uid` finds the event by its iCalendar UID.
    let uid = sync.ical_uid.as_deref().unwrap();
    let by_uid = cal.events_by_ical_uid(None, uid).await.unwrap().value;
    assert_eq!(by_uid.len(), 1);
    assert_eq!(by_uid[0].id, sync.id);

    // Secondary calendar view & cross-calendar event scope check.
    let team_id = calendar_id(&mock.client, "Brightmoor Team").await;
    let shared = cal
        .calendar_view(Some(&team_id), start, end)
        .all()
        .await
        .unwrap()
        .value;
    assert_eq!(subjects(&shared), ["Company all-hands"]);
    let wrong_scope = cal.event(Some(&team_id), master_id).await.unwrap_err();
    assert!(wrong_scope.is_not_found());
}

#[tokio::test]
async fn create_update_clear_and_delete_events() {
    let mock = Mock::start().await;
    let cal = mock.client.calendar();

    let created = cal
        .create_event(None, &lunch_event())
        .timezone("Pacific Standard Time")
        .await
        .unwrap();
    assert_eq!(created.subject.as_deref(), Some("Lunch with Ines"));
    assert_eq!(
        created.start,
        Some(DateTimeTimeZone::new(
            datetime(2026, 11, 18, 12, 0, 0, 0),
            "Pacific Standard Time"
        ))
    );
    assert_eq!(created.categories, ["Red category"]);
    assert_eq!(created.attendees.len(), 1);
    let id = created.id.clone().unwrap();

    // Idempotent retry with the same transactionId returns the existing event.
    let repeated = cal.create_event(None, &lunch_event()).await.unwrap();
    assert_eq!(repeated.id.as_deref(), Some(id.as_str()));

    // EventPatch updates scalars and clears collections (`categories: []`, `attendees: []`).
    let updated = cal
        .update_event(
            None,
            &id,
            &EventPatch {
                subject: Some("Lunch with Ines and Tom".into()),
                show_as: Some(FreeBusyStatus::Oof),
                importance: Some(Importance::High),
                categories: Some(Vec::new()),
                attendees: Some(Vec::new()),
                ..EventPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.subject.as_deref(), Some("Lunch with Ines and Tom"));
    assert_eq!(updated.show_as, Some(FreeBusyStatus::Oof));
    assert_eq!(updated.importance, Some(Importance::High));
    assert!(updated.categories.is_empty());
    assert!(updated.attendees.is_empty());
    assert_ne!(updated.change_key, created.change_key);

    // Deleting removes the event.
    cal.delete_event(None, &id).await.unwrap();
    assert!(cal.event(None, &id).await.unwrap_err().is_not_found());

    // Writing to a read-only calendar fails with 403.
    let holidays = calendar_id(&mock.client, "Holidays in United States").await;
    let denied = cal
        .create_event(Some(&holidays), &lunch_event())
        .await
        .unwrap_err();
    assert_eq!(denied.status(), Some(403));
    assert_eq!(denied.code(), Some("ErrorAccessDenied"));

    // All-day event in a secondary calendar.
    let team = calendar_id(&mock.client, "Brightmoor Team").await;
    let all_day = cal
        .create_event(
            Some(&team),
            &Event {
                subject: Some("Hack day".into()),
                is_all_day: Some(true),
                start: Some(DateTimeTimeZone::new(
                    datetime(2026, 11, 21, 0, 0, 0, 0),
                    "Eastern Standard Time",
                )),
                end: Some(DateTimeTimeZone::new(
                    datetime(2026, 11, 22, 0, 0, 0, 0),
                    "Eastern Standard Time",
                )),
                ..Event::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(all_day.is_all_day, Some(true));
}

#[tokio::test]
async fn recurring_series_exceptions_cancellations_and_detach() {
    let mock = Mock::start().await;
    let cal = mock.client.calendar();

    let master = cal
        .create_event(
            None,
            &Event {
                subject: Some("Planning".into()),
                start: Some(DateTimeTimeZone::new(
                    datetime(2026, 11, 18, 10, 0, 0, 0),
                    "Eastern Standard Time",
                )),
                end: Some(DateTimeTimeZone::new(
                    datetime(2026, 11, 18, 10, 30, 0, 0),
                    "Eastern Standard Time",
                )),
                recurrence: Some(PatternedRecurrence::new(
                    RecurrencePattern {
                        kind: Some(RecurrencePatternType::Weekly),
                        interval: Some(1),
                        days_of_week: vec![DayOfWeek::Monday],
                        ..RecurrencePattern::default()
                    },
                    RecurrenceRange {
                        kind: Some(RecurrenceRangeType::EndDate),
                        start_date: Some(date(2026, 11, 18)),
                        end_date: Some(date(2026, 12, 31)),
                        ..RecurrenceRange::default()
                    },
                )),
                ..Event::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(master.kind, Some(EventType::SeriesMaster));
    let master_id = master.id.as_deref().unwrap();
    let span_start: Timestamp = "2026-11-16T00:00:00Z".parse().unwrap();
    let span_end: Timestamp = "2027-01-31T00:00:00Z".parse().unwrap();
    let instances = cal
        .instances(None, master_id, span_start, span_end)
        .all()
        .await
        .unwrap()
        .value;
    assert_eq!(instances.len(), 6);

    // Patching an occurrence promotes it to an exception.
    let second_id = instances[1].id.as_deref().unwrap();
    let exception = cal
        .update_event(
            None,
            second_id,
            &EventPatch {
                subject: Some("Planning (extended)".into()),
                ..EventPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(exception.kind, Some(EventType::Exception));
    assert_eq!(exception.series_master_id.as_deref(), Some(master_id));

    // Deleting an occurrence records it in `cancelledOccurrences`.
    let third_id = instances[2].id.as_deref().unwrap();
    cal.delete_event(None, third_id).await.unwrap();
    let with_cancelled = cal
        .event(None, master_id)
        .select("cancelledOccurrences")
        .await
        .unwrap();
    assert_eq!(
        with_cancelled.cancelled_occurrences,
        [format!("OID.{master_id}.2026-12-07")]
    );

    // Patching `recurrence: Some(None)` detaches a series back to a single instance.
    let detached = cal
        .update_event(
            None,
            master_id,
            &EventPatch {
                recurrence: Some(None),
                ..EventPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(detached.kind, Some(EventType::SingleInstance));
    assert_eq!(detached.recurrence, None);

    // Relative yearly series (4th Thursday of November).
    let yearly = cal
        .create_event(
            None,
            &Event {
                subject: Some("Thanksgiving dinner".into()),
                start: Some(DateTimeTimeZone::new(
                    datetime(2026, 11, 26, 17, 0, 0, 0),
                    "Eastern Standard Time",
                )),
                end: Some(DateTimeTimeZone::new(
                    datetime(2026, 11, 26, 20, 0, 0, 0),
                    "Eastern Standard Time",
                )),
                recurrence: Some(PatternedRecurrence::new(
                    RecurrencePattern {
                        kind: Some(RecurrencePatternType::RelativeYearly),
                        interval: Some(1),
                        month: Some(11),
                        days_of_week: vec![DayOfWeek::Thursday],
                        index: Some(WeekIndex::Fourth),
                        ..RecurrencePattern::default()
                    },
                    RecurrenceRange {
                        kind: Some(RecurrenceRangeType::Numbered),
                        start_date: Some(date(2026, 11, 26)),
                        number_of_occurrences: Some(3),
                        ..RecurrenceRange::default()
                    },
                )),
                ..Event::default()
            },
        )
        .await
        .unwrap();
    let days: Vec<jiff::civil::Date> = cal
        .instances(
            None,
            yearly.id.as_deref().unwrap(),
            "2026-01-01T00:00:00Z".parse().unwrap(),
            "2030-01-01T00:00:00Z".parse().unwrap(),
        )
        .timezone("Eastern Standard Time")
        .all()
        .await
        .unwrap()
        .value
        .into_iter()
        .map(|e| e.start.unwrap().date_time.date())
        .collect();
    assert_eq!(
        days,
        [date(2026, 11, 26), date(2027, 11, 25), date(2028, 11, 23)]
    );
}

#[tokio::test]
async fn online_meetings_and_personal_account_calendars() {
    let mock = Mock::start().await;
    let cal = mock.client.calendar();

    let mut teams = lunch_event();
    teams.transaction_id = None;
    teams.is_online_meeting = Some(true);
    teams.online_meeting_provider = Some(OnlineMeetingProviderType::TeamsForBusiness);
    let created = cal.create_event(None, &teams).await.unwrap();
    assert_eq!(created.is_online_meeting, Some(true));
    assert_eq!(
        created.online_meeting_provider,
        Some(OnlineMeetingProviderType::TeamsForBusiness)
    );
    let meeting = created.online_meeting.as_ref().expect("onlineMeeting");
    let join_url = meeting.join_url.as_deref().expect("joinUrl");
    assert!(join_url.contains("l/meetup-join/"), "{join_url}");
    assert!(meeting.conference_id.is_some());

    // Once online, attempting to turn it off via PATCH keeps the join link.
    let kept = cal
        .update_event(
            None,
            created.id.as_deref().unwrap(),
            &EventPatch {
                is_online_meeting: Some(false),
                ..EventPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        kept.online_meeting
            .as_ref()
            .and_then(|m| m.join_url.as_deref()),
        Some(join_url)
    );

    // Disallowed provider on a work calendar is refused.
    let mut skype = teams;
    skype.online_meeting_provider = Some(OnlineMeetingProviderType::SkypeForConsumer);
    let err = cal.create_event(None, &skype).await.unwrap_err();
    assert_eq!(err.status(), Some(400));

    // A personal account uses its own calendars and Skype for Consumer.
    let personal = Client::new(
        Endpoints::mock(&mock.server.url()),
        StaticToken(format!("{DEV_ACCESS_TOKEN}:maya.okafor@outlook.com")),
    );
    let personal_cals = personal.calendar().calendars().await.unwrap().value;
    assert_eq!(
        personal_cals[0]
            .owner
            .as_ref()
            .and_then(|o| o.address.as_deref()),
        Some("maya.okafor@outlook.com")
    );
    assert_eq!(
        personal_cals[0].allowed_online_meeting_providers,
        [OnlineMeetingProviderType::SkypeForConsumer]
    );
}

#[tokio::test]
async fn calendar_view_delta_pages_and_tracks_changes() {
    let mock = Mock::start().await;
    let cal = mock.client.calendar();
    let (start, end) = week();

    let first_page = cal
        .calendar_view_delta(None, start, end)
        .max_page_size(3)
        .timezone("America/New_York")
        .await
        .unwrap();
    assert_eq!(first_page.value.len(), 3);
    assert!(first_page.next_link.is_some() && first_page.delta_link.is_none());

    let round = cal
        .calendar_view_delta(None, start, end)
        .max_page_size(3)
        .timezone("America/New_York")
        .all()
        .await
        .unwrap();
    assert_eq!(round.value.len(), 8);
    assert!(round.value.iter().all(|event| {
        event.odata_type.as_deref() == Some("#microsoft.graph.event")
            && event
                .start
                .as_ref()
                .is_some_and(|s| s.time_zone == "America/New_York")
    }));
    let delta_link = round.delta_link.expect("delta link");

    // Quiet round.
    let quiet = mock
        .client
        .follow::<Event>(&delta_link)
        .all()
        .await
        .unwrap();
    assert!(quiet.value.is_empty());

    // Create, update, delete, and move out of window.
    let created = cal.create_event(None, &lunch_event()).await.unwrap();
    let aiko_id = find(&round.value, "Aiko: design exercise")
        .id
        .clone()
        .unwrap();
    cal.delete_event(None, &aiko_id).await.unwrap();
    let call_id = find(&round.value, "Calloway: Atlas feedback call")
        .id
        .clone()
        .unwrap();
    cal.update_event(
        None,
        &call_id,
        &EventPatch {
            start: Some(DateTimeTimeZone::new(
                datetime(2026, 12, 10, 14, 0, 0, 0),
                "Eastern Standard Time",
            )),
            end: Some(DateTimeTimeZone::new(
                datetime(2026, 12, 10, 15, 0, 0, 0),
                "Eastern Standard Time",
            )),
            ..EventPatch::default()
        },
    )
    .await
    .unwrap();
    let dinner_id = find(&round.value, "Dinner with Jess 🍕")
        .id
        .clone()
        .unwrap();
    cal.update_event(
        None,
        &dinner_id,
        &EventPatch {
            subject: Some("Dinner with Jess".into()),
            ..EventPatch::default()
        },
    )
    .await
    .unwrap();

    let changes = mock
        .client
        .follow::<Event>(&delta_link)
        .max_page_size(2)
        .all()
        .await
        .unwrap();
    assert!(changes.delta_link.is_some());
    let removed_ids: Vec<&str> = changes
        .value
        .iter()
        .filter(|e| e.removed.as_ref().map(|r| r.reason) == Some(RemovedReason::Deleted))
        .filter_map(|e| e.id.as_deref())
        .collect();
    assert!(removed_ids.contains(&aiko_id.as_str()));
    assert!(removed_ids.contains(&call_id.as_str()));
    assert!(
        changes
            .value
            .iter()
            .any(|e| e.id == created.id && e.removed.is_none())
    );
    assert!(
        changes
            .value
            .iter()
            .any(|e| e.id.as_deref() == Some(dinner_id.as_str())
                && e.subject.as_deref() == Some("Dinner with Jess"))
    );

    // Unsupported query parameter on delta returns 400.
    let bad_query = cal
        .calendar_view_delta(None, start, end)
        .select("subject")
        .await
        .unwrap_err();
    assert_eq!(bad_query.status(), Some(400));
    assert_eq!(bad_query.code(), Some("ErrorInvalidUrlQuery"));

    // Secondary calendar delta works too.
    let team_id = calendar_id(&mock.client, "Brightmoor Team").await;
    let shared = cal
        .calendar_view_delta(Some(&team_id), start, end)
        .all()
        .await
        .unwrap();
    assert_eq!(subjects(&shared.value), ["Company all-hands"]);
    assert!(shared.delta_link.is_some());
}

#[tokio::test]
async fn meeting_responses_proposals_cancellations_and_event_messages() {
    let mock = Mock::start().await;
    let cal = mock.client.calendar();
    let (start, end) = week();

    let view = cal
        .calendar_view(None, start, end)
        .all()
        .await
        .unwrap()
        .value;
    let invite_id = find(&view, "Calloway: Atlas feedback call")
        .id
        .clone()
        .unwrap();

    // Accept with comment and response email.
    cal.accept(
        None,
        &invite_id,
        &AcceptRequest {
            comment: Some("See you there".into()),
            send_response: Some(true),
        },
    )
    .await
    .unwrap();
    let accepted = cal.event(None, &invite_id).await.unwrap();
    assert_eq!(
        accepted.response_status.as_ref().and_then(|s| s.response),
        Some(ResponseType::Accepted)
    );
    assert_eq!(accepted.show_as, Some(FreeBusyStatus::Busy));
    let sent_accept =
        &sent_with_subject(&mock.client, "Accepted: Calloway: Atlas feedback call").await[0];
    assert_eq!(
        sent_accept.meeting_message_type,
        Some(MeetingMessageType::MeetingAccepted)
    );
    assert_eq!(sent_accept.response_type, Some(ResponseType::Accepted));

    // Tentatively accept with a proposed new time.
    cal.tentatively_accept(
        None,
        &invite_id,
        &RespondRequest {
            comment: Some("Can we do Friday?".into()),
            send_response: Some(true),
            proposed_new_time: Some(TimeSlot {
                start: DateTimeTimeZone::new(
                    datetime(2026, 11, 20, 14, 0, 0, 0),
                    "Eastern Standard Time",
                ),
                end: DateTimeTimeZone::new(
                    datetime(2026, 11, 20, 15, 0, 0, 0),
                    "Eastern Standard Time",
                ),
            }),
        },
    )
    .await
    .unwrap();
    let counter = &sent_with_subject(
        &mock.client,
        "New Time Proposed: Calloway: Atlas feedback call",
    )
    .await[0];
    assert_eq!(
        counter.response_type,
        Some(ResponseType::TentativelyAccepted)
    );
    assert_eq!(
        counter
            .proposed_new_time
            .as_ref()
            .map(|slot| slot.start.date_time),
        Some(datetime(2026, 11, 20, 19, 0, 0, 0))
    );

    // Decline silently (`sendResponse: false`).
    cal.decline(
        None,
        &invite_id,
        &RespondRequest {
            send_response: Some(false),
            ..RespondRequest::default()
        },
    )
    .await
    .unwrap();
    let declined = cal.event(None, &invite_id).await.unwrap();
    assert_eq!(
        declined.response_status.as_ref().and_then(|s| s.response),
        Some(ResponseType::Declined)
    );
    assert_eq!(declined.show_as, Some(FreeBusyStatus::Free));

    // Proposing a new time with `sendResponse: false` is rejected.
    let bad_proposal = cal
        .decline(
            None,
            &invite_id,
            &RespondRequest {
                send_response: Some(false),
                proposed_new_time: Some(TimeSlot {
                    start: DateTimeTimeZone::new(datetime(2026, 11, 20, 14, 0, 0, 0), "UTC"),
                    end: DateTimeTimeZone::new(datetime(2026, 11, 20, 15, 0, 0, 0), "UTC"),
                }),
                ..RespondRequest::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(bad_proposal.status(), Some(400));

    // Responding to one occurrence of a recurring invite promotes only that occurrence.
    let recurring_invite = find(&view, "Priya / Maya 1:1");
    let occ_id = recurring_invite.id.as_deref().unwrap();
    let master_id = recurring_invite.series_master_id.as_deref().unwrap();
    cal.tentatively_accept(
        None,
        occ_id,
        &RespondRequest {
            send_response: Some(false),
            ..RespondRequest::default()
        },
    )
    .await
    .unwrap();
    let occ_after = cal.event(None, occ_id).await.unwrap();
    assert_eq!(occ_after.kind, Some(EventType::Exception));
    assert_eq!(
        occ_after.response_status.as_ref().and_then(|s| s.response),
        Some(ResponseType::TentativelyAccepted)
    );
    let master_after = cal.event(None, master_id).await.unwrap();
    assert_eq!(
        master_after
            .response_status
            .as_ref()
            .and_then(|s| s.response),
        Some(ResponseType::Accepted)
    );

    // Organizer creates a meeting -> Sent Items carries `eventMessageRequest` with expanded `event`.
    let created = cal
        .create_event(
            None,
            &Event {
                subject: Some("Roadmap review".into()),
                body: Some(ItemBody {
                    content_type: Some(BodyType::Text),
                    content: Some("Bring Q1 numbers.".into()),
                }),
                start: Some(DateTimeTimeZone::new(
                    datetime(2026, 11, 24, 15, 0, 0, 0),
                    "Eastern Standard Time",
                )),
                end: Some(DateTimeTimeZone::new(
                    datetime(2026, 11, 24, 16, 0, 0, 0),
                    "Eastern Standard Time",
                )),
                attendees: vec![Attendee::new(
                    "daniel@brightmoor.co",
                    Some("Daniel Reyes"),
                    AttendeeType::Required,
                )],
                ..Event::default()
            },
        )
        .await
        .unwrap();
    let created_id = created.id.as_deref().unwrap();
    let invitation = &sent_with_subject(&mock.client, "Roadmap review").await[0];
    assert_eq!(
        invitation.meeting_request_type,
        Some(MeetingRequestType::NewMeetingRequest)
    );
    assert_eq!(
        invitation
            .event
            .as_deref()
            .and_then(|event| event.id.as_deref()),
        Some(created_id)
    );

    // Organizer cancels with a comment -> event removed and cancellation sent.
    cal.cancel(None, created_id, Some("Moving this to January."))
        .await
        .unwrap();
    assert!(
        cal.event(None, created_id)
            .await
            .unwrap_err()
            .is_not_found()
    );
    let cancelled = &sent_with_subject(&mock.client, "Canceled: Roadmap review").await[0];
    assert_eq!(
        cancelled.meeting_message_type,
        Some(MeetingMessageType::MeetingCancelled)
    );

    // Non-organizer cancel is refused.
    let refused = cal.cancel(None, &invite_id, None).await.unwrap_err();
    assert_eq!(refused.status(), Some(400));
    assert_eq!(refused.code(), Some("ErrorInvalidRequest"));
}

#[tokio::test]
async fn get_schedule_returns_free_busy_and_working_hours() {
    let mock = Mock::start().await;
    let request = ScheduleRequest {
        schedules: vec![
            EMAIL.into(),
            "priya@brightmoor.co".into(),
            "hudson@brightmoor.co".into(),
            "someone@example.com".into(),
        ],
        start_time: DateTimeTimeZone::new(
            datetime(2026, 11, 16, 8, 0, 0, 0),
            "Eastern Standard Time",
        ),
        end_time: DateTimeTimeZone::new(
            datetime(2026, 11, 16, 18, 0, 0, 0),
            "Eastern Standard Time",
        ),
        availability_view_interval: Some(60),
    };
    let schedules = mock
        .client
        .calendar()
        .get_schedule(&request)
        .timezone("Eastern Standard Time")
        .await
        .unwrap()
        .value;
    let views: Vec<&str> = schedules
        .iter()
        .filter_map(|s| s.availability_view.as_deref())
        .collect();
    assert_eq!(views, ["0020002000", "0020000000", "0020002000", ""]);

    let sync = schedules[0]
        .schedule_items
        .iter()
        .find(|item| item.subject.as_deref() == Some("Weekly product sync"))
        .expect("Weekly product sync in schedule");
    assert_eq!(sync.status, Some(FreeBusyStatus::Busy));
    assert_eq!(sync.location.as_deref(), Some("Hudson Room"));
    assert_eq!(
        sync.start,
        Some(DateTimeTimeZone::new(
            datetime(2026, 11, 16, 10, 0, 0, 0),
            "Eastern Standard Time"
        ))
    );

    let hours = schedules[0].working_hours.as_ref().expect("workingHours");
    assert_eq!(hours.start_time, Some(time(8, 0, 0, 0)));
    assert_eq!(hours.end_time, Some(time(17, 0, 0, 0)));
    assert_eq!(
        hours.time_zone.as_ref().and_then(|tz| tz.name.as_deref()),
        Some("Eastern Standard Time")
    );
    assert_eq!(
        schedules[3]
            .error
            .as_ref()
            .and_then(|err| err.response_code.as_deref()),
        Some("ErrorMailRecipientNotFound")
    );

    // Personal accounts do not support getSchedule (403).
    let personal = Client::new(
        Endpoints::mock(&mock.server.url()),
        StaticToken(format!("{DEV_ACCESS_TOKEN}:maya.okafor@outlook.com")),
    );
    let err = personal
        .calendar()
        .get_schedule(&request)
        .await
        .unwrap_err();
    assert_eq!(err.status(), Some(403));
    assert_eq!(err.code(), Some("ErrorAccessDenied"));
}

/// Properties of `raw` that `T` left unread, as `path.property`.
fn unread<T: serde::de::DeserializeOwned>(raw: &Value) -> Vec<String> {
    let mut ignored = Vec::new();
    let _: T = serde_ignored::deserialize(raw, |path| ignored.push(path.to_string())).unwrap();
    ignored.sort();
    ignored
}

#[tokio::test]
async fn calendar_models_read_every_property_graph_sends() {
    let mock = Mock::start().await;
    let http = reqwest::Client::new();
    let raw = |path: &'static str| {
        let url = mock.server.url().join(path).unwrap();
        let http = http.clone();
        async move {
            let response = http
                .get(url)
                .bearer_auth(DEV_ACCESS_TOKEN)
                .header("Prefer", "IdType=\"ImmutableId\"")
                .send()
                .await
                .unwrap();
            assert!(
                response.status().is_success(),
                "{path}: {}",
                response.status()
            );
            response.json::<Value>().await.unwrap()
        }
    };

    // Calendar entity: zero unread fields.
    let calendars = raw("/v1.0/me/calendars").await;
    assert!(unread::<Calendar>(&calendars["value"][0]).is_empty());

    // Series master (with recurrence, attendees, location, responseStatus) and occurrence.
    let events = raw("/v1.0/me/events?$top=100").await;
    let master = events["value"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "seriesMaster")
        .unwrap();
    assert_eq!(unread::<Event>(master), ["@odata.etag"]);

    let view = raw(
        "/v1.0/me/calendarView?startDateTime=2026-11-16T05:00:00Z&endDateTime=2026-11-23T05:00:00Z",
    )
    .await;
    for event in view["value"].as_array().unwrap() {
        assert_eq!(unread::<Event>(event), ["@odata.etag"]);
    }

    // Expanded eventMessage/event on an invite.
    let invite = raw("/v1.0/me/mailFolders/inbox/messages?$search=%22subject:Invitation%22&$top=1&$expand=microsoft.graph.eventMessage/event").await;
    assert_eq!(
        unread::<Message>(&invite["value"][0]),
        ["@odata.etag", "event.?.@odata.etag"]
    );

    // Teams online meeting event (covers `onlineMeeting` properties).
    let mut teams = lunch_event();
    teams.transaction_id = None;
    teams.is_online_meeting = Some(true);
    let created = mock
        .client
        .calendar()
        .create_event(None, &teams)
        .await
        .unwrap();
    let created_raw = http
        .get(
            mock.server
                .url()
                .join(&format!(
                    "/v1.0/me/events/{}",
                    created.id.as_deref().unwrap()
                ))
                .unwrap(),
        )
        .bearer_auth(DEV_ACCESS_TOKEN)
        .header("Prefer", "IdType=\"ImmutableId\"")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(
        unread::<Event>(&created_raw),
        ["@odata.context", "@odata.etag"]
    );

    // ScheduleInformation (both resolved and error items): zero unread fields.
    let schedule_response = http
        .post(
            mock.server
                .url()
                .join("/v1.0/me/calendar/getSchedule")
                .unwrap(),
        )
        .bearer_auth(DEV_ACCESS_TOKEN)
        .json(&serde_json::json!({
            "schedules": [EMAIL, "someone@example.com"],
            "startTime": { "dateTime": "2026-11-16T08:00:00", "timeZone": "Eastern Standard Time" },
            "endTime": { "dateTime": "2026-11-16T18:00:00", "timeZone": "Eastern Standard Time" },
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    for info in schedule_response["value"].as_array().unwrap() {
        assert!(unread::<ScheduleInformation>(info).is_empty());
    }
}
