//! Open extensions on messages through Microsoft Graph: made, read, updated and deleted;
//! expanded and filtered on; kept through moves, copies, draft edits and sending; and unseen by
//! Gmail.

use crate::graph;

use graph::{Graph, Metadata, assert_error, recipients, urlencode};
use reqwest::Method;
use serde_json::{Value, json};

const IMMUTABLE: (&str, &str) = ("Prefer", "IdType=\"ImmutableId\"");
const NAME: &str = "com.brightmoor.exo.snoozed";
const FULL: &str = "Microsoft.OutlookServices.OpenTypeExtension.com.brightmoor.exo.snoozed";

fn marker() -> Value {
    json!({
        "@odata.type": "microsoft.graph.openTypeExtension",
        "extensionName": NAME,
        "until": "2026-12-01T09:00:00Z",
        "count": 2,
        "tags": ["inbox", "later"],
    })
}

async fn mark(graph: &Graph, id: &str) -> Value {
    let reply = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/extensions"),
            marker(),
        )
        .await;
    assert_eq!(reply.status, 201, "{}", reply.json);
    reply.json
}

/// The ids of the messages `path` lists with the marker filter.
async fn marked(graph: &Graph, path: &str, id: &str) -> Vec<String> {
    let filter = urlencode(&format!("Extensions/any(f:f/id eq '{id}')"));
    let (items, _) = graph
        .pages(&format!("{path}?$filter={filter}&$top=100"), &[])
        .await;
    items
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_owned())
        .collect()
}

/// A message's raw MIME and labels through Gmail.
async fn through_gmail(graph: &Graph, internet_id: &str) -> (Value, Value) {
    let rfc822 = internet_id.trim_matches(['<', '>']);
    let found = graph
        .get(&format!(
            "/gmail/v1/users/me/messages?q={}",
            urlencode(&format!("rfc822msgid:{rfc822}"))
        ))
        .await
        .json;
    let id = found["messages"][0]["id"].as_str().unwrap();
    let raw = graph
        .get(&format!("/gmail/v1/users/me/messages/{id}?format=raw"))
        .await
        .json;
    (raw["raw"].clone(), raw["labelIds"].clone())
}

