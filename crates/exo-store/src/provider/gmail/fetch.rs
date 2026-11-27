//! Reading threads and messages from Gmail (`threads.get`, `messages.get`), parsed into the
//! store's rows.

use std::collections::{HashMap, HashSet};

use futures::future;
use gmail::{Format, Message, MessagePart};
use serde_json::Value;

use super::parse::parse;
use crate::api::{Api, Failure};
use crate::error::{Error, Result};
use crate::ids::labels::CHAT;
use crate::provider::{Fetched, Parsed};
use crate::quota::Lane;
use crate::source::normalize_snippets;
use crate::sync::ROW_HEADERS;

/// The largest `text/html` body Gmail moved to an attachment that's put back in its part.
const MAX_INLINED_BODY: u32 = 5 * 1024 * 1024;
/// `messages.get` calls in flight at once for a thread fetched message by message.
pub(crate) const MESSAGES_AT_ONCE: usize = 10;
/// Top-level MIME types whose message holds no file or invite, so its row needs none of its
/// structure. Anything else (`multipart/mixed`, `multipart/related`, `text/calendar`, …) may.
const BODY_ONLY: [&str; 3] = ["text/plain", "text/html", "multipart/alternative"];

/// Thread `id` with every message in full, for `lane`, with large HTML bodies inlined. A thread
/// too large for one answer comes [message by message](fetch_by_message). `None` when the
/// thread is gone.
pub(super) async fn thread(
    api: &Api,
    lane: Lane,
    attempts: u32,
    id: &str,
) -> Result<Option<Fetched>, Failure> {
    Ok(full_thread(api, lane, attempts, id)
        .await?
        .map(|thread| parsed(thread, id, |_| true)))
}

/// Fetches thread `id` for its rows, in `lane`. That's `threads.get` with `format=metadata` (a
/// third of `full`'s quota units) for its messages' labels, snippets and [`ROW_HEADERS`], plus
/// the messages that may hold files or an invite, fetched in full unless they're among `stored`.
/// The row's icon and `has:attachment`/`filename:` need those messages' parts. `None` when the
/// thread is gone.
pub(super) async fn rows(
    api: &Api,
    lane: Lane,
    attempts: u32,
    id: &str,
    stored: &HashSet<String>,
) -> Result<Option<Fetched>, Failure> {
    let fetched = api
        .call(lane, attempts, |gmail| {
            gmail.thread(id, Format::Metadata, &ROW_HEADERS)
        })
        .await;
    let Some(mut thread) = found(fetched)? else {
        return Ok(None);
    };
    let wanted: Vec<String> = thread
        .messages
        .iter()
        .filter(|message| needs_structure(message))
        .filter_map(|message| message.id.clone())
        .filter(|id| !stored.contains(id))
        .collect();
    let fetched = future::try_join_all(
        wanted
            .iter()
            .map(|id| full_message(api, lane, attempts, id)),
    )
    .await?;
    let mut full = HashSet::new();
    // A message gone since the thread was read keeps its row until history says so.
    for message in fetched.into_iter().flatten() {
        let Some(id) = message.id.clone() else {
            continue;
        };
        if let Some(slot) = thread
            .messages
            .iter_mut()
            .find(|slot| slot.id.as_ref() == Some(&id))
        {
            *slot = message;
            full.insert(id);
        }
    }
    Ok(Some(parsed(thread, id, |id| full.contains(id))))
}

/// Message `id` in full, for `lane`, with large HTML bodies inlined. `None` when it's gone (or a
/// chat).
pub(super) async fn message(
    api: &Api,
    lane: Lane,
    attempts: u32,
    id: &str,
) -> Result<Option<Parsed>, Failure> {
    Ok(full_message(api, lane, attempts, id)
        .await?
        .and_then(|message| parse(&message, true)))
}

/// Message `id`'s row alone (`format=metadata` with [`ROW_HEADERS`], a third of `full`'s
/// units). `None` when it's gone (or a chat).
pub(super) async fn row(
    api: &Api,
    lane: Lane,
    attempts: u32,
    id: &str,
) -> Result<Option<Parsed>, Failure> {
    let fetched = found(
        api.call(lane, attempts, |gmail| {
            gmail.message(id, Format::Metadata, &ROW_HEADERS)
        })
        .await,
    )?;
    Ok(fetched.and_then(|message| parse(&message, false)))
}

