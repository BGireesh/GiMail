//! Turning Gmail's messages into the rows the store keeps.

use std::collections::HashSet;

use gmail::mime::strip_angles;
use gmail::{Message, MessagePart};
use toasty::Json;

use crate::db::{AttachmentRow, BodyRow, Mailbox, MessageRow};
use crate::ids::labels::CHAT;
use crate::model::Header;
use crate::provider::Parsed;

/// Flattens a message fetched with `format=full`. When not `full`, the message was fetched with
/// `format=metadata`, and this returns its row alone, built from the headers Gmail sent, with no
/// body or attachments. `None` for a chat message (Exo ignores chats), or for one without an id
/// or thread id.
pub(crate) fn parse(message: &Message, full: bool) -> Option<Parsed> {
    if message.label_ids.iter().any(|label| label == CHAT) {
        return None;
    }
    let id = message.id.clone()?;
    let thread_id = message.thread_id.clone()?;
    let empty = MessagePart::default();
    let payload = message.payload.as_ref().unwrap_or(&empty);
    let header = |name: &str| payload.header(name).map(str::to_owned);
    let mailboxes = |name: &str| {
        payload
            .header(name)
            .map(Mailbox::parse_list)
            .unwrap_or_default()
    };

    let mut attachments = Vec::new();
    let mut calendar = false;
    let (mut html, mut text) = (Vec::new(), Vec::new());
    // A message fetched for its row comes with headers alone, so there are no parts to walk.
    for part in payload.walk().filter(|_| full) {
        let mime_type = part.mime_type.as_deref().unwrap_or_default();
        calendar |= mime_type.eq_ignore_ascii_case("text/calendar");
        let filename = part.filename.as_deref().unwrap_or_default();
        let body = part.body.as_ref();
        let as_attachment = part.header("Content-Disposition").is_some_and(|value| {
            value
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("attachment")
        });
        let attachment_id = body.and_then(|body| body.attachment_id.clone());
        // Any part with a name or an attachment id is a file, because inline images often come
        // without a name. A big HTML or text body that Gmail moved out of the message (sync puts
        // it back) is the body, not a file.
        let is_body = !as_attachment
            && (mime_type.eq_ignore_ascii_case("text/html")
                || mime_type.eq_ignore_ascii_case("text/plain"));
        if !filename.is_empty() || (attachment_id.is_some() && !is_body) {
            let content_id = part.header("Content-ID").map(strip_angles);
            attachments.push(AttachmentRow {
                message_id: id.clone(),
                part_id: part.part_id.clone().unwrap_or_default(),
                attachment_id,
                filename: if filename.is_empty() {
                    default_name(mime_type)
                } else {
                    filename.to_owned()
                },
                mime_type: mime_type.to_owned(),
                size: body.and_then(|body| body.size).unwrap_or_default(),
                inline: false,
                content_id,
            });
            continue;
        }
        if as_attachment {
            continue;
        }
        let Some(data) = body.and_then(|body| body.data.as_deref()) else {
            continue;
        };
        if mime_type.eq_ignore_ascii_case("text/html") {
            html.push(String::from_utf8_lossy(data).into_owned());
        } else if mime_type.eq_ignore_ascii_case("text/plain") {
            text.push(String::from_utf8_lossy(data).into_owned());
        }
    }
    // A file is inline when the HTML shows it (`cid:` + its Content-ID) or it's a Gmail emoji,
    // whatever its Content-Disposition says, because senders often mark the images they show as
    // attachments.
    let shown = shown_content_ids(&html);
    for attachment in &mut attachments {
        attachment.inline = attachment.content_id.as_deref().is_some_and(|cid| {
            let lower = cid.to_ascii_lowercase();
            shown.contains(&lower)
                || lower
                    .strip_suffix("@goomoji.gmail")
                    .is_some_and(is_lower_hex)
        });
    }
    let joined = |parts: Vec<String>| (!parts.is_empty()).then(|| parts.join("\n"));

    Some(Parsed {
        message: MessageRow {
            id: id.clone(),
            thread_id,
            version: message.history_id.unwrap_or_default(),
            internal_date: message.internal_date.unwrap_or_default(),
            label_ids: message.label_ids.clone(),
            subject: header("Subject").unwrap_or_default(),
            snippet: message.snippet.clone().unwrap_or_default(),
            sender: Json(mailboxes("From").into_iter().next()),
            sender_raw: header("From").unwrap_or_default(),
            to: Json(mailboxes("To")),
            cc: Json(mailboxes("Cc")),
            bcc: Json(mailboxes("Bcc")),
            reply_to: Json(mailboxes("Reply-To")),
            rfc822_id: payload.header("Message-ID").map(strip_angles),
            in_reply_to: payload.header("In-Reply-To").map(strip_angles),
            references: payload
                .header("References")
                .map(|value| value.split_whitespace().map(strip_angles).collect())
                .unwrap_or_default(),
            list_id: header("List-Id"),
            list_unsubscribe: header("List-Unsubscribe"),
            list_unsubscribe_post: header("List-Unsubscribe-Post"),
            headers: Json(
                payload
                    .headers
                    .iter()
                    .map(|header| Header {
                        name: header.name.clone(),
                        value: header.value.clone(),
                    })
                    .collect(),
            ),
            size: message.size_estimate.unwrap_or_default(),
            has_attachments: calendar || attachments.iter().any(|attachment| !attachment.inline),
        },
        attachments,
        body: full.then(|| BodyRow {
            message_id: id,
            html: joined(html),
            text: joined(text),
            visible: None,
        }),
    })
}

