//! Outlook Calendar through Microsoft Graph against mock-servers: `calendars`,
//! `calendarView/delta` incremental sync & `SyncStateNotFound` recovery, create/edit/delete/undo
//! with server-assigned ID swapping and `transactionId` idempotency, Teams online meetings,
//! recurring series & split/exceptions, RSVP (`accept`/`tentativelyAccept`/`decline`) + `cancel`,
//! `getSchedule` free/busy + personal-account 403 fallback, cross-provider multi-account without
//! key collision, and Outlook `showAs: oof` out-of-office detection.

use crate::support;

use std::time::Duration;

use exo_store::{
    Calendar, CalendarChange, CalendarEvent, CalendarOutcome, CalendarProviderKind, CalendarRemote,
    Conference, ConferenceKind, Cut, EventAttendee, EventPatch, EventTime, Frequency,
    OnlineMeetingKind, OutOfOfficeEventDetection, Recurrence, RecurrenceEnd, Refusal, Remote,
    RsvpStatus, ShowAs, Store, StoredEvent,
};
use jiff::civil::{Weekday, date};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use mock_servers::{Config, DEV_ACCESS_TOKEN, MockServer};
use outlook::DateTimeTimeZone;
use outlook::calendar::{Event as GraphEvent, ResponseType};
use serde_json::{Value, json};
use support::{TempDir, options};

/// Canonical key for the mock's work account as Microsoft.
const ACCOUNT: &str = "microsoft maya@brightmoor.co";
/// Display email of the mock's work account.
const EMAIL: &str = "maya@brightmoor.co";
/// Another Google account seeded on the mock.
const PERSONAL_GOOGLE: &str = "maya.lin@example.com";
/// A personal consumer Microsoft account on the mock.
const PERSONAL_MS_EMAIL: &str = "maya@outlook.com";
const PERSONAL_MS_KEY: &str = "microsoft maya@outlook.com";

fn new_york() -> TimeZone {
    TimeZone::get("America/New_York").expect("zone")
}

fn friday_at(hour: i8) -> EventTime {
    let at = date(2026, 11, 20)
        .at(hour, 0, 0, 0)
        .to_zoned(new_york())
        .expect("exists");
    EventTime::from_zoned(&at, false)
}

fn titles(events: &[StoredEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|stored| stored.event.summary.clone())
        .collect()
}

struct Outlook {
    server: MockServer,
    _dir: TempDir,
    store: Store,
    graph: outlook::Client,
}

impl Outlook {
    async fn start() -> Self {
        Self::start_with(Config::default()).await
    }

    async fn start_with(config: Config) -> Self {
        let server = MockServer::start(config).await.expect("mock starts");
        let dir = TempDir::new();
        let store = Store::open_with(
            dir.path(),
            ACCOUNT,
            Remote::outlook(
                outlook::Endpoints::mock(&server.url()),
                outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
            ),
            options(),
        )
        .await
        .expect("outlook store opens");
        let graph = outlook::Client::new(
            outlook::Endpoints::mock(&server.url()),
            outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
        );
        Self {
            server,
            _dir: dir,
            store,
            graph,
        }
    }

    async fn primary(&self) -> Calendar {
        self.store
            .sync_calendars(ACCOUNT)
            .await
            .expect("syncs calendars")
            .into_iter()
            .find(|cal| cal.primary)
            .expect("primary calendar")
    }

    async fn flush(&self) -> Vec<CalendarOutcome> {
        let mut outcomes = Vec::new();
        loop {
            let (due, _) = self
                .store
                .due_calendar_changes(false)
                .await
                .expect("reads due");
            if due.is_empty() {
                return outcomes;
            }
            let mut sent = false;
            for queued in due {
                let outcome = self
                    .store
                    .send_calendar_change(&queued)
                    .await
                    .expect("sends");
                sent |= outcome != CalendarOutcome::Postponed;
                outcomes.push(outcome);
            }
            if !sent {
                return outcomes;
            }
        }
    }

    async fn fault(&self, body: Value) {
        let url = self.server.url().join("/__mock/faults").expect("url");
        let response = reqwest::Client::new()
            .post(url)
            .json(&body)
            .send()
            .await
            .expect("arms fault");
        assert!(response.status().is_success());
    }

    async fn graph_events_titled(&self, calendar_id: &str, title: &str) -> Vec<GraphEvent> {
        let from: Timestamp = "2026-11-01T00:00:00Z".parse().expect("ts");
        let to: Timestamp = "2026-12-31T00:00:00Z".parse().expect("ts");
        self.graph
            .calendar()
            .calendar_view(Some(calendar_id), from, to)
            .timezone("UTC")
            .all()
            .await
            .expect("calendarView")
            .value
            .into_iter()
            .filter(|ev| ev.subject.as_deref() == Some(title) && ev.is_cancelled != Some(true))
            .collect()
    }
}