/// The `missing` messages of `thread` in full, the cheapest way: the one message alone
/// (`messages.get`, half a thread's units), or else the whole thread. Returns nothing when the
/// thread is gone.
pub(super) async fn bodies(
    api: &Api,
    lane: Lane,
    attempts: u32,
    thread: &str,
    missing: &[String],
) -> Result<Vec<Parsed>, Failure> {
    let messages = match missing {
        [message] => full_message(api, lane, attempts, message)
            .await?
            .into_iter()
            .collect(),
        _ => full_thread(api, lane, attempts, thread)
            .await?
            .map(|thread| thread.messages)
            .unwrap_or_default(),
    };
    Ok(messages
        .iter()
        .filter_map(|message| parse(message, true))
        .collect())
}

/// Thread `id`'s raw JSON source from `threads.get` (`format=full`), falling back to
/// `format=minimal` plus `messages.get` per message when the thread exceeds Gmail's single-answer
/// limit, with HTML entities in message snippets decoded.
pub(super) async fn thread_source(api: &Api, id: &str) -> Result<Value> {
    const SOURCE_ATTEMPTS: u32 = 3;
    let get = |format| {
        api.call(Lane::Act, SOURCE_ATTEMPTS, move |gmail| {
            gmail.thread(id, format, &[]).returning::<Value>()
        })
    };
    let mut thread = match get(Format::Full).await {
        Err(failure) if failure.is_too_large() => {
            let mut thread = get(Format::Minimal).await?;
            let ids: Vec<String> = thread
                .get("messages")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|message| message.get("id")?.as_str().map(str::to_owned))
                .collect();
            let mut messages = Vec::new();
            for chunk in ids.chunks(MESSAGES_AT_ONCE) {
                messages.extend(
                    future::try_join_all(chunk.iter().map(|message| {
                        api.call(Lane::Act, SOURCE_ATTEMPTS, |gmail| {
                            gmail
                                .message(message, Format::Full, &[])
                                .returning::<Value>()
                        })
                    }))
                    .await?,
                );
            }
            if let Some(slot) = thread.get_mut("messages").and_then(Value::as_array_mut) {
                *slot = messages;
            }
            thread
        }
        fetched => fetched?,
    };
    normalize_snippets(&mut thread);
    Ok(thread)
}

/// The bytes of message `message`'s MIME part `part`, in `lane`. `attachment` is the attachment
/// id the part had when it was stored. Gmail hands out a new one every time it returns the
/// message, so when Gmail refuses a stored id (or there is none, because the part's data came
/// inline), the part is read from the message as Gmail returns it now. A part read by its
/// attachment id reports its transfer to `progress`.
pub(super) async fn part(
    api: &Api,
    lane: Lane,
    message: &str,
    attachment: Option<&str>,
    part: &str,
    progress: Option<gmail::Progress>,
) -> Result<Vec<u8>> {
    fn report<'a>(
        call: gmail::Call<'a, gmail::MessagePartBody>,
        progress: Option<&gmail::Progress>,
    ) -> gmail::Call<'a, gmail::MessagePartBody> {
        match progress {
            Some(progress) => call.on_progress(progress.clone()),
            None => call,
        }
    }
    if let Some(attachment) = attachment {
        match api
            .call(lane, 3, |gmail| {
                report(gmail.attachment(message, attachment), progress.as_ref())
            })
            .await
        {
            Ok(body) => return Ok(body.data.unwrap_or_default()),
            Err(failure) if failure.is_permanent() => {
                log::info!(
                    "Gmail refused part {part} of {message} by its stored id ({}); fetching the message",
                    failure.message()
                );
            }
            Err(failure) => return Err(failure.into()),
        }
    }
    let missing = || Error::MissingPart {
        message: message.to_owned(),
        part: part.to_owned(),
    };
    let fetched = api
        .call(lane, 3, |gmail| gmail.message(message, Format::Full, &[]))
        .await?;
    let body = fetched
        .payload
        .as_ref()
        .and_then(|payload| {
            payload
                .walk()
                .find(|candidate| candidate.part_id.as_deref() == Some(part))
        })
        .and_then(|found| found.body.clone())
        .ok_or_else(missing)?;
    match (body.data, body.attachment_id) {
        (Some(data), _) => Ok(data),
        (None, Some(fresh)) => Ok(api
            .call(lane, 3, |gmail| {
                report(gmail.attachment(message, &fresh), progress.as_ref())
            })
            .await?
            .data
            .unwrap_or_default()),
        (None, None) => Err(missing()),
    }
}

