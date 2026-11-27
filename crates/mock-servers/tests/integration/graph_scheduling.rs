//! Meetings and scheduling through Microsoft Graph: an invitee's responses, the organizer's
//! invitations, updates and cancellations as meeting mail, free/busy, meeting time
//! suggestions, rooms, and series that fall on a weekday of the month.

use crate::graph;

use graph::{Graph, Metadata, assert_error, urlencode};
use mock_servers::DEV_ACCESS_TOKEN;
use reqwest::Method;
use serde_json::{Value, json};

const WEEK: &str = "startDateTime=2026-11-16T00:00:00-05:00&endDateTime=2026-11-23T00:00:00-05:00";
const EASTERN: (&str, &str) = ("Prefer", "outlook.timezone=\"Eastern Standard Time\"");

async fn event(graph: &Graph, subject: &str) -> Value {
    let (week, _) = graph
        .pages(&format!("/v1.0/me/calendarView?{WEEK}"), &[])
        .await;
    let (all, _) = graph.pages("/v1.0/me/events?$top=100", &[]).await;
    week.into_iter()
        .chain(all)
        .find(|e| e["subject"] == subject)
        .unwrap_or_else(|| panic!("no {subject}"))
}

/// Sent Items' messages with `subject`.
async fn sent(graph: &Graph, subject: &str) -> Vec<Value> {
    let filter = urlencode(&format!("subject eq '{}'", subject.replace('\'', "''")));
    let reply = graph
        .get(&format!(
            "/v1.0/me/mailFolders/sentitems/messages?$filter={filter}&$expand=microsoft.graph.eventMessage/event"
        ))
        .await;
    assert_eq!(reply.status, 200, "{}", reply.json);
    reply.json["value"].as_array().unwrap().clone()
}

fn post(id: &str, action: &str) -> String {
    format!("/v1.0/me/events/{id}/{action}")
}

