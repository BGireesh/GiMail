//! Graph's messages as the rows the store keeps, and the versions that order them.
//!
//! Graph has no history id. A message's version is when it last changed (`lastModifiedDateTime`,
//! to the second), and within one second, the order this session read it in. A message whose
//! state changed twice in a second can only have been read in its first state during that second,
//! so its later state, read later, still comes out newer.

use std::sync::atomic::{AtomicU32, Ordering};

use gmail::mime::strip_angles;
use jiff::Timestamp;
use outlook::{Message, Recipient};
use toasty::Json;

use super::mailbox::{Mailbox, markers_expand};
use crate::db::{AttachmentRow, BodyRow, Mailbox as Address, MessageRow};
use crate::model::Header;
use crate::provider::Parsed;
use crate::provider::gmail::parse::{default_name, shown_content_ids};
use crate::sync::ROW_HEADERS;

/// The properties a message's row is read from.
macro_rules! row {
    () => {
        "id,conversationId,parentFolderId,createdDateTime,lastModifiedDateTime,receivedDateTime,\
         sentDateTime,isRead,isDraft,flag,categories,inferenceClassification,subject,bodyPreview,\
         from,sender,toRecipients,ccRecipients,bccRecipients,replyTo,internetMessageId,\
         internetMessageHeaders,hasAttachments"
    };
}

/// What a message's row is read from.
pub(super) const ROW: &str = row!();
/// What a message is read from in full: its row and its body.
pub(super) const FULL: &str = concat!(row!(), ",body");
/// What a message's labels are read from.
pub(super) const LABELS: &str = "id,conversationId,parentFolderId,lastModifiedDateTime,\
receivedDateTime,isRead,isDraft,flag,categories,inferenceClassification";

/// The `$expand` reading a message: Exo's markers, and in full its attachments, without their
/// bytes.
pub(super) fn expand(full: bool) -> String {
    if full {
        format!(
            "attachments($select=id,name,contentType,size,isInline,\
             microsoft.graph.fileAttachment/contentId),{}",
            markers_expand()
        )
    } else {
        markers_expand()
    }
}

/// Mints the versions of what this session reads.
#[derive(Debug, Default)]
pub(crate) struct Versions(AtomicU32);

impl Versions {
    /// The version of a message read now that last changed at `modified`: above what a listing
    /// has for it, by how many reads came before this one.
    pub fn read(&self, modified: Option<Timestamp>) -> u64 {
        let read = self.0.fetch_add(1, Ordering::Relaxed) % 0x7fff_ffff + 1;
        listed(modified) | u64::from(read)
    }
}

/// The version of a message that last changed at `modified` as a listing knows it: older than
/// any read of it since it changed, so a thread read since isn't fetched again.
pub(super) fn listed(modified: Option<Timestamp>) -> u64 {
    let second = modified
        .map_or(0, |at| at.as_second())
        .clamp(0, 0xffff_ffff);
    u64::try_from(second).unwrap_or_default() << 31
}