/// `thread` parsed, with the messages `full` picks as fetched in full and the rest as rows
/// alone. `requested` stands in for an id Gmail left out.
fn parsed(thread: gmail::Thread, requested: &str, full: impl Fn(&str) -> bool) -> Fetched {
    Fetched {
        id: thread.id.unwrap_or_else(|| requested.to_owned()),
        version: thread.history_id.unwrap_or_default(),
        messages: thread
            .messages
            .iter()
            .filter_map(|message| parse(message, message.id.as_deref().is_some_and(&full)))
            .collect(),
    }
}

/// `threads.get` in full for `lane`, with large HTML bodies inlined. A thread too large for one
/// answer comes [message by message](fetch_by_message). `None` when the thread is gone.
async fn full_thread(
    api: &Api,
    lane: Lane,
    attempts: u32,
    id: &str,
) -> Result<Option<gmail::Thread>, Failure> {
    let fetched = api
        .call(lane, attempts, |gmail| gmail.thread(id, Format::Full, &[]))
        .await;
    let mut thread = match fetched {
        Err(failure) if failure.is_too_large() => {
            return fetch_by_message(api, lane, attempts, id).await;
        }
        fetched => match found(fetched)? {
            Some(thread) => thread,
            None => return Ok(None),
        },
    };
    for message in &mut thread.messages {
        inline_bodies(api, lane, attempts, message).await?;
    }
    Ok(Some(thread))
}

/// Thread `id` as [`full_thread`] has it, for a thread Gmail won't send in one answer: its
/// message ids (`format=minimal`), then each message in full, [`MESSAGES_AT_ONCE`] at a time.
/// A message gone meanwhile is left out.
async fn fetch_by_message(
    api: &Api,
    lane: Lane,
    attempts: u32,
    id: &str,
) -> Result<Option<gmail::Thread>, Failure> {
    let fetched = api
        .call(lane, attempts, |gmail| {
            gmail.thread(id, Format::Minimal, &[])
        })
        .await;
    let Some(mut thread) = found(fetched)? else {
        return Ok(None);
    };
    let ids: Vec<String> = thread
        .messages
        .iter()
        .filter_map(|message| message.id.clone())
        .collect();
    let mut messages = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(MESSAGES_AT_ONCE) {
        let fetched =
            future::try_join_all(chunk.iter().map(|id| full_message(api, lane, attempts, id)))
                .await?;
        messages.extend(fetched.into_iter().flatten());
    }
    thread.messages = messages;
    Ok(Some(thread))
}

/// Whether a message fetched for its row may hold a file or an invite. It may when its top-level
/// type isn't one of [`BODY_ONLY`], or when it's an Exchange meeting message, whose invite or
/// RSVP is a `text/calendar` alternative inside `multipart/alternative` (Outlook's
/// `Content-Class: urn:content-classes:calendarmessage`). Chats are never stored.
fn needs_structure(message: &Message) -> bool {
    let Some(payload) = &message.payload else {
        return true;
    };
    let content_type = || {
        payload
            .header("Content-Type")
            .and_then(|value| value.split(';').next())
            .map(str::trim)
    };
    let mime = payload.mime_type.as_deref().or_else(content_type);
    let meeting = payload
        .header("Content-Class")
        .is_some_and(|class| class.trim().eq_ignore_ascii_case(CALENDAR_MESSAGE));
    !message.label_ids.iter().any(|label| label == CHAT)
        && (meeting
            || !mime.is_some_and(|mime| {
                BODY_ONLY
                    .iter()
                    .any(|plain| mime.eq_ignore_ascii_case(plain))
            }))
}

/// The `Content-Class` Exchange gives its meeting requests, updates, cancellations and replies.
const CALENDAR_MESSAGE: &str = "urn:content-classes:calendarmessage";

/// `messages.get` in full for `lane`, with large HTML bodies inlined. `None` when the message
/// is gone.
async fn full_message(
    api: &Api,
    lane: Lane,
    attempts: u32,
    id: &str,
) -> Result<Option<Message>, Failure> {
    let fetched = api
        .call(lane, attempts, |gmail| gmail.message(id, Format::Full, &[]))
        .await;
    let Some(mut message) = found(fetched)? else {
        return Ok(None);
    };
    inline_bodies(api, lane, attempts, &mut message).await?;
    Ok(Some(message))
}

/// `None` when Gmail says what was asked for doesn't exist.
fn found<T>(fetched: Result<T, Failure>) -> Result<Option<T>, Failure> {
    match fetched {
        Ok(value) => Ok(Some(value)),
        Err(failure) if failure.is_not_found() => Ok(None),
        Err(failure) => Err(failure),
    }
}