#[tokio::test]
async fn invitees_answer_their_organizers() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let invite = event(&graph, "Calloway: Atlas feedback call").await;
    let id = invite["id"].as_str().unwrap();
    let organizer = invite["organizer"]["emailAddress"]["address"]
        .as_str()
        .unwrap()
        .to_owned();

    let accepted = graph
        .json(
            Method::POST,
            &post(id, "accept"),
            json!({ "comment": "See you there", "sendResponse": true }),
        )
        .await;
    assert_eq!(accepted.status, 202, "{}", accepted.json);
    let read = graph.get(&format!("/v1.0/me/events/{id}")).await.json;
    assert_eq!(
        (
            read["responseStatus"]["response"].as_str(),
            read["showAs"].as_str()
        ),
        (Some("accepted"), Some("busy"))
    );
    let theirs = graph
        .get(&format!(
            "/calendar/v3/calendars/primary/events?q={}",
            urlencode("Calloway: Atlas feedback call")
        ))
        .await
        .json;
    let me = theirs["items"][0]["attendees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["self"] == true)
        .unwrap()
        .clone();
    assert_eq!(
        (me["responseStatus"].as_str(), me["comment"].as_str()),
        (Some("accepted"), Some("See you there"))
    );
    let answer = &sent(&graph, "Accepted: Calloway: Atlas feedback call").await[0];
    metadata.assert("message", answer);
    assert_eq!(
        answer["@odata.type"],
        "#microsoft.graph.eventMessageResponse"
    );
    assert_eq!(
        (
            answer["meetingMessageType"].as_str(),
            answer["responseType"].as_str()
        ),
        (Some("meetingAccepted"), Some("accepted"))
    );
    assert_eq!(
        answer["toRecipients"][0]["emailAddress"]["address"],
        organizer.as_str()
    );
    assert!(
        answer["body"]["content"]
            .as_str()
            .unwrap()
            .contains("See you there")
    );

    let proposal = json!({
        "Comment": "Can we do Friday?",
        "SendResponse": "true",
        "ProposedNewTime": {
            "Start": { "DateTime": "2026-11-20T14:00:00", "TimeZone": "Eastern Standard Time" },
            "End": { "DateTime": "2026-11-20T15:00:00", "TimeZone": "Eastern Standard Time" },
        },
    });
    assert_eq!(
        graph
            .json(Method::POST, &post(id, "tentativelyAccept"), proposal)
            .await
            .status,
        202
    );
    let counter = &sent(&graph, "New Time Proposed: Calloway: Atlas feedback call").await[0];
    assert_eq!(counter["responseType"], "tentativelyAccepted");
    assert_eq!(
        counter["proposedNewTime"]["start"]["dateTime"],
        "2026-11-20T19:00:00.0000000"
    );
    assert_eq!(
        graph.get(&format!("/v1.0/me/events/{id}")).await.json["responseStatus"]["response"],
        "tentativelyAccepted"
    );

    let before = graph.get("/v1.0/me/mailFolders/sentitems").await.json["totalItemCount"].clone();
    assert_eq!(
        graph
            .json(
                Method::POST,
                &post(id, "decline"),
                json!({ "sendResponse": false })
            )
            .await
            .status,
        202
    );
    assert_eq!(
        graph.get("/v1.0/me/mailFolders/sentitems").await.json["totalItemCount"],
        before,
        "nothing sent"
    );
    let declined = graph.get(&format!("/v1.0/me/events/{id}")).await.json;
    assert_eq!(
        (
            declined["responseStatus"]["response"].as_str(),
            declined["showAs"].as_str()
        ),
        (Some("declined"), Some("free"))
    );

    let quiet = json!({ "sendResponse": false, "proposedNewTime": { "start": { "dateTime": "2026-11-20T14:00:00", "timeZone": "UTC" }, "end": { "dateTime": "2026-11-20T15:00:00", "timeZone": "UTC" } } });
    assert_error(
        &graph.json(Method::POST, &post(id, "decline"), quiet).await,
        400,
        "ErrorInvalidRequest",
    );
    assert_error(
        &graph
            .json(
                Method::POST,
                &post(id, "accept"),
                json!({ "mood": "great" }),
            )
            .await,
        400,
        "RequestBodyRead",
    );
    let mine = event(&graph, "Weekly product sync").await;
    assert_error(
        &graph
            .json(
                Method::POST,
                &post(mine["id"].as_str().unwrap(), "accept"),
                json!({}),
            )
            .await,
        400,
        "ErrorInvalidRequest",
    );

    // An answer to one occurrence makes it an exception of its series.
    let one = event(&graph, "Priya / Maya 1:1").await;
    let occurrence = one["id"].as_str().unwrap();
    assert_eq!(
        graph
            .json(
                Method::POST,
                &post(occurrence, "tentativelyAccept"),
                json!({ "sendResponse": false })
            )
            .await
            .status,
        202
    );
    let changed = graph
        .get(&format!("/v1.0/me/events/{occurrence}"))
        .await
        .json;
    assert_eq!(
        (
            changed["type"].as_str(),
            changed["responseStatus"]["response"].as_str()
        ),
        (Some("exception"), Some("tentativelyAccepted"))
    );
    let series = graph
        .get(&format!(
            "/v1.0/me/events/{}",
            one["seriesMasterId"].as_str().unwrap()
        ))
        .await
        .json;
    assert_eq!(series["responseStatus"]["response"], "accepted");
}

