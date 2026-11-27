//! Calendar v3's recurring series that fall on a weekday of the month or year, or on one of
//! several weekdays, expanded as Google Calendar expands them.

use crate::support;

use gmail::calendar::*;

fn at(time: &str) -> EventDateTime {
    EventDateTime {
        date_time: Some(time.into()),
        time_zone: Some("America/New_York".into()),
        ..EventDateTime::default()
    }
}

/// The local start dates of a new series' instances in 2026–2029, from `events.list` with
/// `singleEvents` and from `instances`.
async fn dates(calendar: &Calendar<'_>, summary: &str, start: &str, rule: &str) -> Vec<String> {
    let event = Event {
        summary: Some(summary.into()),
        start: Some(at(&format!("{start}T09:00:00-05:00"))),
        end: Some(at(&format!("{start}T10:00:00-05:00"))),
        recurrence: vec![rule.into()],
        ..Event::default()
    };
    let series = calendar
        .insert_event("primary", &event, None)
        .await
        .unwrap();
    let (min, max) = ("2026-01-01T00:00:00Z", "2030-01-01T00:00:00Z");
    let listed = calendar
        .events(
            "primary",
            &EventsQuery {
                time_min: Some(min.into()),
                time_max: Some(max.into()),
                single_events: true,
                q: Some(summary.into()),
                ..EventsQuery::default()
            },
        )
        .await
        .unwrap();
    let instances = calendar
        .instances(
            "primary",
            series.id.as_deref().unwrap(),
            Some(min),
            Some(max),
        )
        .await
        .unwrap();
    let days = |events: &Events| -> Vec<String> {
        events
            .items
            .iter()
            .map(|e| e.start.as_ref().unwrap().date_time.as_deref().unwrap()[..10].to_owned())
            .collect()
    };
    assert_eq!(days(&listed), days(&instances), "{summary}");
    days(&listed)
}

#[tokio::test]
async fn weekdays_of_the_month_and_year() {
    let server = support::server().await;
    let client = support::client(&server);
    let calendar = client.calendar();
    assert_eq!(
        dates(
            &calendar,
            "Thanksgiving",
            "2026-11-26",
            "RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=4TH;COUNT=4"
        )
        .await,
        ["2026-11-26", "2027-11-25", "2028-11-23", "2029-11-22"]
    );
    assert_eq!(
        dates(
            &calendar,
            "Retro",
            "2026-11-06",
            "RRULE:FREQ=MONTHLY;BYDAY=TH,FR;BYSETPOS=2;COUNT=3"
        )
        .await,
        ["2026-11-06", "2026-12-04", "2027-01-07"],
        "the second Thursday or Friday"
    );
    assert_eq!(
        dates(
            &calendar,
            "Close the books",
            "2026-11-30",
            "RRULE:FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1;COUNT=3"
        )
        .await,
        ["2026-11-30", "2026-12-31", "2027-01-29"],
        "the last weekday"
    );
    assert_eq!(
        dates(
            &calendar,
            "Pay day",
            "2026-11-15",
            "RRULE:FREQ=MONTHLY;BYMONTHDAY=1,15;COUNT=4"
        )
        .await,
        ["2026-11-15", "2026-12-01", "2026-12-15", "2027-01-01"]
    );
}