#[tokio::test]
async fn outlook_calendars_list_and_delta_sync_tracks_changes_and_recovers_expired_token() {
    let outlook = Outlook::start().await;
    let tz = new_york();

    let calendars = outlook
        .store
        .sync_calendars(ACCOUNT)
        .await
        .expect("lists calendars");
    let primary = calendars
        .iter()
        .find(|cal| cal.primary)
        .expect("primary calendar");
    assert_eq!(primary.summary, "Calendar");
    assert_eq!(primary.provider, CalendarProviderKind::Microsoft);
    assert!(primary.supports_teams(), "work account supports Teams");
    assert_ne!(primary.id, EMAIL, "Graph calendar IDs are opaque");

    let names: Vec<&str> = calendars.iter().map(|cal| cal.summary.as_str()).collect();
    assert_eq!(
        names,
        ["Calendar", "Brightmoor Team", "Holidays in United States"]
    );

    let stored_cals = outlook.store.calendars().await.expect("reads calendars");
    assert_eq!(stored_cals.len(), 3);
    assert!(stored_cals.iter().all(|ac| {
        ac.account == ACCOUNT && ac.email == EMAIL && ac.provider == CalendarProviderKind::Microsoft
    }));

    // Initial window sync populates recurring instances and single events.
    let week = date(2026, 11, 15)..date(2026, 11, 22);
    outlook
        .store
        .sync_events(&primary.id, week.clone(), &tz)
        .await
        .expect("initial sync");

    let tuesday = outlook
        .store
        .events(
            std::slice::from_ref(&primary.id),
            date(2026, 11, 17)..date(2026, 11, 18),
            &tz,
        )
        .await
        .expect("reads tuesday");
    assert_eq!(titles(&tuesday), ["Priya / Maya 1:1", "Design crit"]);
    assert!(
        tuesday
            .iter()
            .all(|stored| stored.event.recurring_event_id.is_some()),
        "recurring occurrences carry seriesMasterId"
    );

    let thursday = date(2026, 11, 19)..date(2026, 11, 20);
    let before = outlook
        .store
        .events(std::slice::from_ref(&primary.id), thursday.clone(), &tz)
        .await
        .expect("reads thursday");
    let dinner = before
        .iter()
        .find(|stored| stored.event.summary.as_deref() == Some("Dinner with Jess 🍕"))
        .expect("dinner")
        .event
        .clone();

    // Mutate behind the store's back: delete dinner and add a new Thursday lunch.
    outlook
        .graph
        .calendar()
        .delete_event(Some(&primary.id), dinner.id.as_deref().expect("id"))
        .await
        .expect("deletes dinner");
    let added = GraphEvent {
        subject: Some("Delta lunch".into()),
        start: Some(DateTimeTimeZone::new(
            date(2026, 11, 19).at(17, 0, 0, 0),
            "UTC",
        )),
        end: Some(DateTimeTimeZone::new(
            date(2026, 11, 19).at(18, 0, 0, 0),
            "UTC",
        )),
        ..GraphEvent::default()
    };
    outlook
        .graph
        .calendar()
        .create_event(Some(&primary.id), &added)
        .await
        .expect("creates lunch");

    // Second sync uses the stored calendarView/delta token: applies upsert + @removed.
    outlook
        .store
        .sync_events(&primary.id, week.clone(), &tz)
        .await
        .expect("delta sync");
    let after = outlook
        .store
        .events(std::slice::from_ref(&primary.id), thursday.clone(), &tz)
        .await
        .expect("reads thursday after delta");
    assert_eq!(
        titles(&after),
        ["Delta lunch", "Calloway: Atlas feedback call"]
    );

    // An expired delta token (410 SyncStateNotFound) transparently resets to a fresh delta pass.
    outlook
        .fault(json!({
            "method": "GET",
            "path": "/v1.0/me/calendars/",
            "status": 410,
            "reason": "SyncStateNotFound",
            "message": "The sync state generation is not found."
        }))
        .await;
    outlook
        .store
        .sync_events(&primary.id, week, &tz)
        .await
        .expect("recovers from SyncStateNotFound");
    let recovered = outlook
        .store
        .events(std::slice::from_ref(&primary.id), thursday, &tz)
        .await
        .expect("reads after recovery");
    assert_eq!(
        titles(&recovered),
        ["Delta lunch", "Calloway: Atlas feedback call"]
    );
}

#[tokio::test]
async fn outlook_create_swaps_server_id_rewrites_queued_edits_deduplicates_and_creates_teams() {
    let outlook = Outlook::start().await;
    let tz = new_york();
    let primary = outlook.primary().await;
    let friday = date(2026, 11, 20)..date(2026, 11, 21);

    // Queue Create + follow-up Edit while Create is still held, referencing the local ID.
    let local_id = "e0localoutlook1";
    let review = CalendarEvent {
        id: Some(local_id.into()),
        summary: Some("Architecture sync".into()),
        start: Some(friday_at(12)),
        end: Some(friday_at(13)),
        conference: Some(Conference::pending(OnlineMeetingKind::TeamsForBusiness)),
        ..CalendarEvent::default()
    };
    outlook
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Create {
                calendar: primary.id.clone(),
                event: review.clone(),
            },
            None,
        )
        .await
        .expect("queues create");

    let edited = CalendarEvent {
        summary: Some("Architecture sync (extended)".into()),
        end: Some(friday_at(14)),
        ..review.clone()
    };
    outlook
        .store
        .queue_calendar_change(
            2,
            CalendarChange::Edit {
                calendar: primary.id.clone(),
                id: local_id.into(),
                series_id: None,
                patch: EventPatch {
                    summary: Some("Architecture sync (extended)".into()),
                    end: Some(friday_at(14)),
                    ..EventPatch::default()
                },
                notify: false,
                shift: None,
                shown: vec![edited],
            },
            None,
        )
        .await
        .expect("queues edit against local ID");

    // Optimistic cache already reflects the queued edit.
    let optimistic = outlook
        .store
        .events(std::slice::from_ref(&primary.id), friday.clone(), &tz)
        .await
        .expect("reads optimistic");
    assert_eq!(titles(&optimistic), ["Architecture sync (extended)"]);
    assert!(optimistic[0].unsynced);

    // Lose the first Create response (`500 after: true`): retry with the same transactionId must
    // deduplicate rather than creating a duplicate event on Graph.
    outlook
        .fault(json!({
            "method": "POST",
            "path": "/v1.0/me/calendars/",
            "status": 500,
            "after": true
        }))
        .await;

    assert_eq!(
        outlook.flush().await,
        [CalendarOutcome::Sent, CalendarOutcome::Sent],
        "Create commits with server ID, rewrites Edit to server ID, and Edit succeeds"
    );

    let synced = outlook
        .store
        .events(std::slice::from_ref(&primary.id), friday, &tz)
        .await
        .expect("reads synced");
    assert_eq!(synced.len(), 1);
    assert!(!synced[0].unsynced);
    let server_id = synced[0].event.id.clone().expect("server ID");
    assert_ne!(server_id, local_id, "local ID was swapped to Graph's ID");
    assert_eq!(
        synced[0].event.summary.as_deref(),
        Some("Architecture sync (extended)")
    );
    let conference = synced[0]
        .event
        .conference
        .as_ref()
        .expect("Teams meeting created");
    assert_eq!(conference.kind, ConferenceKind::Teams);
    assert!(
        conference
            .join_url
            .as_deref()
            .is_some_and(|url| !url.is_empty()),
        "Teams join URL populated"
    );

    // Verify exactly one event exists on Graph (transactionId deduplicated the 503 retry).
    let on_graph = outlook
        .graph_events_titled(&primary.id, "Architecture sync (extended)")
        .await;
    assert_eq!(on_graph.len(), 1);
    assert_eq!(on_graph[0].id.as_deref(), Some(server_id.as_str()));
}