#[tokio::test]
async fn organizers_invite_update_and_cancel() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let created = graph
        .json(
            Method::POST,
            "/v1.0/me/events",
            json!({
                "subject": "Roadmap review",
                "body": { "contentType": "text", "content": "Bring the Q1 numbers." },
                "start": { "dateTime": "2026-11-24T15:00:00", "timeZone": "Eastern Standard Time" },
                "end": { "dateTime": "2026-11-24T16:00:00", "timeZone": "Eastern Standard Time" },
                "location": { "displayName": "Hudson Room" },
                "allowNewTimeProposals": false,
                "attendees": [
                    { "emailAddress": { "address": "daniel@brightmoor.co", "name": "Daniel Reyes" }, "type": "required" },
                    { "emailAddress": { "address": "grace@juniperpartners.vc" }, "type": "optional" },
                ],
            }),
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.json);
    let id = created.json["id"].as_str().unwrap();
    let invitations = sent(&graph, "Roadmap review").await;
    assert_eq!(invitations.len(), 1);
    let invitation = &invitations[0];
    metadata.assert("message", invitation);
    assert_eq!(
        invitation["@odata.type"],
        "#microsoft.graph.eventMessageRequest"
    );
    assert_eq!(
        (
            invitation["meetingMessageType"].as_str(),
            invitation["meetingRequestType"].as_str()
        ),
        (Some("meetingRequest"), Some("newMeetingRequest"))
    );
    assert_eq!(invitation["allowNewTimeProposals"], false);
    assert_eq!(
        invitation["event"]["id"], id,
        "the invitation names its event"
    );
    let to: Vec<&str> = invitation["toRecipients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["emailAddress"]["address"].as_str().unwrap())
        .collect();
    assert_eq!(to, ["daniel@brightmoor.co", "grace@juniperpartners.vc"]);
    assert!(
        invitation["body"]["content"]
            .as_str()
            .unwrap()
            .contains("Bring the Q1 numbers.")
    );

    graph
        .json(
            Method::PATCH,
            &format!("/v1.0/me/events/{id}"),
            json!({ "categories": ["Blue category"] }),
        )
        .await;
    assert_eq!(
        sent(&graph, "Roadmap review").await.len(),
        1,
        "a change invitees don't see sends nothing"
    );
    let later = json!({
        "start": { "dateTime": "2026-11-24T16:00:00", "timeZone": "Eastern Standard Time" },
        "end": { "dateTime": "2026-11-24T17:00:00", "timeZone": "Eastern Standard Time" },
    });
    assert_eq!(
        graph
            .json(Method::PATCH, &format!("/v1.0/me/events/{id}"), later)
            .await
            .status,
        200
    );
    let updates = sent(&graph, "Roadmap review").await;
    assert_eq!(updates.len(), 2);
    let update = updates
        .iter()
        .find(|m| m["meetingRequestType"] == "fullUpdate")
        .expect("an update");
    assert_eq!(
        update["startDateTime"]["dateTime"],
        "2026-11-24T21:00:00.0000000"
    );

    let cancel = graph
        .json(
            Method::POST,
            &post(id, "cancel"),
            json!({ "Comment": "Moving this to January." }),
        )
        .await;
    assert_eq!(cancel.status, 202, "{}", cancel.json);
    let cancellation = &sent(&graph, "Canceled: Roadmap review").await[0];
    metadata.assert("message", cancellation);
    assert_eq!(
        (
            cancellation["@odata.type"].as_str(),
            cancellation["meetingMessageType"].as_str()
        ),
        (
            Some("#microsoft.graph.eventMessage"),
            Some("meetingCancelled")
        )
    );
    assert!(
        cancellation["body"]["content"]
            .as_str()
            .unwrap()
            .contains("Moving this to January.")
    );
    assert_error(
        &graph.get(&format!("/v1.0/me/events/{id}")).await,
        404,
        "ErrorItemNotFound",
    );

    let invite = event(&graph, "Calloway: Atlas feedback call").await;
    let refused = graph
        .json(
            Method::POST,
            &post(invite["id"].as_str().unwrap(), "cancel"),
            json!({}),
        )
        .await;
    assert_error(&refused, 400, "ErrorInvalidRequest");
    assert!(
        refused.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("You need to be an organizer to cancel a meeting.")
    );

    let go = event(&graph, "Atlas go/no-go").await;
    assert_eq!(
        graph
            .request(
                Method::DELETE,
                &format!("/v1.0/me/events/{}", go["id"].as_str().unwrap()),
                &[],
                None
            )
            .await
            .status,
        204
    );
    assert_eq!(
        sent(&graph, "Canceled: Atlas go/no-go").await.len(),
        1,
        "an organizer's delete cancels the meeting"
    );
    let dinner = event(&graph, "Dinner with Jess 🍕").await;
    graph
        .request(
            Method::DELETE,
            &format!("/v1.0/me/events/{}", dinner["id"].as_str().unwrap()),
            &[],
            None,
        )
        .await;
    assert!(
        sent(&graph, "Canceled: Dinner with Jess 🍕")
            .await
            .is_empty(),
        "an appointment has no one to tell"
    );
}

