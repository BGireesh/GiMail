//! Drafts, replies and forwards, sending, attachments and upload sessions against mock-servers'
//! Graph.

use crate::support;

use outlook::{
    Attachment, AttachmentItem, BodyType, ItemBody, Message, Recipient, ReplyRequest, SendMail,
    well_known,
};
use support::Mock;

fn draft(subject: &str) -> Message {
    Message {
        subject: Some(subject.into()),
        body: Some(ItemBody {
            content_type: Some(BodyType::Text),
            content: Some("Hi Ada".into()),
        }),
        to_recipients: vec![Recipient::new("ada@example.com", Some("Ada Lovelace"))],
        ..Message::default()
    }
}

/// The Sent Items message with `subject`.
async fn sent(mock: &Mock, subject: &str) -> Option<Message> {
    let page = mock
        .client
        .mail()
        .folder_messages(well_known::SENT_ITEMS)
        .filter(&format!("subject eq {}", outlook::odata::quote(subject)))
        .await
        .unwrap();
    page.value.into_iter().next()
}

#[tokio::test]
async fn json_drafts_are_edited_and_sent() {
    let mock = Mock::start().await;
    let mail = mock.client.mail();
    let made = mail.create_draft(&draft("Plans")).await.unwrap();
    assert_eq!(made.is_draft, Some(true));
    let drafts = mail.folder(well_known::DRAFTS).await.unwrap().id;
    assert_eq!(made.parent_folder_id, drafts);
    let id = made.id.unwrap();

    let edited = mail
        .update_draft(
            &id,
            &Message {
                subject: Some("Plans for Monday".into()),
                ..Message::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(edited.subject.as_deref(), Some("Plans for Monday"));
    assert_eq!(edited.to_recipients.len(), 1, "the rest stays");

    mail.send_draft(&id).await.unwrap();
    let copy = sent(&mock, "Plans for Monday")
        .await
        .expect("a Sent Items copy");
    assert_eq!(
        copy.id.as_deref(),
        Some(id.as_str()),
        "it keeps its immutable ID"
    );
    assert_eq!(copy.is_draft, Some(false));
}

#[tokio::test]
async fn mime_drafts_and_mime_sends() {
    let mock = Mock::start().await;
    let mail = mock.client.mail();
    let mime = b"From: Maya Okafor <maya@brightmoor.co>\r\nTo: Ada Lovelace <ada@example.com>\r\nSubject: From MIME\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nWritten elsewhere.\r\n";
    let made = mail.create_draft_mime(mime.to_vec()).await.unwrap();
    assert_eq!(made.subject.as_deref(), Some("From MIME"));
    assert_eq!(made.is_draft, Some(true));
    let back = mail.mime(made.id.as_deref().unwrap()).await.unwrap();
    assert!(String::from_utf8_lossy(&back.data).contains("Written elsewhere."));

    let sent_mime = b"To: Ada Lovelace <ada@example.com>\r\nSubject: Sent as MIME\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nStraight out.\r\n";
    mail.send_mime(sent_mime.to_vec()).await.unwrap();
    assert!(sent(&mock, "Sent as MIME").await.is_some());

    mail.send_mail(&SendMail {
        message: draft("Sent as JSON"),
        save_to_sent_items: Some(false),
    })
    .await
    .unwrap();
    assert!(
        sent(&mock, "Sent as JSON").await.is_none(),
        "saveToSentItems false leaves no copy"
    );
    let error = mail
        .send_mail(&SendMail {
            message: Message {
                subject: Some("Nobody".into()),
                ..Message::default()
            },
            save_to_sent_items: None,
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some("ErrorInvalidRecipients"), "{error}");
}

#[tokio::test]
async fn replies_and_forwards_are_drafts_in_the_conversation() {
    let mock = Mock::start().await;
    mock.deliver("Ada Lovelace <ada@example.com>", "Engines")
        .await;
    let original = mock.inbox_message("Engines").await;
    let mail = mock.client.mail();
    let reply = mail
        .create_reply(
            original.id.as_deref().unwrap(),
            &ReplyRequest {
                comment: Some("Sounds good.".into()),
                ..ReplyRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(reply.is_draft, Some(true));
    assert_eq!(reply.conversation_id, original.conversation_id);
    assert_eq!(reply.subject.as_deref(), Some("RE: Engines"));
    assert_eq!(
        reply.to_recipients[0]
            .email_address
            .as_ref()
            .and_then(|address| address.address.as_deref()),
        Some("ada@example.com")
    );

    let forward = mail
        .create_forward(
            original.id.as_deref().unwrap(),
            &ReplyRequest {
                to_recipients: vec![Recipient::new("sam@brightmoor.co", None)],
                ..ReplyRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(forward.subject.as_deref(), Some("FW: Engines"));
    let all = mail
        .create_reply_all(original.id.as_deref().unwrap(), &ReplyRequest::default())
        .await
        .unwrap();
    assert_eq!(all.conversation_id, original.conversation_id);
}

#[tokio::test]
async fn small_attachments_post_and_large_ones_upload() {
    let mock = Mock::start().await;
    let mail = mock.client.mail();
    let id = mail
        .create_draft(&draft("With files"))
        .await
        .unwrap()
        .id
        .unwrap();

    let small = mail
        .add_attachment(
            &id,
            &Attachment::file("notes.txt", "text/plain", b"notes".to_vec()),
        )
        .await
        .unwrap();
    assert!(small.is_file());
    assert_eq!(small.size.map(|size| size > 0), Some(true));
    let content = mail
        .attachment_content(&id, small.id.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(content.data, b"notes");
    let listed = mail.attachments(&id).await.unwrap().value;
    assert_eq!(listed.len(), 1);
    let read = mail
        .attachment(&id, small.id.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(read.content_bytes.as_deref(), Some(&b"notes"[..]));

    // 7 MB goes up in three ranges.
    let large: Vec<u8> = (0..7_000_000_u32).map(|n| (n % 251) as u8).collect();
    let session = mail
        .create_upload_session(
            &id,
            &AttachmentItem::file("deck.bin", "application/octet-stream", large.len() as u64),
        )
        .await
        .unwrap();
    assert_eq!(session.next_expected_ranges, ["0-"]);
    mail.upload(&session, &large).await.unwrap();
    let listed = mail.attachments(&id).await.unwrap().value;
    let uploaded = listed
        .iter()
        .find(|attachment| attachment.name.as_deref() == Some("deck.bin"))
        .expect("the uploaded file");
    assert_eq!(uploaded.size, Some(large.len() as u64));
    let back = mail
        .attachment_content(&id, uploaded.id.as_deref().unwrap())
        .await
        .unwrap();
    assert!(back.data == large, "the bytes arrive whole and in order");

    // An upload session refuses what's small enough to post.
    let error = mail
        .create_upload_session(&id, &AttachmentItem::file("tiny.txt", "text/plain", 10))
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        Some("ErrorAttachmentSizeShouldNotBeLessThanMinimumSize")
    );

    mail.delete_attachment(&id, small.id.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(mail.attachments(&id).await.unwrap().value.len(), 1);
}