/// `message`'s rows, in `folder` when the message doesn't say, at `version`. When not `full`, its
/// row alone. `None` without an id or a conversation.
pub(super) fn parse(
    message: &Message,
    full: bool,
    mailbox: &Mailbox,
    folder: &str,
    version: u64,
) -> Option<Parsed> {
    let id = message.id.clone()?;
    let thread_id = message.conversation_id.clone()?;
    let header = |name: &str| {
        message
            .internet_message_headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.clone())
    };
    let from = message.from.as_ref().or(message.sender.as_ref());
    let sender = from.and_then(address);
    let addresses = |recipients: &[Recipient]| -> Vec<Address> {
        recipients.iter().filter_map(address).collect()
    };
    let headers: Vec<Header> = if message.internet_message_headers.is_empty() {
        // Mail Outlook made itself (drafts, what it sent) comes without its headers.
        written_headers(message, sender.as_ref())
    } else {
        message
            .internet_message_headers
            .iter()
            .filter(|header| {
                full || ROW_HEADERS
                    .iter()
                    .any(|row| row.eq_ignore_ascii_case(&header.name))
            })
            .map(|header| Header {
                name: header.name.clone(),
                value: header.value.clone(),
            })
            .collect()
    };
    let body = message.body.as_ref().filter(|_| full);
    let html = body
        .filter(|body| body.content_type != Some(outlook::BodyType::Text))
        .and_then(|body| body.content.clone());
    let text = body
        .filter(|body| body.content_type == Some(outlook::BodyType::Text))
        .and_then(|body| body.content.clone());
    let shown = shown_content_ids(html.as_slice());
    let attachments: Vec<AttachmentRow> = message
        .attachments
        .iter()
        .filter(|_| full)
        // Only file attachments have downloadable `/$value` bytes; reference and item attachments
        // don't.
        .filter(|attachment| attachment.odata_type.is_none() || attachment.is_file())
        .filter_map(|attachment| {
            let part = attachment.id.clone()?;
            let mime_type = attachment
                .content_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".into());
            let content_id = attachment.content_id.as_deref().map(strip_angles);
            Some(AttachmentRow {
                message_id: id.clone(),
                part_id: part.clone(),
                attachment_id: Some(part),
                filename: attachment
                    .name
                    .clone()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| default_name(&mime_type)),
                mime_type,
                size: attachment
                    .size
                    .and_then(|size| u32::try_from(size).ok())
                    .unwrap_or_default(),
                // As for Gmail: a file is inline when the body shows it, whatever it says.
                inline: content_id
                    .as_deref()
                    .is_some_and(|cid| shown.contains(&cid.to_ascii_lowercase())),
                content_id,
            })
        })
        .collect();
    let invite = message.odata_type.as_deref() == Some("#microsoft.graph.eventMessageRequest");
    let has_attachments = invite
        || if full {
            attachments.iter().any(|attachment| !attachment.inline)
        } else {
            message.has_attachments == Some(true)
        };
    let date = message
        .received_date_time
        .or(message.sent_date_time)
        .or(message.created_date_time)
        .map_or(0, Timestamp::as_millisecond);
    let size = html.as_ref().or(text.as_ref()).map_or(0, String::len)
        + attachments
            .iter()
            .map(|attachment| attachment.size as usize)
            .sum::<usize>();
    Some(Parsed {
        message: MessageRow {
            id: id.clone(),
            thread_id,
            version,
            internal_date: date,
            label_ids: mailbox.labels(message, folder),
            subject: message.subject.clone().unwrap_or_default(),
            snippet: snippet(message.body_preview.as_deref().unwrap_or_default()),
            sender_raw: header("From")
                .or_else(|| sender.as_ref().map(formatted))
                .unwrap_or_default(),
            sender: Json(sender),
            to: Json(addresses(&message.to_recipients)),
            cc: Json(addresses(&message.cc_recipients)),
            bcc: Json(addresses(&message.bcc_recipients)),
            reply_to: Json(addresses(&message.reply_to)),
            rfc822_id: message.internet_message_id.as_deref().map(strip_angles),
            in_reply_to: header("In-Reply-To").as_deref().map(strip_angles),
            references: header("References")
                .map(|value| value.split_whitespace().map(strip_angles).collect())
                .unwrap_or_default(),
            list_id: header("List-Id"),
            list_unsubscribe: header("List-Unsubscribe"),
            list_unsubscribe_post: header("List-Unsubscribe-Post"),
            headers: Json(headers),
            size: u32::try_from(size).unwrap_or(u32::MAX),
            has_attachments,
        },
        attachments,
        body: full.then_some(BodyRow {
            message_id: id,
            html,
            text,
            visible: None,
        }),
    })
}

/// Whether a message read for its row also needs reading in full for its row to be right: it
/// holds files (or is an invite), which the row's icon and searches by file need.
pub(super) fn needs_structure(message: &Message) -> bool {
    message.has_attachments == Some(true)
        || message.odata_type.as_deref() == Some("#microsoft.graph.eventMessageRequest")
}

/// The address `recipient` names.
fn address(recipient: &Recipient) -> Option<Address> {
    let email = recipient.email_address.as_ref()?;
    let address = email
        .address
        .clone()
        .filter(|address| !address.is_empty())?;
    Some(Address {
        name: email
            .name
            .clone()
            .filter(|name| *name != address && !name.is_empty()),
        email: address,
    })
}

