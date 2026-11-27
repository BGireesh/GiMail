//! Folders, messages, deltas and per-message changes against mock-servers' Graph.

use crate::support;

use outlook::odata::RemovedReason;
use outlook::{
    FollowupFlag, FollowupFlagStatus, InferenceClassificationType, MailFolder, MessagePatch,
    OpenTypeExtension, well_known,
};
use support::{EMAIL, Mock};

#[tokio::test]
async fn folders_and_their_children() {
    let mock = Mock::start().await;
    let mail = mock.client.mail();
    let folders = mail.folders().all().await.unwrap().value;
    let names: Vec<&str> = folders
        .iter()
        .filter_map(|folder| folder.display_name.as_deref())
        .collect();
    for name in ["Inbox", "Drafts", "Sent Items", "Deleted Items", "Archive"] {
        assert!(names.contains(&name), "{name} in {names:?}");
    }
    let inbox = mail.folder(well_known::INBOX).await.unwrap();
    assert_eq!(inbox.display_name.as_deref(), Some("Inbox"));
    assert!(inbox.total_item_count.unwrap() > 0);

    let children = mail.child_folders(well_known::INBOX).await.unwrap().value;
    let mut names: Vec<&str> = children
        .iter()
        .filter_map(|folder| folder.display_name.as_deref())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["Newsletters", "Receipts"]);
    assert!(
        children
            .iter()
            .all(|child| child.parent_folder_id == inbox.id)
    );

    // The folder tree's delta reports a new folder, then its deletion.
    let round = mail.folders_delta().all().await.unwrap();
    let delta = round.delta_link.expect("a delta link");
    let made = mail
        .create_folder(
            Some(well_known::INBOX),
            &MailFolder {
                display_name: Some("Travel".into()),
                ..MailFolder::default()
            },
        )
        .await
        .unwrap();
    let changes = mock
        .client
        .follow::<MailFolder>(&delta)
        .all()
        .await
        .unwrap();
    assert!(
        changes
            .value
            .iter()
            .any(|folder| folder.id == made.id && folder.removed.is_none()),
        "{changes:?}"
    );
    mail.delete_folder(made.id.as_deref().unwrap())
        .await
        .unwrap();
    let after = mock
        .client
        .follow::<MailFolder>(&changes.delta_link.unwrap())
        .all()
        .await
        .unwrap();
    let removed = after
        .value
        .iter()
        .find(|folder| folder.id == made.id)
        .expect("the deletion");
    assert_eq!(
        removed.removed.as_ref().map(|removed| removed.reason),
        Some(RemovedReason::Deleted)
    );
}

#[tokio::test]
async fn a_folders_delta_pages_and_follows_changes() {
    let mock = Mock::start().await;
    let mail = mock.client.mail();
    // The first round lists the Inbox, five a page, and ends with a delta link.
    let first = mail
        .messages_delta(well_known::INBOX)
        .max_page_size(5)
        .await
        .unwrap();
    assert_eq!(first.value.len(), 5);
    assert!(first.next_link.is_some() && first.delta_link.is_none());
    let round = mail
        .messages_delta(well_known::INBOX)
        .select("subject,isRead,parentFolderId")
        .max_page_size(5)
        .all()
        .await
        .unwrap();
    let inbox = mail.folder(well_known::INBOX).await.unwrap();
    assert_eq!(
        round.value.len() as u32,
        inbox.total_item_count.unwrap(),
        "every message, across pages"
    );
    let delta = round.delta_link.expect("a delta link");

    // Nothing changed yet.
    let quiet = mock
        .client
        .follow::<outlook::Message>(&delta)
        .all()
        .await
        .unwrap();
    assert!(quiet.value.is_empty(), "{quiet:?}");

    // A delivery, a change and a move out come in the next round.
    mock.deliver("Ada Lovelace <ada@example.com>", "Engines")
        .await;
    let changed = round.value[0].id.clone().unwrap();
    mail.update_message(
        &changed,
        &MessagePatch {
            is_read: Some(true),
            ..MessagePatch::default()
        },
    )
    .await
    .unwrap();
    let moved = round.value[1].id.clone().unwrap();
    let after_move = mail
        .move_message(&moved, well_known::ARCHIVE)
        .await
        .unwrap();
    assert_eq!(
        after_move.id.as_deref(),
        Some(moved.as_str()),
        "immutable IDs stay"
    );

    let next = mock
        .client
        .follow::<outlook::Message>(&delta)
        .max_page_size(5)
        .all()
        .await
        .unwrap();
    let by_id = |id: &str| {
        next.value
            .iter()
            .find(|item| item.id.as_deref() == Some(id))
    };
    assert!(
        next.value
            .iter()
            .any(|item| item.subject.as_deref() == Some("Engines")),
        "{next:?}"
    );
    assert_eq!(by_id(&changed).and_then(|item| item.is_read), Some(true));
    assert_eq!(
        by_id(&moved)
            .and_then(|item| item.removed.as_ref())
            .map(|removed| removed.reason),
        Some(RemovedReason::Deleted)
    );
    assert!(next.delta_link.is_some());
}