#[tokio::test]
async fn outlook_undo_or_delete_of_in_flight_create_waits_for_server_id_and_deletes() {
    let config = Config {
        latency: Duration::from_millis(80),
        ..Config::default()
    };
    let outlook = Outlook::start_with(config).await;
    let tz = new_york();
    let primary = outlook.primary().await;
    let friday = date(2026, 11, 20)..date(2026, 11, 21);

    // 1. Undoing an in-flight Outlook Create waits for the server-assigned ID and queues Delete.
    let first = CalendarEvent {
        id: Some("e0inflightms01".into()),
        summary: Some("Phantom Teams A".into()),
        start: Some(friday_at(9)),
        end: Some(friday_at(10)),
        ..CalendarEvent::default()
    };
    outlook
        .store
        .queue_calendar_change(
            10,
            CalendarChange::Create {
                calendar: primary.id.clone(),
                event: first,
            },
            None,
        )
        .await
        .expect("queues");
    let (due, _) = outlook
        .store
        .due_calendar_changes(true)
        .await
        .expect("reads");
    let sending = {
        let store = outlook.store.clone();
        let item = due[0].clone();
        tokio::spawn(async move { store.send_calendar_change(&item).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        outlook
            .store
            .unqueue_calendar_change(10)
            .await
            .expect("undoes in-flight create")
    );
    assert_eq!(
        sending.await.expect("joins").expect("sends"),
        CalendarOutcome::Sent
    );
    assert!(
        outlook
            .store
            .events(std::slice::from_ref(&primary.id), friday.clone(), &tz)
            .await
            .expect("reads")
            .is_empty(),
        "compensating delete removes created event immediately"
    );
    assert_eq!(outlook.flush().await, [CalendarOutcome::Sent]);
    assert!(
        outlook
            .graph_events_titled(&primary.id, "Phantom Teams A")
            .await
            .is_empty(),
        "deleted on Graph using the server-assigned ID"
    );

    // 2. Queueing Delete against the local ID while Create is in flight also waits and deletes.
    let second = CalendarEvent {
        id: Some("e0inflightms02".into()),
        summary: Some("Phantom Teams B".into()),
        start: Some(friday_at(11)),
        end: Some(friday_at(12)),
        ..CalendarEvent::default()
    };
    outlook
        .store
        .queue_calendar_change(
            11,
            CalendarChange::Create {
                calendar: primary.id.clone(),
                event: second,
            },
            None,
        )
        .await
        .expect("queues");
    let (due, _) = outlook
        .store
        .due_calendar_changes(true)
        .await
        .expect("reads");
    let sending = {
        let store = outlook.store.clone();
        let item = due[0].clone();
        tokio::spawn(async move { store.send_calendar_change(&item).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    outlook
        .store
        .queue_calendar_change(
            12,
            CalendarChange::Delete {
                calendar: primary.id.clone(),
                id: "e0inflightms02".into(),
                series_id: None,
                notify: false,
                comment: None,
            },
            None,
        )
        .await
        .expect("queues delete while create is in flight");
    assert_eq!(
        sending.await.expect("joins").expect("sends"),
        CalendarOutcome::Sent
    );
    assert!(
        outlook
            .store
            .events(std::slice::from_ref(&primary.id), friday, &tz)
            .await
            .expect("reads")
            .is_empty()
    );
    assert_eq!(outlook.flush().await, [CalendarOutcome::Sent]);
    assert!(
        outlook
            .graph_events_titled(&primary.id, "Phantom Teams B")
            .await
            .is_empty(),
        "no phantom event remains on Graph"
    );
}

#[tokio::test]
async fn outlook_recurring_series_edit_exception_split_and_delete() {
    let outlook = Outlook::start().await;
    let tz = new_york();
    let primary = outlook.primary().await;

    let rule = Recurrence {
        frequency: Frequency::Weekly(vec![Weekday::Tuesday]),
        interval: 1,
        end: RecurrenceEnd::After(5),
    };
    let series = CalendarEvent {
        id: Some("e0msseries001".into()),
        summary: Some("Platform standup".into()),
        start: Some(EventTime::Timed {
            at: "2026-11-10T10:00:00-05:00".parse().expect("ts"),
            time_zone: Some("America/New_York".into()),
        }),
        end: Some(EventTime::Timed {
            at: "2026-11-10T10:30:00-05:00".parse().expect("ts"),
            time_zone: Some("America/New_York".into()),
        }),
        recurrence: Some(rule.clone()),
        ..CalendarEvent::default()
    };
    outlook
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Create {
                calendar: primary.id.clone(),
                event: series,
            },
            None,
        )
        .await
        .expect("queues recurring create");
    assert_eq!(outlook.flush().await, [CalendarOutcome::Sent]);

    // Sync 5 weeks and locate the server-assigned series master ID + occurrences.
    let window = date(2026, 11, 8)..date(2026, 12, 15);
    outlook
        .store
        .sync_events(&primary.id, window.clone(), &tz)
        .await
        .expect("syncs window");
    let occurrences: Vec<CalendarEvent> = outlook
        .store
        .events(std::slice::from_ref(&primary.id), window.clone(), &tz)
        .await
        .expect("reads")
        .into_iter()
        .map(|s| s.event)
        .filter(|ev| ev.summary.as_deref() == Some("Platform standup"))
        .collect();
    assert_eq!(occurrences.len(), 5, "all 5 weekly instances expanded");
    let master_id = occurrences[0]
        .recurring_event_id
        .clone()
        .expect("seriesMasterId");

    // series_recurrence reads back the PatternedRecurrence cleanly.
    assert_eq!(
        outlook
            .store
            .series_recurrence(&primary.id, &master_id)
            .await
            .expect("reads recurrence"),
        Some(rule)
    );

    // Edit a single occurrence (the 2nd week, Nov 17) as an exception.
    let second = occurrences[1].clone();
    let second_id = second.id.clone().expect("occurrence id");
    let renamed_second = CalendarEvent {
        summary: Some("Platform standup (demo)".into()),
        ..second
    };
    outlook
        .store
        .queue_calendar_change(
            2,
            CalendarChange::Edit {
                calendar: primary.id.clone(),
                id: second_id.clone(),
                series_id: Some(master_id.clone()),
                patch: EventPatch {
                    summary: Some("Platform standup (demo)".into()),
                    ..EventPatch::default()
                },
                notify: false,
                shift: None,
                shown: vec![renamed_second],
            },
            None,
        )
        .await
        .expect("queues exception edit");
    assert_eq!(outlook.flush().await, [CalendarOutcome::Sent]);
    let graph_second = outlook
        .graph
        .calendar()
        .event(Some(&primary.id), &second_id)
        .await
        .expect("reads exception");
    assert_eq!(
        graph_second.subject.as_deref(),
        Some("Platform standup (demo)")
    );

    // Split ("this and following") starting at the 3rd occurrence (Nov 24), simulating a 500
    // after the old master PATCH commits on Graph so all 3 attempts fail and retry safety is
    // exercised (new series created first with deterministic transactionId, no duplicate series).
    for _ in 0..3 {
        outlook
            .fault(json!({
                "method": "PATCH",
                "path": "/v1.0/me/calendars/",
                "status": 500,
                "after": true
            }))
            .await;
    }
    // Provide optimistic `shown` rows with client-generated IDs on Split to verify Outlook does
    // NOT leave fake-ID ghost rows behind after flush + delta sync.
    let fake_shown = vec![
        CalendarEvent {
            id: Some("e0splitremms01_20261124T160000Z".into()),
            summary: Some("Platform standup (v2)".into()),
            start: Some(EventTime::Timed {
                at: "2026-11-24T11:00:00-05:00".parse().expect("ts"),
                time_zone: Some("America/New_York".into()),
            }),
            end: Some(EventTime::Timed {
                at: "2026-11-24T11:30:00-05:00".parse().expect("ts"),
                time_zone: Some("America/New_York".into()),
            }),
            recurring_event_id: Some("e0splitremms01".into()),
            ..CalendarEvent::default()
        },
        CalendarEvent {
            id: Some("e0splitremms01_20261201T160000Z".into()),
            summary: Some("Platform standup (v2)".into()),
            start: Some(EventTime::Timed {
                at: "2026-12-01T11:00:00-05:00".parse().expect("ts"),
                time_zone: Some("America/New_York".into()),
            }),
            end: Some(EventTime::Timed {
                at: "2026-12-01T11:30:00-05:00".parse().expect("ts"),
                time_zone: Some("America/New_York".into()),
            }),
            recurring_event_id: Some("e0splitremms01".into()),
            ..CalendarEvent::default()
        },
    ];
    outlook
        .store
        .queue_calendar_change(
            3,
            CalendarChange::Split {
                calendar: primary.id.clone(),
                series: master_id.clone(),
                from: Cut::At("2026-11-24T15:00:00Z".parse().expect("cut")),
                patch: EventPatch {
                    id: Some("e0splitremms01".into()),
                    summary: Some("Platform standup (v2)".into()),
                    ..EventPatch::default()
                },
                start: EventTime::Timed {
                    at: "2026-11-24T11:00:00-05:00".parse().expect("ts"),
                    time_zone: Some("America/New_York".into()),
                },
                end: EventTime::Timed {
                    at: "2026-11-24T11:30:00-05:00".parse().expect("ts"),
                    time_zone: Some("America/New_York".into()),
                },
                notify: false,
                shown: fake_shown,
            },
            None,
        )
        .await
        .expect("queues split");

    let (due, _) = outlook
        .store
        .due_calendar_changes(true)
        .await
        .expect("reads");
    assert_eq!(
        outlook
            .store
            .send_calendar_change(&due[0])
            .await
            .expect("first split attempt"),
        CalendarOutcome::Postponed
    );
    let (due, _) = outlook
        .store
        .due_calendar_changes(true)
        .await
        .expect("reads retry");
    assert_eq!(
        outlook
            .store
            .send_calendar_change(&due[0])
            .await
            .expect("split retry"),
        CalendarOutcome::Sent
    );

    // Old series now only runs through Nov 17 (2 instances), and the new series has 3 instances
    // (Nov 24, Dec 1, Dec 8) with no duplicate series created on retry!
    let v2_events = outlook
        .graph_events_titled(&primary.id, "Platform standup (v2)")
        .await;
    assert_eq!(
        v2_events.len(),
        3,
        "split created 3 remaining instances with no duplicates"
    );
    let v2_master = v2_events[0]
        .series_master_id
        .clone()
        .expect("v2 seriesMasterId");

    // Syncing after Outlook Split yields ONLY the 3 real server-ID instances (no fake-ID ghosts).
    outlook
        .store
        .sync_events(&primary.id, window.clone(), &tz)
        .await
        .expect("syncs after split");
    let cached_v2: Vec<StoredEvent> = outlook
        .store
        .events(std::slice::from_ref(&primary.id), window.clone(), &tz)
        .await
        .expect("reads")
        .into_iter()
        .filter(|s| s.event.summary.as_deref() == Some("Platform standup (v2)"))
        .collect();
    assert_eq!(
        cached_v2.len(),
        3,
        "no fake-ID ghost rows remain in SQLite alongside real Graph instances: {:?}",
        cached_v2
            .iter()
            .map(|s| s.event.id.as_deref())
            .collect::<Vec<_>>()
    );
    assert!(
        cached_v2.iter().all(|s| !s
            .event
            .id
            .as_deref()
            .unwrap_or_default()
            .starts_with("e0splitremms01")),
        "cached instances carry Graph server IDs"
    );

    // Editing ONLY the recurrence rule without patch.start updates the Outlook series master.
    let updated_rule = Recurrence {
        frequency: Frequency::Weekly(vec![Weekday::Tuesday]),
        interval: 2,
        end: RecurrenceEnd::After(4),
    };
    outlook
        .store
        .queue_calendar_change(
            30,
            CalendarChange::Edit {
                calendar: primary.id.clone(),
                id: v2_master.clone(),
                series_id: None,
                patch: EventPatch {
                    recurrence: Some(Some(updated_rule.clone())),
                    ..EventPatch::default()
                },
                notify: false,
                shift: None,
                shown: Vec::new(),
            },
            None,
        )
        .await
        .expect("queues recurrence-only patch");
    assert_eq!(outlook.flush().await, [CalendarOutcome::Sent]);
    assert_eq!(
        outlook
            .store
            .series_recurrence(&primary.id, &v2_master)
            .await
            .expect("reads updated recurrence"),
        Some(updated_rule),
        "recurrence-only patch without patch.start updated Outlook series"
    );

    // Deleting the entire v2 series removes all cached instances via indexed series_id and Graph.
    outlook
        .store
        .queue_calendar_change(
            4,
            CalendarChange::Delete {
                calendar: primary.id.clone(),
                id: v2_master,
                series_id: None,
                notify: false,
                comment: None,
            },
            None,
        )
        .await
        .expect("queues series delete");
    assert!(
        outlook
            .store
            .events(std::slice::from_ref(&primary.id), window, &tz)
            .await
            .expect("reads")
            .iter()
            .all(|s| s.event.summary.as_deref() != Some("Platform standup (v2)")),
        "all cached instances of deleted series dropped optimistically"
    );
    assert_eq!(outlook.flush().await, [CalendarOutcome::Sent]);
    assert!(
        outlook
            .graph_events_titled(&primary.id, "Platform standup (v2)")
            .await
            .is_empty(),
        "v2 series deleted on Graph"
    );
}

#[tokio::test]
async fn outlook_rsvp_cancel_with_comment_and_respond_to_invite() {
    let outlook = Outlook::start().await;
    let tz = new_york();
    let primary = outlook.primary().await;
    let thursday = date(2026, 11, 19)..date(2026, 11, 20);

    outlook
        .store
        .sync_events(&primary.id, thursday.clone(), &tz)
        .await
        .expect("syncs thursday");
    let read_call = async || {
        outlook
            .store
            .events(std::slice::from_ref(&primary.id), thursday.clone(), &tz)
            .await
            .expect("reads")
            .into_iter()
            .find(|s| s.event.summary.as_deref() == Some("Calloway: Atlas feedback call"))
            .expect("feedback call")
            .event
    };
    let call = read_call().await;
    assert_eq!(call.my_rsvp(EMAIL), Some(RsvpStatus::NeedsAction));

    // Accept via Graph POST .../accept.
    outlook
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Respond {
                calendar: primary.id.clone(),
                event: call.clone(),
                status: RsvpStatus::Accepted,
                all: false,
            },
            None,
        )
        .await
        .expect("queues accept");
    assert_eq!(read_call().await.my_rsvp(EMAIL), Some(RsvpStatus::Accepted));
    assert_eq!(outlook.flush().await, [CalendarOutcome::Sent]);

    let call_id = call.id.as_deref().expect("id");
    let graph_call = outlook
        .graph
        .calendar()
        .event(Some(&primary.id), call_id)
        .await
        .expect("gets event");
    assert_eq!(
        graph_call.response_status.and_then(|r| r.response),
        Some(ResponseType::Accepted)
    );

    // Switch to Tentative via POST .../tentativelyAccept.
    outlook
        .store
        .queue_calendar_change(
            2,
            CalendarChange::Respond {
                calendar: primary.id.clone(),
                event: read_call().await,
                status: RsvpStatus::Tentative,
                all: false,
            },
            None,
        )
        .await
        .expect("queues tentative");
    assert_eq!(outlook.flush().await, [CalendarOutcome::Sent]);
    let graph_call = outlook
        .graph
        .calendar()
        .event(Some(&primary.id), call_id)
        .await
        .expect("gets event");
    assert_eq!(
        graph_call.response_status.and_then(|r| r.response),
        Some(ResponseType::TentativelyAccepted)
    );

    // Non-organizer attempting cancel with a comment is refused as Refusal::NonOrganizer.
    outlook
        .store
        .queue_calendar_change(
            3,
            CalendarChange::Delete {
                calendar: primary.id.clone(),
                id: call_id.to_owned(),
                series_id: None,
                notify: true,
                comment: Some("Trying to cancel someone else's meeting".into()),
            },
            None,
        )
        .await
        .expect("queues non-organizer cancel");
    let outcomes = outlook.flush().await;
    assert!(
        matches!(
            &outcomes[..],
            [CalendarOutcome::Refused {
                refusal: Refusal::NonOrganizer,
                ..
            }]
        ),
        "non-organizer cancel refused: {outcomes:?}"
    );

    // RespondToInvite by iCalUId accepts the existing event via Graph filter on iCalUId.
    let call_uid = call.ical_uid.clone().expect("ical_uid");
    outlook
        .store
        .queue_calendar_change(
            4,
            CalendarChange::RespondToInvite {
                calendar: primary.id.clone(),
                account: Some(ACCOUNT.into()),
                invite: CalendarEvent {
                    ical_uid: Some(call_uid),
                    summary: call.summary.clone(),
                    start: call.start.clone(),
                    end: call.end.clone(),
                    ..CalendarEvent::default()
                },
                status: RsvpStatus::Accepted,
                thread: None,
                links: None,
            },
            None,
        )
        .await
        .expect("queues invite accept");
    assert_eq!(outlook.flush().await, [CalendarOutcome::Sent]);

    // RespondToInvite for a missing/cancelled event is refused as Gone.
    outlook
        .store
        .queue_calendar_change(
            5,
            CalendarChange::RespondToInvite {
                calendar: primary.id.clone(),
                account: Some(ACCOUNT.into()),
                invite: CalendarEvent {
                    ical_uid: Some("missing-cancelled-uid@example.com".into()),
                    summary: Some("Cancelled invite".into()),
                    start: Some(friday_at(10)),
                    end: Some(friday_at(11)),
                    ..CalendarEvent::default()
                },
                status: RsvpStatus::Accepted,
                thread: None,
                links: None,
            },
            None,
        )
        .await
        .expect("queues missing invite accept");
    let outcomes = outlook.flush().await;
    assert!(
        matches!(
            &outcomes[..],
            [CalendarOutcome::Refused {
                refusal: Refusal::Gone,
                ..
            }]
        ),
        "missing invite refused as Gone: {outcomes:?}"
    );

    // Organizer cancel with comment invokes Graph POST .../cancel and sends cancellation notice.
    let hosted = CalendarEvent {
        id: Some("e0hostedcancel1".into()),
        summary: Some("Client kickoff".into()),
        start: Some(friday_at(15)),
        end: Some(friday_at(16)),
        attendees: vec![EventAttendee {
            email: Some("priya@brightmoor.co".into()),
            ..EventAttendee::default()
        }],
        ..CalendarEvent::default()
    };
    outlook
        .store
        .queue_calendar_change(
            6,
            CalendarChange::Create {
                calendar: primary.id.clone(),
                event: hosted,
            },
            None,
        )
        .await
        .expect("queues hosted event");
    assert_eq!(outlook.flush().await, [CalendarOutcome::Sent]);
    let created = outlook
        .graph_events_titled(&primary.id, "Client kickoff")
        .await;
    assert_eq!(created.len(), 1);
    let hosted_id = created[0].id.clone().expect("server id");

    outlook
        .store
        .queue_calendar_change(
            7,
            CalendarChange::Delete {
                calendar: primary.id.clone(),
                id: hosted_id,
                series_id: None,
                notify: true,
                comment: Some("Moving to next Tuesday".into()),
            },
            None,
        )
        .await
        .expect("queues organizer cancel");
    assert_eq!(outlook.flush().await, [CalendarOutcome::Sent]);
    assert!(
        outlook
            .graph_events_titled(&primary.id, "Client kickoff")
            .await
            .is_empty(),
        "cancelled event removed from Graph"
    );
    let sent = outlook
        .graph
        .mail()
        .folder_messages(outlook::well_known::SENT_ITEMS)
        .all()
        .await
        .expect("sent items");
    assert!(
        sent.value
            .iter()
            .any(|msg| msg.subject.as_deref() == Some("Canceled: Client kickoff")),
        "cancellation notice sent to attendees"
    );
}

#[tokio::test]
async fn outlook_free_busy_get_schedule_and_personal_account_fallback() {
    let outlook = Outlook::start().await;
    let primary = outlook.primary().await;
    let from: Timestamp = "2026-11-16T05:00:00Z".parse().expect("ts");
    let to: Timestamp = "2026-11-21T05:00:00Z".parse().expect("ts");

    // Work account: own opaque calendar ID + colleague + room + unknown recipient.
    let queried = vec![
        primary.id.clone(),
        "priya@brightmoor.co".to_owned(),
        "hudson@brightmoor.co".to_owned(),
        "nobody@outside.example".to_owned(),
    ];
    let busy = outlook
        .store
        .free_busy(&queried, from, to)
        .await
        .expect("getSchedule");
    assert_eq!(busy.len(), 4);
    assert!(
        busy[0].1.as_ref().is_some_and(|slots| !slots.is_empty()),
        "own opaque calendar ID resolved to work email for getSchedule"
    );
    assert!(
        busy[1].1.as_ref().is_some_and(|slots| !slots.is_empty()),
        "colleague schedule returned"
    );
    assert!(busy[2].1.is_some(), "room schedule returned");
    assert_eq!(busy[3].1, None, "unknown external recipient returns None");

    let priya_events = outlook
        .store
        .attendee_events("priya@brightmoor.co", from, to)
        .await
        .expect("attendee_events")
        .expect("colleague visible");
    assert!(!priya_events.is_empty());
    assert_eq!(
        outlook
            .store
            .attendee_events("nobody@outside.example", from, to)
            .await
            .expect("unknown attendee"),
        None
    );

    // Personal Outlook.com account: Graph returns 403 ErrorAccessDenied on getSchedule; store
    // falls back to calendarView for own calendars and None for guests without failing.
    let personal_dir = TempDir::new();
    let personal_token = format!("{DEV_ACCESS_TOKEN}:{PERSONAL_MS_EMAIL}");
    let personal = Store::open_with(
        personal_dir.path(),
        PERSONAL_MS_KEY,
        Remote::outlook(
            outlook::Endpoints::mock(&outlook.server.url()),
            outlook::StaticToken(personal_token),
        ),
        options(),
    )
    .await
    .expect("personal outlook store opens");
    let personal_cals = personal
        .sync_calendars(PERSONAL_MS_KEY)
        .await
        .expect("syncs personal calendars");
    let personal_primary = personal_cals
        .iter()
        .find(|cal| cal.primary)
        .expect("personal primary");
    assert!(!personal_primary.supports_teams());
    assert!(
        personal_primary
            .allowed_online_meetings
            .contains(&OnlineMeetingKind::SkypeForConsumer)
    );

    let fallback = personal
        .free_busy(
            &[
                personal_primary.id.clone(),
                "priya@brightmoor.co".to_owned(),
            ],
            from,
            to,
        )
        .await
        .expect("personal account free_busy falls back cleanly on 403");
    assert!(fallback[0].1.is_some(), "own calendar uses calendarView");
    assert_eq!(
        fallback[1].1, None,
        "guests return None on personal account"
    );
}

#[tokio::test]
async fn cross_provider_accounts_coexist_without_key_collision() {
    let server = MockServer::start(Config::default()).await.expect("mock");
    let tz = new_york();

    // 1. Primary Gmail store adds Microsoft calendar for the SAME email address.
    let gmail_dir = TempDir::new();
    let gmail_store = Store::open_with(
        gmail_dir.path(),
        EMAIL,
        Remote::gmail(
            server.endpoints(),
            gmail::StaticToken(DEV_ACCESS_TOKEN.into()),
        ),
        options(),
    )
    .await
    .expect("gmail store");
    gmail_store
        .sync_calendars(EMAIL)
        .await
        .expect("syncs google calendars");
    gmail_store.add_calendar_account(
        ACCOUNT,
        CalendarRemote::outlook(
            EMAIL,
            outlook::Endpoints::mock(&server.url()),
            outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
        ),
    );
    gmail_store
        .sync_calendars(ACCOUNT)
        .await
        .expect("syncs microsoft calendars");

    let all = gmail_store.calendars().await.expect("reads all calendars");
    assert!(
        all.iter()
            .any(|ac| ac.account == EMAIL && ac.provider == CalendarProviderKind::Google),
        "Google calendars present under email key"
    );
    assert!(
        all.iter()
            .any(|ac| ac.account == ACCOUNT && ac.provider == CalendarProviderKind::Microsoft),
        "Microsoft calendars present under 'microsoft <email>' key with same display email"
    );
    assert_eq!(
        all.iter()
            .filter(|ac| ac.email == EMAIL && ac.calendar.primary)
            .count(),
        2,
        "both primary calendars coexist without colliding"
    );

    // 2. Primary Outlook store adds secondary Google calendar account.
    let outlook_dir = TempDir::new();
    let outlook_store = Store::open_with(
        outlook_dir.path(),
        ACCOUNT,
        Remote::outlook(
            outlook::Endpoints::mock(&server.url()),
            outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
        ),
        options(),
    )
    .await
    .expect("outlook store");
    outlook_store
        .sync_calendars(ACCOUNT)
        .await
        .expect("syncs outlook primary");
    let personal_google_token = gmail::StaticToken(format!("{DEV_ACCESS_TOKEN}:{PERSONAL_GOOGLE}"));
    outlook_store.add_calendar_account(
        PERSONAL_GOOGLE,
        CalendarRemote::gmail(PERSONAL_GOOGLE, server.endpoints(), personal_google_token),
    );
    outlook_store
        .sync_calendars(PERSONAL_GOOGLE)
        .await
        .expect("syncs secondary google");

    let wednesday = date(2026, 11, 18)..date(2026, 11, 19);
    outlook_store
        .sync_events(PERSONAL_GOOGLE, wednesday.clone(), &tz)
        .await
        .expect("syncs google events from outlook store");
    let google_events = outlook_store
        .events(&[PERSONAL_GOOGLE.to_owned()], wednesday, &tz)
        .await
        .expect("reads google events");
    assert_eq!(titles(&google_events), ["Dentist"]);

    outlook_store
        .remove_calendar_account(PERSONAL_GOOGLE, true)
        .await
        .expect("removes secondary google");
    assert!(
        outlook_store
            .calendars()
            .await
            .expect("reads")
            .iter()
            .all(|ac| ac.provider == CalendarProviderKind::Microsoft),
        "only primary Outlook calendars remain"
    );
}

#[tokio::test]
async fn outlook_upcoming_events_resolves_primary_and_detects_show_as_ooo() {
    let outlook = Outlook::start().await;
    let primary = outlook.primary().await;

    // Create an Outlook all-day event with ShowAs::OutOfOffice and a neutral title (no OOO word).
    let ooo = CalendarEvent {
        id: Some("e0outlookooo01".into()),
        summary: Some("Away from desk".into()),
        start: Some(EventTime::AllDay(date(2026, 11, 20))),
        end: Some(EventTime::AllDay(date(2026, 11, 22))),
        show_as: ShowAs::OutOfOffice,
        ..CalendarEvent::default()
    };
    outlook
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Create {
                calendar: primary.id.clone(),
                event: ooo,
            },
            None,
        )
        .await
        .expect("queues ooo event");
    assert_eq!(outlook.flush().await, [CalendarOutcome::Sent]);

    // upcoming_events("primary", ...) resolves "primary" to Outlook's default calendar opaque ID.
    let now: Timestamp = "2026-11-17T14:00:00Z".parse().expect("now");
    let lookahead = now + SignedDuration::from_hours(14 * 24);
    let upcoming = outlook
        .store
        .upcoming_events("primary", now, lookahead)
        .await
        .expect("lists upcoming on primary");
    let found_ooo = upcoming
        .iter()
        .find(|ev| ev.summary.as_deref() == Some("Away from desk"))
        .expect("OOO event listed via 'primary'");
    assert!(found_ooo.is_out_of_office());
    assert_eq!(found_ooo.show_as, ShowAs::OutOfOffice);

    let detected = OutOfOfficeEventDetection::default()
        .detected(&upcoming, EMAIL, now)
        .expect("detects Outlook OOO event");
    let ooo_id = found_ooo.id.as_deref().expect("id");
    assert!(
        detected.contains_key(ooo_id),
        "Outlook showAs=oof event detected even without OOO keywords: {detected:?}"
    );
}

#[tokio::test]
async fn outlook_delta_scoped_to_window_plain_text_body_invite_fold_and_account_isolation() {
    let outlook = Outlook::start().await;
    let tz = new_york();
    let primary = outlook.primary().await;

    // 1. Blocker 6: unregistered secondary account never silently falls back to primary.
    let err = outlook
        .store
        .sync_calendars("microsoft unregistered@example.com")
        .await
        .expect_err("unregistered secondary must fail");
    assert!(matches!(err, exo_store::Error::Unauthorized(_)), "{err}");
    assert!(
        outlook
            .store
            .calendars()
            .await
            .expect("reads")
            .iter()
            .all(|ac| ac.account == ACCOUNT),
        "primary calendars were not written under the unregistered secondary account"
    );

    // 2. Blocker 4: multi-line plain-text description with `<` and `&` round-trips without
    // losing newlines or stripping `<alice@example.com>` as HTML.
    let plain_note = "Discuss Q4 <alice@example.com> & Bob\nVerify a < b";
    let note_event = CalendarEvent {
        id: Some("e0plaindesc001".into()),
        summary: Some("Plain text check".into()),
        description: Some(plain_note.into()),
        start: Some(friday_at(10)),
        end: Some(friday_at(11)),
        ..CalendarEvent::default()
    };
    outlook
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Create {
                calendar: primary.id.clone(),
                event: note_event,
            },
            None,
        )
        .await
        .expect("queues plain-text event");
    assert_eq!(outlook.flush().await, [CalendarOutcome::Sent]);
    let on_graph = outlook
        .graph_events_titled(&primary.id, "Plain text check")
        .await;
    assert_eq!(on_graph.len(), 1);
    let body = on_graph[0]
        .body
        .as_ref()
        .and_then(|b| b.content.as_deref())
        .unwrap_or_default();
    assert!(
        body.contains("&lt;alice@example.com&gt;")
            && body.contains("&amp; Bob")
            && body.contains("<br>")
            && body.contains("Verify a &lt; b"),
        "Graph escaped plain text and preserved newlines rather than stripping <...>: {body}"
    );

    // 3. Blocker 1: syncing Week 1 delta after an event moves from Week 1 into Week 2 must NOT
    // wipe Week 2's cached copy of that moved event.
    let week1 = date(2026, 11, 15)..date(2026, 11, 22);
    let week2 = date(2026, 11, 22)..date(2026, 11, 29);
    outlook
        .store
        .sync_events(&primary.id, week1.clone(), &tz)
        .await
        .expect("initial week1");
    outlook
        .store
        .sync_events(&primary.id, week2.clone(), &tz)
        .await
        .expect("initial week2");

    // Move "Plain text check" from Week 1 (Nov 20) to Week 2 (Nov 25) directly on Graph.
    let moved_id = on_graph[0].id.clone().expect("id");
    let move_patch = outlook::calendar::EventPatch {
        start: Some(DateTimeTimeZone::new(
            date(2026, 11, 25).at(15, 0, 0, 0),
            "UTC",
        )),
        end: Some(DateTimeTimeZone::new(
            date(2026, 11, 25).at(16, 0, 0, 0),
            "UTC",
        )),
        ..outlook::calendar::EventPatch::default()
    };
    outlook
        .graph
        .calendar()
        .update_event(Some(&primary.id), &moved_id, &move_patch)
        .await
        .expect("moves to week2");

    // Sync Week 2 first (caches moved event in Week 2), then sync Week 1 (delta emits @removed
    // for moved_id because it left Week 1). Week 2's cached row must survive!
    outlook
        .store
        .sync_events(&primary.id, week2.clone(), &tz)
        .await
        .expect("delta week2");
    outlook
        .store
        .sync_events(&primary.id, week1, &tz)
        .await
        .expect("delta week1");
    let week2_events = outlook
        .store
        .events(std::slice::from_ref(&primary.id), week2, &tz)
        .await
        .expect("reads week2");
    assert!(
        week2_events
            .iter()
            .any(|s| s.event.id.as_deref() == Some(moved_id.as_str())),
        "Week 1 delta @removed did not wipe Week 2's cached moved event"
    );

    // 4. Blocker 5: RespondToInvite with calendar == "primary" folds RSVP into cached primary.id
    // immediately, and blank ical_uid is refused as Gone without querying Graph.
    let thursday = date(2026, 11, 19)..date(2026, 11, 20);
    let call = outlook
        .store
        .events(std::slice::from_ref(&primary.id), thursday.clone(), &tz)
        .await
        .expect("reads thursday")
        .into_iter()
        .find(|s| s.event.summary.as_deref() == Some("Calloway: Atlas feedback call"))
        .expect("call")
        .event;
    assert_eq!(call.my_rsvp(EMAIL), Some(RsvpStatus::NeedsAction));
    outlook
        .store
        .queue_calendar_change(
            2,
            CalendarChange::RespondToInvite {
                calendar: "primary".into(),
                account: Some(ACCOUNT.into()),
                invite: CalendarEvent {
                    ical_uid: call.ical_uid.clone(),
                    summary: call.summary.clone(),
                    start: call.start.clone(),
                    end: call.end.clone(),
                    ..CalendarEvent::default()
                },
                status: RsvpStatus::Accepted,
                thread: None,
                links: None,
            },
            None,
        )
        .await
        .expect("queues primary invite accept");
    assert_eq!(outlook.flush().await, [CalendarOutcome::Sent]);
    let after_fold = outlook
        .store
        .events(std::slice::from_ref(&primary.id), thursday, &tz)
        .await
        .expect("reads thursday after fold")
        .into_iter()
        .find(|s| s.event.id == call.id)
        .expect("call still cached")
        .event;
    assert_eq!(
        after_fold.my_rsvp(EMAIL),
        Some(RsvpStatus::Accepted),
        "RespondToInvite with calendar='primary' folded RSVP into cached opaque primary.id row"
    );

    outlook
        .store
        .queue_calendar_change(
            3,
            CalendarChange::RespondToInvite {
                calendar: "primary".into(),
                account: Some(ACCOUNT.into()),
                invite: CalendarEvent {
                    ical_uid: Some("   ".into()),
                    summary: Some("Blank UID".into()),
                    ..CalendarEvent::default()
                },
                status: RsvpStatus::Accepted,
                thread: None,
                links: None,
            },
            None,
        )
        .await
        .expect("queues blank uid invite");
    assert!(matches!(
        outlook.flush().await.as_slice(),
        [CalendarOutcome::Refused {
            refusal: Refusal::Gone,
            ..
        }]
    ));

    // 5. Quality 3: unreadable opaque calendar IDs (400 malformed or 403 access denied) in
    // free_busy return None without aborting the rest of the query.
    let from: Timestamp = "2026-11-16T05:00:00Z".parse().expect("ts");
    let to: Timestamp = "2026-11-21T05:00:00Z".parse().expect("ts");
    let secondary_cal = outlook
        .store
        .calendars()
        .await
        .expect("reads cals")
        .into_iter()
        .find(|ac| !ac.calendar.primary)
        .expect("secondary calendar")
        .calendar
        .id;
    outlook
        .fault(json!({
            "method": "GET",
            "path": format!("/v1.0/me/calendars/{secondary_cal}/calendarView"),
            "status": 403,
            "reason": "ErrorAccessDenied"
        }))
        .await;
    let busy = outlook
        .store
        .free_busy(
            &[
                primary.id.clone(),
                secondary_cal,
                "nonexistent-opaque-cal-id".to_owned(),
            ],
            from,
            to,
        )
        .await
        .expect("free_busy tolerates unreadable opaque calendars");
    assert!(busy[0].1.is_some());
    assert_eq!(busy[1].1, None, "403 unreadable calendar maps to None");
    assert_eq!(busy[2].1, None, "400 malformed calendar ID maps to None");
}

#[tokio::test]
async fn clear_cache_preserves_and_does_not_misroute_queued_secondary_calendar_change() {
    let outlook = Outlook::start().await;
    let tz = new_york();
    let friday = date(2026, 11, 20)..date(2026, 11, 21);

    // Register a secondary Google calendar account on an Outlook primary store.
    let google_remote = CalendarRemote::gmail(
        PERSONAL_GOOGLE,
        outlook.server.endpoints(),
        gmail::StaticToken(DEV_ACCESS_TOKEN.into()),
    );
    outlook
        .store
        .add_calendar_account(PERSONAL_GOOGLE, google_remote.clone());
    let google_cals = outlook
        .store
        .sync_calendars(PERSONAL_GOOGLE)
        .await
        .expect("syncs secondary Google calendars");
    let google_cal = google_cals
        .iter()
        .find(|cal| cal.primary)
        .expect("secondary Google primary calendar")
        .id
        .clone();

    // Persist calendar_accounts in Settings (as the UI does after listing calendars).
    let mut settings: exo_store::Settings = outlook.store.settings().await.expect("reads settings");
    settings.update_calendar(PERSONAL_GOOGLE, &google_cal, |cal| {
        cal.sync = true;
        cal.show = true;
    });
    outlook
        .store
        .save_settings(&settings)
        .await
        .expect("saves settings");

    // Create a real event on the secondary Google calendar so we have a valid event ID to edit.
    let google_client = gmail::Client::new(
        outlook.server.endpoints(),
        gmail::StaticToken(DEV_ACCESS_TOKEN.into()),
    );
    let created = google_client
        .calendar()
        .insert_event(
            &google_cal,
            &gmail::calendar::Event {
                summary: Some("Before clear cache".into()),
                start: Some(gmail::calendar::EventDateTime {
                    date_time: Some("2026-11-20T14:00:00-05:00".into()),
                    time_zone: Some("America/New_York".into()),
                    ..Default::default()
                }),
                end: Some(gmail::calendar::EventDateTime {
                    date_time: Some("2026-11-20T15:00:00-05:00".into()),
                    time_zone: Some("America/New_York".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some("none"),
        )
        .await
        .expect("inserts secondary Google event");
    let event_id = created.id.expect("event id");

    // Queue an offline edit targeting the secondary Google calendar, then Clear Cache and Restart.
    outlook
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Edit {
                calendar: google_cal.clone(),
                id: event_id.clone(),
                series_id: None,
                patch: EventPatch {
                    summary: Some("Edited offline after clear cache".into()),
                    ..EventPatch::default()
                },
                notify: false,
                shift: None,
                shown: Vec::new(),
            },
            None,
        )
        .await
        .expect("queues edit");

    let Outlook {
        server,
        _dir: dir,
        store,
        ..
    } = outlook;
    drop(store);
    Store::clear_cache(dir.path(), ACCOUNT)
        .await
        .expect("clears cache");

    // Reopen the Outlook primary store WITHOUT registering the secondary Google account yet.
    // `calendars` table is empty, while `settings.calendar_accounts` and `calendar_changes` were
    // preserved. Flushing must postpone (Unauthorized), NOT misroute to Graph and drop as Refused!
    let reopened = Store::open_with(
        dir.path(),
        ACCOUNT,
        Remote::outlook(
            outlook::Endpoints::mock(&server.url()),
            outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
        ),
        options(),
    )
    .await
    .expect("reopens Outlook store");
    assert_eq!(reopened.calendar_pending().await.expect("pending"), 1);
    let (due, _) = reopened
        .due_calendar_changes(true)
        .await
        .expect("reads due changes");
    assert_eq!(due.len(), 1);
    let outcome = reopened
        .send_calendar_change(&due[0])
        .await
        .expect("sends change");
    assert_eq!(
        outcome,
        CalendarOutcome::Postponed,
        "secondary calendar change must wait for its account instead of misrouting to primary"
    );
    assert_eq!(reopened.calendar_pending().await.expect("still pending"), 1);

    // Once the secondary Google account is registered, flushing delivers the edit cleanly to Google.
    reopened.add_calendar_account(PERSONAL_GOOGLE, google_remote);
    let (due, _) = reopened
        .due_calendar_changes(true)
        .await
        .expect("reads due changes after registering secondary");
    assert_eq!(due.len(), 1);
    let outcome = reopened
        .send_calendar_change(&due[0])
        .await
        .expect("flushes to secondary Google account");
    assert_eq!(outcome, CalendarOutcome::Sent);
    assert_eq!(reopened.calendar_pending().await.expect("drained"), 0);

    reopened
        .sync_events(&google_cal, friday.clone(), &tz)
        .await
        .expect("syncs secondary events");
    let synced = reopened
        .events(std::slice::from_ref(&google_cal), friday, &tz)
        .await
        .expect("reads synced events");
    assert!(
        synced
            .iter()
            .any(|s| s.event.id.as_deref() == Some(event_id.as_str())
                && s.event.summary.as_deref() == Some("Edited offline after clear cache")),
        "queued edit landed on secondary Google calendar: {:?}",
        titles(&synced)
    );
}