#[tokio::test]
async fn free_busy_and_suggestions() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let body = json!({
        "schedules": ["maya@brightmoor.co", "priya@brightmoor.co", "hudson@brightmoor.co", "someone@example.com"],
        "startTime": { "dateTime": "2026-11-16T08:00:00", "timeZone": "Eastern Standard Time" },
        "endTime": { "dateTime": "2026-11-16T18:00:00", "timeZone": "Eastern Standard Time" },
        "availabilityViewInterval": 60,
    });
    let reply = graph
        .json_with(
            Method::POST,
            "/v1.0/me/calendar/getSchedule",
            &[EASTERN],
            body.clone(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.json);
    metadata.assert_collection("scheduleInformation", &reply.json);
    let schedules = reply.json["value"].as_array().unwrap();
    let views: Vec<&str> = schedules
        .iter()
        .map(|s| s["availabilityView"].as_str().unwrap())
        .collect();
    assert_eq!(views, ["0020002000", "0020000000", "0020002000", ""]);
    let sync = schedules[0]["scheduleItems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["subject"] == "Weekly product sync")
        .unwrap()
        .clone();
    assert_eq!(
        (sync["status"].as_str(), sync["location"].as_str()),
        (Some("busy"), Some("Hudson Room"))
    );
    assert_eq!(
        sync["start"],
        json!({ "dateTime": "2026-11-16T10:00:00.0000000", "timeZone": "Eastern Standard Time" })
    );
    assert_eq!(
        schedules[0]["workingHours"]["timeZone"]["name"],
        "Eastern Standard Time"
    );
    assert_eq!(
        schedules[3]["error"]["responseCode"],
        "ErrorMailRecipientNotFound"
    );
    assert_eq!(
        graph
            .get("/v1.0/users/maya@brightmoor.co/calendar")
            .await
            .status,
        200
    );
    let mut personal = Graph::start().await;
    personal.token = format!("{DEV_ACCESS_TOKEN}:maya.okafor@outlook.com");
    assert_error(
        &personal
            .json(Method::POST, "/v1.0/me/calendar/getSchedule", body)
            .await,
        403,
        "ErrorAccessDenied",
    );

    let find = |attendee: &str, extra: Value| {
        let mut request = json!({
            "attendees": [{ "type": "required", "emailAddress": { "address": attendee } }],
            "timeConstraint": { "activityDomain": "work", "timeSlots": [{
                "start": { "dateTime": "2026-11-19T13:00:00", "timeZone": "Eastern Standard Time" },
                "end": { "dateTime": "2026-11-19T16:00:00", "timeZone": "Eastern Standard Time" },
            }] },
            "meetingDuration": "PT1H",
            "returnSuggestionReasons": true,
        });
        for (key, value) in extra.as_object().unwrap() {
            request[key] = value.clone();
        }
        let graph = &graph;
        async move {
            graph
                .json_with(
                    Method::POST,
                    "/v1.0/me/findMeetingTimes",
                    &[EASTERN],
                    request,
                )
                .await
        }
    };
    let found = find("priya@brightmoor.co", json!({})).await;
    assert_eq!(found.status, 200, "{}", found.json);
    metadata.assert("meetingTimeSuggestionsResult", &found.json);
    let starts: Vec<&str> = found.json["meetingTimeSuggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["meetingTimeSlot"]["start"]["dateTime"].as_str().unwrap())
        .collect();
    assert_eq!(
        starts,
        ["2026-11-19T13:00:00.0000000", "2026-11-19T15:00:00.0000000"],
        "around Priya's 2 o'clock"
    );
    let first = &found.json["meetingTimeSuggestions"][0];
    assert_eq!(
        (first["confidence"].as_f64(), first["order"].as_u64()),
        (Some(100.0), Some(1))
    );
    assert_eq!(first["attendeeAvailability"][0]["availability"], "free");
    assert_eq!(
        first["suggestionReason"],
        "Suggested because it is one of the nearest times when all attendees are available."
    );
    let outside = find("someone@example.com", json!({})).await.json;
    assert_eq!(
        outside["emptySuggestionsReason"],
        "attendeesUnavailableOrUnknown"
    );
    let lenient = find(
        "someone@example.com",
        json!({ "minimumAttendeePercentage": 40 }),
    )
    .await
    .json;
    assert_eq!(lenient["meetingTimeSuggestions"][0]["confidence"], 49.0);
    let room = find(
        "priya@brightmoor.co",
        json!({ "locationConstraint": { "isRequired": false, "suggestLocation": true } }),
    )
    .await
    .json;
    assert_eq!(
        room["meetingTimeSuggestions"][0]["locations"][0]["displayName"],
        "Hudson Room"
    );
}

#[tokio::test]
async fn rooms_are_places() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let rooms = graph.get("/v1.0/places/microsoft.graph.room").await;
    assert_eq!(rooms.status, 200, "{}", rooms.json);
    metadata.assert_collection("room", &rooms.json);
    let names: Vec<&str> = rooms.json["value"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["displayName"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Hudson Room", "Big conference room"]);
    let big = graph
        .get(&format!(
            "/v1.0/places/microsoft.graph.room?$filter={}",
            urlencode("capacity ge 12")
        ))
        .await
        .json;
    assert_eq!(big["value"].as_array().unwrap().len(), 1);
    let one = graph.get("/v1.0/places/hudson@brightmoor.co").await.json;
    assert_eq!(
        (one["@odata.type"].as_str(), one["capacity"].as_i64()),
        (Some("#microsoft.graph.room"), Some(10))
    );
    metadata.assert("place", &one);
    let lists = graph
        .get("/v1.0/places/microsoft.graph.roomlist")
        .await
        .json;
    metadata.assert_collection("roomList", &lists);
    let address = lists["value"][0]["emailAddress"].as_str().unwrap();
    let inside = graph
        .get(&format!(
            "/v1.0/places/{address}/microsoft.graph.roomlist/rooms"
        ))
        .await
        .json;
    assert_eq!(inside["value"].as_array().unwrap().len(), 2);
    assert_error(
        &graph.get("/v1.0/places/nowhere@brightmoor.co").await,
        404,
        "ErrorItemNotFound",
    );
    let mut personal = Graph::start().await;
    personal.token = format!("{DEV_ACCESS_TOKEN}:maya.okafor@outlook.com");
    assert_error(
        &personal.get("/v1.0/places/microsoft.graph.room").await,
        403,
        "ErrorAccessDenied",
    );
}

#[tokio::test]
async fn series_on_a_weekday_of_the_month() {
    let graph = Graph::start().await;
    let created = graph
        .json(
            Method::POST,
            "/v1.0/me/events",
            json!({
                "subject": "Thanksgiving dinner",
                "start": { "dateTime": "2026-11-26T17:00:00", "timeZone": "Eastern Standard Time" },
                "end": { "dateTime": "2026-11-26T20:00:00", "timeZone": "Eastern Standard Time" },
                "recurrence": {
                    "pattern": { "type": "relativeYearly", "interval": 1, "month": 11, "daysOfWeek": ["thursday"], "index": "fourth" },
                    "range": { "type": "numbered", "startDate": "2026-11-26", "numberOfOccurrences": 3 },
                },
            }),
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.json);
    let id = created.json["id"].as_str().unwrap();
    let span = "startDateTime=2026-01-01T00:00:00Z&endDateTime=2030-01-01T00:00:00Z";
    let (instances, _) = graph
        .pages(
            &format!("/v1.0/me/events/{id}/instances?{span}"),
            &[EASTERN],
        )
        .await;
    let days: Vec<&str> = instances
        .iter()
        .map(|e| &e["start"]["dateTime"].as_str().unwrap()[..10])
        .collect();
    assert_eq!(days, ["2026-11-26", "2027-11-25", "2028-11-23"]);
    // Google's API expands the series the same way.
    let theirs = graph
        .get(&format!(
            "/calendar/v3/calendars/primary/events?singleEvents=true&q={}&timeMin=2026-01-01T00:00:00Z&timeMax=2030-01-01T00:00:00Z",
            urlencode("Thanksgiving dinner")
        ))
        .await
        .json;
    let google_days: Vec<&str> = theirs["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| &e["start"]["dateTime"].as_str().unwrap()[..10])
        .collect();
    assert_eq!(google_days, days);

    let several = graph
        .json(
            Method::POST,
            "/v1.0/me/events",
            json!({
                "subject": "Retro",
                "start": { "dateTime": "2026-11-06T16:00:00", "timeZone": "Eastern Standard Time" },
                "end": { "dateTime": "2026-11-06T17:00:00", "timeZone": "Eastern Standard Time" },
                "recurrence": {
                    "pattern": { "type": "relativeMonthly", "interval": 1, "daysOfWeek": ["thursday", "friday"], "index": "second" },
                    "range": { "type": "numbered", "startDate": "2026-11-01", "numberOfOccurrences": 3 },
                },
            }),
        )
        .await;
    assert_eq!(several.status, 201, "{}", several.json);
    assert_eq!(
        several.json["recurrence"]["pattern"]["daysOfWeek"],
        json!(["thursday", "friday"])
    );
    let id = several.json["id"].as_str().unwrap();
    let (instances, _) = graph
        .pages(
            &format!("/v1.0/me/events/{id}/instances?{span}"),
            &[EASTERN],
        )
        .await;
    let days: Vec<&str> = instances
        .iter()
        .map(|e| &e["start"]["dateTime"].as_str().unwrap()[..10])
        .collect();
    assert_eq!(
        days,
        ["2026-11-06", "2026-12-04", "2027-01-07"],
        "the second Thursday or Friday"
    );
}