#[tokio::test]
async fn an_expired_delta_link_is_sync_state_not_found() {
    let mock = Mock::start().await;
    let round = mock
        .client
        .mail()
        .messages_delta(well_known::INBOX)
        .all()
        .await
        .unwrap();
    // Graph forgets the sync state behind a link after a while.
    mock.deliver("Ada Lovelace <ada@example.com>", "Later")
        .await;
    mock.inbox_message("Later").await;
    mock.admin("/__mock/history/expire", serde_json::json!({}))
        .await;
    let error = mock
        .client
        .follow::<outlook::Message>(&round.delta_link.unwrap())
        .await
        .unwrap_err();
    assert_eq!(error.status(), Some(410), "{error}");
    assert_eq!(error.code(), Some("SyncStateNotFound"));
}

#[tokio::test]
async fn a_conversation_and_its_mime() {
    let mock = Mock::start().await;
    let mail = mock.client.mail();
    let messages = mail
        .messages()
        .select("conversationId,subject")
        .top(50)
        .await
        .unwrap()
        .value;
    let conversation = messages[0].conversation_id.clone().unwrap();
    let thread = mail.conversation(&conversation).all().await.unwrap().value;
    assert!(!thread.is_empty());
    assert!(
        thread
            .iter()
            .all(|message| message.conversation_id.as_deref() == Some(conversation.as_str()))
    );
    assert!(
        thread
            .windows(2)
            .all(|pair| pair[0].received_date_time <= pair[1].received_date_time),
        "oldest first"
    );

    let mime = mail.mime(thread[0].id.as_deref().unwrap()).await.unwrap();
    let text = String::from_utf8_lossy(&mime.data);
    assert!(text.contains("Subject:"), "{text}");
    assert!(text.contains("Message-ID:") || text.contains("Message-Id:"));
}

#[tokio::test]
async fn read_flag_categories_and_focus() {
    let mock = Mock::start().await;
    mock.deliver("Ada Lovelace <ada@example.com>", "Notes")
        .await;
    let message = mock.inbox_message("Notes").await;
    let id = message.id.unwrap();
    assert_eq!(message.is_read, Some(false));
    let mail = mock.client.mail();
    let updated = mail
        .update_message(
            &id,
            &MessagePatch {
                is_read: Some(true),
                flag: Some(FollowupFlag::new(FollowupFlagStatus::Flagged)),
                categories: Some(vec!["Projects".into()]),
                inference_classification: Some(InferenceClassificationType::Other),
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.is_read, Some(true));
    assert_eq!(
        updated.flag.and_then(|flag| flag.flag_status),
        Some(FollowupFlagStatus::Flagged)
    );
    assert_eq!(updated.categories, ["Projects"]);
    assert_eq!(
        updated.inference_classification,
        Some(InferenceClassificationType::Other)
    );

    // Clearing categories sends an empty list.
    let cleared = mail
        .update_message(
            &id,
            &MessagePatch {
                categories: Some(Vec::new()),
                ..MessagePatch::default()
            },
        )
        .await
        .unwrap();
    assert!(cleared.categories.is_empty());
}

#[tokio::test]
async fn move_copy_and_delete() {
    let mock = Mock::start().await;
    mock.deliver("Ada Lovelace <ada@example.com>", "Keep this")
        .await;
    let id = mock.inbox_message("Keep this").await.id.unwrap();
    let mail = mock.client.mail();
    let archive = mail.folder(well_known::ARCHIVE).await.unwrap().id;

    let moved = mail.move_message(&id, well_known::ARCHIVE).await.unwrap();
    assert_eq!(moved.id.as_deref(), Some(id.as_str()));
    assert_eq!(moved.parent_folder_id, archive);

    let copy = mail.copy_message(&id, well_known::INBOX).await.unwrap();
    assert_ne!(
        copy.id.as_deref(),
        Some(id.as_str()),
        "a copy is its own item"
    );
    assert_eq!(
        mail.message(copy.id.as_deref().unwrap())
            .await
            .unwrap()
            .subject
            .as_deref(),
        Some("Keep this")
    );

    mail.delete_message(&id).await.unwrap();
    let error = mail.message(&id).await.unwrap_err();
    assert!(error.is_not_found(), "{error}");
    assert_eq!(error.code(), Some("ErrorItemNotFound"));
}

#[tokio::test]
async fn open_extensions_mark_messages() {
    const NAME: &str = "dev.exo.test";
    let mock = Mock::start().await;
    mock.deliver("Ada Lovelace <ada@example.com>", "Marked")
        .await;
    let id = mock.inbox_message("Marked").await.id.unwrap();
    let mail = mock.client.mail();
    let mut data = serde_json::Map::new();
    data.insert("until".into(), "2026-11-20T09:00:00Z".into());
    let made = mail
        .create_extension(&id, &OpenTypeExtension::new(NAME, data))
        .await
        .unwrap();
    assert_eq!(made.extension_name.as_deref(), Some(NAME));
    assert_eq!(made.data["until"], "2026-11-20T09:00:00Z");

    // Found across folders, and expanded with the message.
    mail.move_message(&id, well_known::ARCHIVE).await.unwrap();
    let found = mail
        .messages()
        .filter(&OpenTypeExtension::filter(NAME))
        .expand(&OpenTypeExtension::expand(NAME))
        .await
        .unwrap()
        .value;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id.as_deref(), Some(id.as_str()));
    assert_eq!(found[0].extensions[0].data["until"], "2026-11-20T09:00:00Z");

    let mut later = serde_json::Map::new();
    later.insert("until".into(), "2026-11-21T09:00:00Z".into());
    let updated = mail
        .update_extension(&id, NAME, &OpenTypeExtension::new(NAME, later))
        .await
        .unwrap();
    assert_eq!(updated.data["until"], "2026-11-21T09:00:00Z");
    assert_eq!(
        mail.extensions(&id).await.unwrap().value.len(),
        1,
        "one mark"
    );

    let again = mail
        .create_extension(&id, &OpenTypeExtension::new(NAME, serde_json::Map::new()))
        .await
        .unwrap_err();
    assert_eq!(again.status(), Some(409), "{again}");

    mail.delete_extension(&id, NAME).await.unwrap();
    assert!(mail.extension(&id, NAME).await.unwrap_err().is_not_found());
    assert!(
        mail.messages()
            .filter(&OpenTypeExtension::filter(NAME))
            .await
            .unwrap()
            .value
            .is_empty()
    );
}