#[tokio::test]
async fn extensions_mark_messages() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let message = graph.first("inbox", "isRead eq true").await;
    let id = message["id"].as_str().unwrap();
    let internet_id = message["internetMessageId"].as_str().unwrap();
    let gmail_before = through_gmail(&graph, internet_id).await;

    let made = mark(&graph, id).await;
    metadata.assert("openTypeExtension", &made);
    assert_eq!(made["@odata.type"], "#microsoft.graph.openTypeExtension");
    assert_eq!(
        (made["id"].as_str(), made["extensionName"].as_str()),
        (Some(FULL), Some(NAME))
    );
    assert_eq!(
        (made["count"].as_i64(), made["tags"][1].as_str()),
        (Some(2), Some("later"))
    );
    assert!(
        made["@odata.context"]
            .as_str()
            .unwrap()
            .ends_with(&format!("messages('{id}')/extensions/$entity"))
    );
    for name in [
        NAME,
        FULL,
        "microsoft.graph.openTypeExtension.com.brightmoor.exo.snoozed",
    ] {
        let read = graph
            .get(&format!("/v1.0/me/messages/{id}/extensions/{name}"))
            .await;
        assert_eq!(read.status, 200, "{name}: {}", read.json);
        assert_eq!(read.json["until"], "2026-12-01T09:00:00Z");
    }
    let all = graph
        .get(&format!("/v1.0/me/messages/{id}/extensions"))
        .await
        .json;
    metadata.assert_collection("openTypeExtension", &all);
    assert_eq!(all["value"].as_array().unwrap().len(), 1);

    let again = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/extensions"),
            marker(),
        )
        .await;
    assert_error(&again, 409, "NameAlreadyExists");
    for name in ["com.microsoft.exo", "com.onmicrosoft.exo", "reminders"] {
        let mut bad = marker();
        bad["extensionName"] = name.into();
        assert_error(
            &graph
                .json(
                    Method::POST,
                    &format!("/v1.0/me/messages/{id}/extensions"),
                    bad,
                )
                .await,
            400,
            "ErrorInvalidRequest",
        );
    }
    let mut untyped = marker();
    untyped.as_object_mut().unwrap().remove("@odata.type");
    assert_error(
        &graph
            .json(
                Method::POST,
                &format!("/v1.0/me/messages/{id}/extensions"),
                untyped,
            )
            .await,
        400,
        "RequestBodyRead",
    );
    let mut nested = marker();
    nested["extensionName"] = "com.brightmoor.exo.other".into();
    nested["deep"] = json!({ "a": 1 });
    assert_error(
        &graph
            .json(
                Method::POST,
                &format!("/v1.0/me/messages/{id}/extensions"),
                nested,
            )
            .await,
        400,
        "RequestBodyRead",
    );

    // An update changes what it names and keeps the rest; null isn't allowed.
    let changed = graph
        .json(Method::PATCH, &format!("/v1.0/me/messages/{id}/extensions/{NAME}"), json!({ "@odata.type": "#microsoft.graph.openTypeExtension", "count": 3, "extra": true }))
        .await;
    assert_eq!(changed.status, 200, "{}", changed.json);
    assert_eq!(
        (
            changed.json["count"].as_i64(),
            changed.json["extra"].as_bool()
        ),
        (Some(3), Some(true))
    );
    assert_eq!(
        changed.json["until"], "2026-12-01T09:00:00Z",
        "what an update omits stays"
    );
    assert_error(
        &graph
            .json(
                Method::PATCH,
                &format!("/v1.0/me/messages/{id}/extensions/{NAME}"),
                json!({ "count": null }),
            )
            .await,
        400,
        "RequestBodyRead",
    );
    assert_error(
        &graph
            .json(
                Method::PATCH,
                &format!("/v1.0/me/messages/{id}/extensions/com.brightmoor.nope"),
                json!({ "count": 1 }),
            )
            .await,
        404,
        "ErrorItemNotFound",
    );

    let expanded = graph
        .get(&format!(
            "/v1.0/me/messages/{id}?$expand={}",
            urlencode(&format!("extensions($filter=id eq '{FULL}')"))
        ))
        .await
        .json;
    metadata.assert("message", &expanded);
    assert_eq!(expanded["extensions"][0]["id"], FULL);
    let none = graph
        .get(&format!(
            "/v1.0/me/messages/{id}?$expand={}",
            urlencode("Extensions($filter=id eq 'com.brightmoor.other')")
        ))
        .await
        .json;
    assert_eq!(none["extensions"], json!([]));
    assert_eq!(
        graph
            .get(&format!("/v1.0/me/messages/{id}?$expand=extensions"))
            .await
            .json["extensions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    assert_eq!(marked(&graph, "/v1.0/me/messages", NAME).await, [id]);
    assert_eq!(
        marked(&graph, "/v1.0/me/messages", FULL).await,
        [id],
        "by the full id too"
    );
    assert_eq!(
        marked(&graph, "/v1.0/me/mailFolders/inbox/messages", NAME).await,
        [id]
    );
    assert!(
        marked(&graph, "/v1.0/me/mailFolders/archive/messages", NAME)
            .await
            .is_empty()
    );
    let both = urlencode(&format!(
        "Extensions/any(f:f/id eq '{NAME}') and isRead eq true"
    ));
    assert_eq!(
        graph
            .get(&format!("/v1.0/me/messages?$filter={both}"))
            .await
            .json["value"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let by_value = urlencode("Extensions/any(f:f/count eq 3)");
    assert_error(
        &graph
            .get(&format!("/v1.0/me/messages?$filter={by_value}"))
            .await,
        400,
        "ErrorInvalidUrlQueryFilter",
    );
    let listed = graph
        .get(&format!(
            "/v1.0/me/messages?$filter={}&$expand={}",
            urlencode(&format!("Extensions/any(f:f/id eq '{NAME}')")),
            urlencode(&format!("Extensions($filter=id eq '{NAME}')"))
        ))
        .await
        .json;
    metadata.assert_collection("message", &listed);
    assert_eq!(listed["value"][0]["extensions"][0]["extensionName"], NAME);

    assert_eq!(
        through_gmail(&graph, internet_id).await,
        gmail_before,
        "Gmail sees nothing of it"
    );

    assert_eq!(
        graph
            .request(
                Method::DELETE,
                &format!("/v1.0/me/messages/{id}/extensions/{FULL}"),
                &[],
                None
            )
            .await
            .status,
        204
    );
    assert_error(
        &graph
            .get(&format!("/v1.0/me/messages/{id}/extensions/{NAME}"))
            .await,
        404,
        "ErrorItemNotFound",
    );
    assert_error(
        &graph
            .request(
                Method::DELETE,
                &format!("/v1.0/me/messages/{id}/extensions/{NAME}"),
                &[],
                None,
            )
            .await,
        404,
        "ErrorItemNotFound",
    );
    assert!(marked(&graph, "/v1.0/me/messages", NAME).await.is_empty());
}

#[tokio::test]
async fn extensions_stay_with_their_message() {
    let graph = Graph::start().await;
    let message = graph.first("inbox", "isRead eq false").await;
    let id = message["id"].as_str().unwrap().to_owned();
    mark(&graph, &id).await;
    let immutable = graph
        .get_with(&format!("/v1.0/me/messages/{id}"), &[IMMUTABLE])
        .await
        .json["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let moved = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/move"),
            json!({ "destinationId": "archive" }),
        )
        .await
        .json;
    let moved_id = moved["id"].as_str().unwrap().to_owned();
    assert_ne!(moved_id, id);
    assert_eq!(
        graph
            .get(&format!("/v1.0/me/messages/{moved_id}/extensions/{NAME}"))
            .await
            .status,
        200,
        "a move keeps it"
    );
    assert_eq!(
        graph
            .get_with(
                &format!("/v1.0/me/messages/{immutable}/extensions/{NAME}"),
                &[IMMUTABLE]
            )
            .await
            .status,
        200
    );
    assert_eq!(
        marked(&graph, "/v1.0/me/mailFolders/archive/messages", NAME).await,
        [moved_id.as_str()]
    );
    assert_eq!(
        graph
            .get(&format!(
                "/v1.0/me/mailFolders/archive/messages/{moved_id}/extensions/{NAME}"
            ))
            .await
            .status,
        200
    );
    assert_eq!(
        graph
            .get(&format!(
                "/v1.0/me/mailFolders/inbox/messages/{moved_id}/extensions/{NAME}"
            ))
            .await
            .status,
        404
    );

    let copy = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{moved_id}/copy"),
            json!({ "destinationId": "inbox" }),
        )
        .await
        .json;
    let copy_id = copy["id"].as_str().unwrap();
    assert_eq!(
        graph
            .get(&format!("/v1.0/me/messages/{copy_id}/extensions/{NAME}"))
            .await
            .status,
        200,
        "a copy gets its own"
    );
    graph
        .request(
            Method::DELETE,
            &format!("/v1.0/me/messages/{copy_id}/extensions/{NAME}"),
            &[],
            None,
        )
        .await;
    assert_eq!(
        graph
            .get(&format!("/v1.0/me/messages/{moved_id}/extensions/{NAME}"))
            .await
            .status,
        200,
        "the original keeps its own"
    );

    let own = graph
        .get(&format!(
            "/v1.0/users/maya@brightmoor.co/messages/{moved_id}/extensions"
        ))
        .await;
    assert_eq!(own.status, 200, "{}", own.json);
    assert_eq!(own.json["value"].as_array().unwrap().len(), 1);
    let users_filter = urlencode(&format!("Extensions/any(f:f/id eq '{NAME}')"));
    let across = graph
        .get(&format!(
            "/v1.0/users/maya@brightmoor.co/messages?$filter={users_filter}"
        ))
        .await
        .json;
    assert_eq!(across["value"][0]["id"], moved_id.as_str());
    assert_error(
        &graph
            .get(&format!(
                "/v1.0/users/priya@brightmoor.co/messages/{moved_id}/extensions"
            ))
            .await,
        403,
        "ErrorAccessDenied",
    );

    // A deleted message takes its extensions with it.
    graph
        .request(
            Method::DELETE,
            &format!("/v1.0/me/messages/{moved_id}"),
            &[],
            None,
        )
        .await;
    assert!(marked(&graph, "/v1.0/me/messages", NAME).await.is_empty());
}

#[tokio::test]
async fn messages_made_with_extensions() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let drafts =
        || async { graph.get("/v1.0/me/mailFolders/drafts").await.json["totalItemCount"].clone() };
    let before = drafts().await;
    let mut bad = marker();
    bad["extensionName"] = "com.microsoft.mine".into();
    let refused = graph
        .json(
            Method::POST,
            "/v1.0/me/messages",
            json!({ "subject": "Annual review", "extensions": [bad] }),
        )
        .await;
    assert_error(&refused, 400, "ErrorInvalidRequest");
    assert_eq!(drafts().await, before, "a refused extension makes no draft");

    let created = graph
        .json(
            Method::POST,
            "/v1.0/me/messages",
            json!({
                "subject": "Annual review",
                "body": { "contentType": "HTML", "content": "You should be proud!" },
                "toRecipients": recipients(&["noor@brightmoor.co"]),
                "extensions": [marker()],
            }),
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.json);
    metadata.assert("message", &created.json);
    assert_eq!(
        created.json["extensions"][0]["id"], FULL,
        "it comes back expanded with its extension"
    );
    let id = created.json["id"].as_str().unwrap().to_owned();

    graph
        .json(
            Method::PATCH,
            &format!("/v1.0/me/messages/{id}"),
            json!({ "subject": "Annual review (final)" }),
        )
        .await;
    assert_eq!(
        graph
            .get(&format!("/v1.0/me/messages/{id}/extensions/{NAME}"))
            .await
            .status,
        200,
        "a draft edit keeps it"
    );
    assert_eq!(
        graph
            .request(
                Method::POST,
                &format!("/v1.0/me/messages/{id}/send"),
                &[],
                None
            )
            .await
            .status,
        202
    );
    let sent = marked(&graph, "/v1.0/me/mailFolders/sentitems/messages", NAME).await;
    assert_eq!(sent.len(), 1, "the sent message keeps it");
}