/// The Content-IDs the HTML references, normalized to ASCII lowercase with `%HH` escapes decoded.
/// Each is what follows an ASCII-case-insensitive `cid:`, up to a quote, parenthesis, `>` or
/// whitespace.
pub(crate) fn shown_content_ids(html: &[String]) -> HashSet<String> {
    let mut out = HashSet::new();
    for part in html {
        let bytes = part.as_bytes();
        let mut i = 0;
        while i + 4 <= bytes.len() {
            if bytes[i..i + 4].eq_ignore_ascii_case(b"cid:") {
                let rest = &part[i + 4..];
                let end = rest
                    .find(|c: char| matches!(c, '(' | '\'' | '"' | ')' | '>') || c.is_whitespace())
                    .unwrap_or(rest.len());
                let raw_cid = &rest[..end];
                if !raw_cid.is_empty() {
                    out.insert(percent_decode(raw_cid).to_ascii_lowercase());
                }
                i += 4 + end;
            } else {
                i += 1;
            }
        }
    }
    out
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = bytes.get(i + 1..i + 3)
            && hex.iter().all(u8::is_ascii_hexdigit)
            && let Ok(hex_str) = std::str::from_utf8(hex)
            && let Ok(byte) = u8::from_str_radix(hex_str, 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The name of a file sent without one, from its MIME type.
pub(crate) fn default_name(mime_type: &str) -> String {
    let extension = [
        ("json", ".json"),
        ("pdf", ".pdf"),
        ("html", ".html"),
        ("jpeg", ".jpg"),
        ("gif", ".gif"),
        ("plain", ".txt"),
        ("png", ".png"),
        ("message/rfc822", ".eml"),
    ]
    .into_iter()
    .find(|(kind, _)| mime_type.contains(kind))
    .map_or("", |(_, extension)| extension);
    format!("untitled{extension}")
}

fn is_lower_hex(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_address_lists_outside_quotes() {
        let list = Mailbox::parse_list(
            r#""Lovelace, Ada" <ada@example.com>, "Ada \"The, Boss\" \\" <boss@example.com> (Team, Lead), bob@example.com, undisclosed-recipients:;"#,
        );
        assert_eq!(
            list,
            vec![
                Mailbox {
                    name: Some("Lovelace, Ada".into()),
                    email: "ada@example.com".into()
                },
                Mailbox {
                    name: Some(r#"Ada "The, Boss" \"#.into()),
                    email: "boss@example.com".into()
                },
                Mailbox {
                    name: None,
                    email: "bob@example.com".into()
                },
            ]
        );
    }

    #[test]
    fn outlook_uppercase_and_percent_encoded_cids_are_inline() {
        let part = |mime: &str, filename: &str, headers: &[(&str, &str)], data: &str| MessagePart {
            mime_type: Some(mime.into()),
            filename: Some(filename.into()),
            headers: headers
                .iter()
                .map(|(name, value)| gmail::MessagePartHeader {
                    name: (*name).into(),
                    value: (*value).into(),
                })
                .collect(),
            body: Some(gmail::MessagePartBody {
                data: (!data.is_empty()).then(|| data.as_bytes().to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let message = Message {
            id: Some("m".into()),
            thread_id: Some("t".into()),
            payload: Some(MessagePart {
                mime_type: Some("multipart/related".into()),
                parts: vec![
                    part(
                        "text/html",
                        "",
                        &[],
                        r#"<img src="CID:Banner.PNG@01DB.A0"><img src="cid:image001.png%4001DB3A10.12345670">"#,
                    ),
                    part(
                        "image/png",
                        "banner.png",
                        &[
                            ("Content-Disposition", "attachment"),
                            ("Content-ID", "<banner.png@01db.a0>"),
                        ],
                        "",
                    ),
                    part(
                        "image/png",
                        "image001.png",
                        &[
                            ("Content-Disposition", "inline"),
                            ("Content-ID", "<Image001.png@01DB3A10.12345670>"),
                        ],
                        "",
                    ),
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        let parsed = parse(&message, true).unwrap();
        assert!(parsed.attachments.iter().all(|a| a.inline));
        assert!(!parsed.message.has_attachments);
    }

    #[test]
    fn files_the_html_shows_are_inline_whatever_their_disposition() {
        let part = |mime: &str, filename: &str, headers: &[(&str, &str)], data: &str| MessagePart {
            mime_type: Some(mime.into()),
            filename: Some(filename.into()),
            headers: headers
                .iter()
                .map(|(name, value)| gmail::MessagePartHeader {
                    name: (*name).into(),
                    value: (*value).into(),
                })
                .collect(),
            body: Some(gmail::MessagePartBody {
                data: (!data.is_empty()).then(|| data.as_bytes().to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let message = Message {
            id: Some("m".into()),
            thread_id: Some("t".into()),
            payload: Some(MessagePart {
                mime_type: Some("multipart/mixed".into()),
                parts: vec![
                    part(
                        "text/html",
                        "",
                        &[],
                        "<img src=\"cid:comic=\"><img src='cid:1f600@goomoji.gmail'>",
                    ),
                    part(
                        "image/png",
                        "comic.png",
                        &[
                            ("Content-Disposition", "attachment"),
                            ("Content-ID", "<comic=>"),
                        ],
                        "",
                    ),
                    part(
                        "image/png",
                        "logo.png",
                        &[("Content-Disposition", "inline"), ("Content-ID", "<logo>")],
                        "",
                    ),
                    part(
                        "image/png",
                        "emoji.png",
                        &[("Content-ID", "<1f600@goomoji.gmail>")],
                        "",
                    ),
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        let parsed = parse(&message, true).unwrap();
        let inline: Vec<_> = parsed
            .attachments
            .iter()
            .map(|attachment| (attachment.filename.as_str(), attachment.inline))
            .collect();
        assert_eq!(
            inline,
            [
                ("comic.png", true),
                ("logo.png", false),
                ("emoji.png", true)
            ]
        );
        assert!(
            parsed.message.has_attachments,
            "the unreferenced logo is a file"
        );
    }

    #[test]
    fn nameless_parts_gmail_stores_apart_are_files() {
        let part =
            |mime: &str, headers: &[(&str, &str)], attachment_id: Option<&str>| MessagePart {
                mime_type: Some(mime.into()),
                filename: Some(String::new()),
                headers: headers
                    .iter()
                    .map(|(name, value)| gmail::MessagePartHeader {
                        name: (*name).into(),
                        value: (*value).into(),
                    })
                    .collect(),
                body: Some(gmail::MessagePartBody {
                    attachment_id: attachment_id.map(str::to_owned),
                    data: attachment_id
                        .is_none()
                        .then(|| b"<img src=\"cid:chart\">".to_vec()),
                    ..Default::default()
                }),
                ..Default::default()
            };
        let message = Message {
            id: Some("m".into()),
            thread_id: Some("t".into()),
            payload: Some(MessagePart {
                mime_type: Some("multipart/related".into()),
                parts: vec![
                    part("text/html", &[], None),
                    part("image/png", &[("Content-ID", "<chart>")], Some("A1")),
                    part("text/html", &[], Some("A2")),
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        let parsed = parse(&message, true).unwrap();
        let files: Vec<_> = parsed
            .attachments
            .iter()
            .map(|attachment| (attachment.filename.as_str(), attachment.inline))
            .collect();
        assert_eq!(
            files,
            [("untitled.png", true)],
            "a big HTML body isn't a file"
        );
        assert_eq!(default_name("application/octet-stream"), "untitled");
        assert_eq!(default_name("image/jpeg"), "untitled.jpg");
    }

    #[test]
    fn a_row_alone_has_no_body_or_files() {
        let message = Message {
            id: Some("m".into()),
            thread_id: Some("t".into()),
            payload: Some(MessagePart {
                mime_type: Some("multipart/mixed".into()),
                headers: vec![gmail::MessagePartHeader {
                    name: "Subject".into(),
                    value: "Q4".into(),
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let parsed = parse(&message, false).unwrap();
        assert!(parsed.body.is_none() && parsed.attachments.is_empty());
        assert!(!parsed.message.has_attachments);
        assert_eq!(parsed.message.subject, "Q4");
        assert!(parse(&message, true).unwrap().body.is_some());
    }

    #[test]
    fn percent_decode_requires_hex_digits() {
        assert_eq!(percent_decode("image%40host%2f1"), "image@host/1");
        assert_eq!(percent_decode("%+a%-a%0g"), "%+a%-a%0g");
    }
}