#[tokio::test]
async fn graph_errors_are_typed() {
    let mock = Mock::start().await;
    let error = mock.client.mail().message("nope").await.unwrap_err();
    let outlook::Error::Api(api) = &error else {
        panic!("{error:?}");
    };
    assert_eq!(api.status, 400);
    assert_eq!(api.error.code, "ErrorInvalidIdMalformed");
    let inner = api.error.inner_error.as_ref().expect("innerError");
    assert!(inner.request_id.is_some());
    assert_eq!(api.retry_after, None);

    // `/me` for the mailbox's owner.
    let me = mock.client.people().me().await.unwrap();
    assert_eq!(me.address(), Some(EMAIL));
}

/// The properties of `raw` that `T` leaves unread, as `path.property`.
fn unread<T: serde::de::DeserializeOwned>(raw: &serde_json::Value) -> Vec<String> {
    let mut ignored = Vec::new();
    let _: T = serde_ignored::deserialize(raw, |path| ignored.push(path.to_string())).unwrap();
    ignored
}

#[tokio::test]
async fn the_models_read_the_properties_they_name() {
    // A misspelled property would read as absent and leave Graph's own unread.
    let mock = Mock::start().await;
    let raw = |path: &'static str| {
        let url = mock.server.url().join(path).unwrap();
        async move {
            let response = reqwest::Client::new()
                .get(url)
                .bearer_auth(mock_servers::DEV_ACCESS_TOKEN)
                .header("Prefer", "IdType=\"ImmutableId\"")
                .send()
                .await
                .unwrap();
            let json: serde_json::Value = response.json().await.unwrap();
            json["value"][0].clone()
        }
    };
    let invite = raw("/v1.0/me/mailFolders/inbox/messages?$search=%22subject:Invitation%22&$top=1&$expand=attachments").await;
    let mut skipped = unread::<outlook::Message>(&invite);
    skipped.sort();
    assert_eq!(
        skipped,
        [
            "@odata.etag",
            "attachments.0.@odata.mediaContentType",
            "attachments.0.contentLocation",
        ]
    );
    let folder = raw("/v1.0/me/mailFolders?$top=1").await;
    assert!(unread::<MailFolder>(&folder).is_empty());
    let user = raw("/v1.0/users?$top=1").await;
    let mut skipped = unread::<outlook::people::User>(&user);
    skipped.sort();
    assert_eq!(skipped, ["officeLocation", "preferredLanguage"]);
}
