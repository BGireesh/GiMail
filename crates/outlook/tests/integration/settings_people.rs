//! Mail settings, people, contacts and photos against mock-servers' Graph.

use crate::support;

use mock_servers::Config;
use outlook::settings::{
    AutomaticRepliesSetting, AutomaticRepliesStatus, CategoryColor,
    InferenceClassificationOverride, MailboxSettings, MessageRule, MessageRuleActions,
    MessageRulePredicates, OutlookCategory,
};
use outlook::{EmailAddress, InferenceClassificationType, Recipient};
use serde_json::json;
use support::{EMAIL, Mock};

#[tokio::test]
async fn categories_are_the_master_list() {
    let mock = Mock::start().await;
    let settings = mock.client.settings();
    let categories = settings.categories().await.unwrap().value;
    let projects = categories
        .iter()
        .find(|category| category.display_name.as_deref() == Some("Projects"))
        .expect("the fixtures' Projects label");
    assert!(matches!(projects.color, Some(CategoryColor::Preset(_))));

    let made = settings
        .create_category(&OutlookCategory {
            display_name: Some("Errands".into()),
            color: Some(CategoryColor::Preset(3)),
            ..OutlookCategory::default()
        })
        .await
        .unwrap();
    assert_eq!(made.display_name.as_deref(), Some("Errands"));
    let id = made.id.unwrap();
    let recolored = settings
        .update_category(
            &id,
            &OutlookCategory {
                color: Some(CategoryColor::Preset(7)),
                ..OutlookCategory::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(recolored.color, Some(CategoryColor::Preset(7)));
    settings.delete_category(&id).await.unwrap();
    assert!(
        !settings
            .categories()
            .await
            .unwrap()
            .value
            .iter()
            .any(|category| category.id.as_deref() == Some(id.as_str()))
    );
}

#[tokio::test]
async fn automatic_replies_are_mailbox_settings() {
    let mock = Mock::start().await;
    let settings = mock.client.settings();
    let current = settings.mailbox_settings().await.unwrap();
    assert_eq!(current.time_zone.as_deref(), Some("Eastern Standard Time"));
    assert!(current.archive_folder.is_some());
    assert_eq!(
        current
            .working_hours
            .as_ref()
            .and_then(|hours| hours.time_zone.as_ref())
            .and_then(|tz| tz.name.as_deref()),
        Some("Eastern Standard Time")
    );
    let replies = current.automatic_replies_setting.unwrap();
    assert_eq!(replies.status, Some(AutomaticRepliesStatus::Disabled));
    assert!(replies.scheduled_start_date_time.is_some());

    let updated = settings
        .update_mailbox_settings(&MailboxSettings {
            automatic_replies_setting: Some(AutomaticRepliesSetting {
                status: Some(AutomaticRepliesStatus::AlwaysEnabled),
                internal_reply_message: Some("<p>Away until Monday.</p>".into()),
                ..AutomaticRepliesSetting::default()
            }),
            ..MailboxSettings::default()
        })
        .await
        .unwrap();
    assert_eq!(
        updated
            .automatic_replies_setting
            .and_then(|replies| replies.status),
        Some(AutomaticRepliesStatus::AlwaysEnabled)
    );
    let replies = settings
        .mailbox_settings()
        .await
        .unwrap()
        .automatic_replies_setting
        .unwrap();
    assert_eq!(replies.status, Some(AutomaticRepliesStatus::AlwaysEnabled));
    assert!(
        replies
            .internal_reply_message
            .unwrap()
            .contains("Away until Monday.")
    );
}

#[tokio::test]
async fn inbox_rules_and_focused_overrides() {
    let mock = Mock::start().await;
    let settings = mock.client.settings();
    let rules = settings.rules().await.unwrap().value;
    assert!(
        rules.iter().any(|rule| rule
            .conditions
            .as_ref()
            .is_some_and(|conditions| !conditions.from_addresses.is_empty())),
        "{rules:?}"
    );

    let made = settings
        .create_rule(&MessageRule {
            display_name: Some("Receipts from the shop".into()),
            sequence: Some(10),
            is_enabled: Some(true),
            conditions: Some(MessageRulePredicates {
                from_addresses: vec![Recipient::new("shop@example.com", None)],
                ..MessageRulePredicates::default()
            }),
            actions: Some(MessageRuleActions {
                mark_as_read: Some(true),
                stop_processing_rules: Some(false),
                ..MessageRuleActions::default()
            }),
            ..MessageRule::default()
        })
        .await
        .unwrap();
    let id = made.id.unwrap();
    let disabled = settings
        .update_rule(
            &id,
            &MessageRule {
                is_enabled: Some(false),
                ..MessageRule::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(disabled.is_enabled, Some(false));
    settings.delete_rule(&id).await.unwrap();

    let override_ = settings
        .create_override(&InferenceClassificationOverride {
            classify_as: Some(InferenceClassificationType::Other),
            sender_email_address: Some(EmailAddress {
                name: Some("Shop".into()),
                address: Some("shop@example.com".into()),
            }),
            ..InferenceClassificationOverride::default()
        })
        .await
        .unwrap();
    let id = override_.id.unwrap();
    let focused = settings
        .update_override(&id, InferenceClassificationType::Focused)
        .await
        .unwrap();
    assert_eq!(
        focused.classify_as,
        Some(InferenceClassificationType::Focused)
    );
    assert_eq!(settings.overrides().await.unwrap().value.len(), 1);
    settings.delete_override(&id).await.unwrap();
    assert!(settings.overrides().await.unwrap().value.is_empty());
}

#[tokio::test]
async fn people_users_contacts_and_photos() {
    let mock = Mock::with(Config {
        photo: true,
        contact_photos: true,
        ..Config::default()
    })
    .await;
    let people = mock.client.people();
    let relevant = people.relevant().top(5).await.unwrap().value;
    assert!(!relevant.is_empty());
    assert!(relevant[0].scored_email_addresses[0].address.is_some());

    let users = people.users().all().await.unwrap().value;
    assert!(users.iter().any(|user| user.address() == Some(EMAIL)));
    let colleague = users
        .iter()
        .find(|user| user.address() != Some(EMAIL))
        .unwrap();
    let by_id = people.user(colleague.id.as_deref().unwrap()).await.unwrap();
    assert_eq!(by_id.display_name, colleague.display_name);
    let photo = people
        .user_photo_content(colleague.id.as_deref().unwrap())
        .await
        .unwrap();
    assert!(photo.content_type.unwrap().starts_with("image/"));

    let mine = people.photo_content().await.unwrap();
    assert!(!mine.data.is_empty());
    assert!(people.photo().await.unwrap().width.is_some());

    // The contacts' delta reports a removal in its next round.
    let round = people.contacts_delta().all().await.unwrap();
    let contacts = people.contacts().all().await.unwrap().value;
    assert_eq!(round.value.len(), contacts.len());
    let gone = &contacts[0];
    let address = gone.email_addresses[0].address.clone().unwrap();
    assert_eq!(
        people
            .contact(gone.id.as_deref().unwrap())
            .await
            .unwrap()
            .id,
        gone.id
    );
    mock.admin(
        "/__mock/people",
        json!({ "list": "contacts", "remove": address }),
    )
    .await;
    let next = mock
        .client
        .follow::<outlook::people::Contact>(&round.delta_link.unwrap())
        .all()
        .await
        .unwrap();
    let removed = next
        .value
        .iter()
        .find(|contact| contact.id == gone.id)
        .expect("the removal");
    assert!(removed.removed.is_some());

    let users = people.users_delta().all().await.unwrap();
    assert!(users.delta_link.is_some());
}

#[tokio::test]
async fn photos_that_dont_exist_are_not_found() {
    let mock = Mock::start().await;
    let error = mock.client.people().photo_content().await.unwrap_err();
    assert!(error.is_not_found(), "{error}");
    assert_eq!(error.code(), Some("ImageNotFound"));
}