/// `address` as a header writes it.
fn formatted(address: &Address) -> String {
    match &address.name {
        Some(name) => format!("{name} <{}>", address.email),
        None => address.email.clone(),
    }
}

/// The headers of a message Outlook gave none for, from what it says of itself.
fn written_headers(message: &Message, from: Option<&Address>) -> Vec<Header> {
    let list = |recipients: &[Recipient]| {
        let addresses: Vec<String> = recipients
            .iter()
            .filter_map(address)
            .map(|address| formatted(&address))
            .collect();
        (!addresses.is_empty()).then(|| addresses.join(", "))
    };
    [
        ("From", from.map(formatted)),
        ("To", list(&message.to_recipients)),
        ("Cc", list(&message.cc_recipients)),
        ("Reply-To", list(&message.reply_to)),
        ("Subject", message.subject.clone()),
        ("Message-ID", message.internet_message_id.clone()),
    ]
    .into_iter()
    .filter_map(|(name, value)| {
        Some(Header {
            name: name.to_owned(),
            value: value?,
        })
    })
    .collect()
}

/// Graph's preview of a body as the store keeps snippets: on one line, and HTML-escaped as
/// Gmail's are, since readers decode them.
fn snippet(preview: &str) -> String {
    let line = preview.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut escaped = String::with_capacity(line.len());
    for ch in line.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            ch => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::unescape_html;

    #[test]
    fn previews_are_kept_as_snippets_are() {
        let preview = "Tom & Jerry's  <plans>\r\n for \"Q3\"";
        let stored = snippet(preview);
        assert_eq!(
            stored,
            "Tom &amp; Jerry&#39;s &lt;plans&gt; for &quot;Q3&quot;"
        );
        assert_eq!(unescape_html(&stored), "Tom & Jerry's <plans> for \"Q3\"");
    }

    #[test]
    fn versions_order_by_change_then_by_reading() {
        let versions = Versions::default();
        let at = |second| Some(Timestamp::from_second(second).unwrap());
        let first = versions.read(at(1_800_000_000));
        let second = versions.read(at(1_800_000_000));
        assert!(second > first, "the same second, read later");
        assert!(versions.read(at(1_800_000_001)) > second);
        assert!(
            listed(at(1_800_000_000)) < first,
            "a listing is older than any read"
        );
        assert!(
            listed(at(1_800_000_001)) > second,
            "but newer than reads before the change"
        );
        assert!(
            versions.read(at(1_800_000_001)) < i64::MAX as u64,
            "SQLite stores it"
        );
    }

    #[test]
    fn parse_keeps_file_attachments_and_skips_reference_and_item_attachments() {
        let mailbox = Mailbox::default();
        let message = Message {
            id: Some("m1".into()),
            conversation_id: Some("c1".into()),
            parent_folder_id: Some("in".into()),
            attachments: vec![
                outlook::Attachment {
                    odata_type: Some(outlook::Attachment::FILE.into()),
                    id: Some("a1".into()),
                    name: Some("doc.pdf".into()),
                    content_type: Some("application/pdf".into()),
                    size: Some(42),
                    ..outlook::Attachment::default()
                },
                outlook::Attachment {
                    odata_type: Some("#microsoft.graph.referenceAttachment".into()),
                    id: Some("a2".into()),
                    name: Some("cloud.docx".into()),
                    ..outlook::Attachment::default()
                },
                outlook::Attachment {
                    odata_type: Some("#microsoft.graph.itemAttachment".into()),
                    id: Some("a3".into()),
                    name: Some("forwarded.eml".into()),
                    ..outlook::Attachment::default()
                },
            ],
            ..Message::default()
        };
        let parsed = parse(&message, true, &mailbox, "in", 1).expect("parses");
        let ids: Vec<&str> = parsed
            .attachments
            .iter()
            .map(|a| a.part_id.as_str())
            .collect();
        assert_eq!(ids, ["a1"]);
    }
}