/// Gmail moves big `text/html` bodies into attachments, so this fetches them back into the
/// part.
async fn inline_bodies(
    api: &Api,
    lane: Lane,
    attempts: u32,
    message: &mut Message,
) -> Result<(), Failure> {
    let (Some(message_id), Some(payload)) = (message.id.clone(), message.payload.as_mut()) else {
        return Ok(());
    };
    let wanted: Vec<String> = payload
        .walk()
        .filter(|part| part.mime_type.as_deref() == Some("text/html"))
        .filter_map(|part| {
            let body = part.body.as_ref()?;
            let small = body.size.unwrap_or_default() <= MAX_INLINED_BODY;
            (body.data.is_none() && small).then(|| body.attachment_id.clone())?
        })
        .collect();
    let mut fetched = HashMap::new();
    for attachment in wanted {
        let body = api
            .call(lane, attempts, |gmail| {
                gmail.attachment(&message_id, &attachment)
            })
            .await?;
        fetched.insert(attachment, body.data);
    }
    fill(payload, &mut fetched);
    Ok(())
}

fn fill(part: &mut MessagePart, fetched: &mut HashMap<String, Option<Vec<u8>>>) {
    if let Some(body) = part.body.as_mut()
        && let Some(data) = body
            .attachment_id
            .as_ref()
            .and_then(|id| fetched.remove(id))
    {
        body.data = data;
    }
    for child in &mut part.parts {
        fill(child, fetched);
    }
}

#[cfg(test)]
mod tests {
    use gmail::MessagePartHeader;

    use super::*;

    #[test]
    fn rows_need_the_parts_of_what_may_hold_files_or_invites() {
        let message = |mime: Option<&str>, content_type: Option<&str>| Message {
            payload: Some(MessagePart {
                mime_type: mime.map(str::to_owned),
                headers: content_type
                    .map(|value| MessagePartHeader {
                        name: "Content-Type".into(),
                        value: value.into(),
                    })
                    .into_iter()
                    .collect(),
                ..MessagePart::default()
            }),
            ..Message::default()
        };
        for body_only in ["multipart/alternative", "text/html", "TEXT/PLAIN"] {
            assert!(
                !needs_structure(&message(Some(body_only), None)),
                "{body_only}"
            );
        }
        for structured in [
            "multipart/mixed",
            "multipart/related",
            "multipart/signed",
            "text/calendar",
            "application/pdf",
        ] {
            assert!(
                needs_structure(&message(Some(structured), None)),
                "{structured}"
            );
        }
        assert!(
            !needs_structure(&message(
                None,
                Some("multipart/alternative; boundary=\"b\"")
            )),
            "the header stands in for a missing type"
        );
        assert!(
            needs_structure(&message(None, None)),
            "an unknown type is fetched in full"
        );
        let mut outlook = message(Some("multipart/alternative"), None);
        if let Some(payload) = outlook.payload.as_mut() {
            payload.headers.push(MessagePartHeader {
                name: "Content-class".into(),
                value: "urn:content-classes:calendarmessage".into(),
            });
        }
        assert!(
            needs_structure(&outlook),
            "an Exchange invite's calendar part is an alternative"
        );
    }

    fn plain(id: &str, labels: &[&str]) -> Message {
        Message {
            id: Some(id.into()),
            thread_id: Some("t1".into()),
            label_ids: labels.iter().map(|label| (*label).to_owned()).collect(),
            payload: Some(MessagePart {
                mime_type: Some("text/plain".into()),
                body: Some(gmail::MessagePartBody {
                    data: Some(b"hello".to_vec()),
                    ..gmail::MessagePartBody::default()
                }),
                ..MessagePart::default()
            }),
            ..Message::default()
        }
    }

    #[test]
    fn a_thread_parses_into_rows_with_bodies_for_what_came_in_full() {
        let thread = gmail::Thread {
            id: None,
            history_id: Some(42),
            messages: vec![
                plain("m1", &["INBOX"]),
                plain("m2", &["INBOX", "CHAT"]),
                plain("m3", &["SENT"]),
            ],
            ..gmail::Thread::default()
        };
        let fetched = parsed(thread, "t1", |id| id == "m3");
        assert_eq!(fetched.id, "t1", "the id asked for stands in");
        assert_eq!(fetched.version, 42);
        let ids: Vec<&str> = fetched
            .messages
            .iter()
            .map(|parsed| parsed.message.id.as_str())
            .collect();
        assert_eq!(ids, ["m1", "m3"], "chats are left out");
        assert!(fetched.messages[0].body.is_none(), "a row alone");
        assert_eq!(
            fetched.messages[1]
                .body
                .as_ref()
                .and_then(|body| body.text.as_deref()),
            Some("hello")
        );
    }
}
