//! Drafts on Outlook, and sending them.
//!
//! An Outlook draft is a message in Drafts, so a draft's id and its message's are one id. It's
//! made from Exo's RFC 822 message, and since Graph can't change a draft's MIME, a changed draft
//! is made again and the old one deleted; the draft's id follows the new one. A draft goes only
//! by `send` on it, never by sending its message afresh, so a send tried again after a lost
//! answer finds the draft already sent and sends nothing twice. Its Sent Items copy keeps its id.
//!
//! Graph takes a request of up to 4 MB, and MIME goes base64-encoded. A message larger than that
//! is made without its largest files, which then go on the draft one by one: those over 3 MB
//! through an upload session (learn.microsoft.com/graph/outlook-large-attachments).

use std::time::Duration;

use gmail::{Progress, Transfer};
use mail_parser::{MessageParser, MimeHeaders};
use outlook::{ApiError, Attachment, AttachmentItem, ErrorResponse, Message, UploadSession};
use reqwest::header;

use super::Outlook;
use crate::api::Failure;
use crate::drafts::SendFailure;
use crate::error::ProviderError;
use crate::provider::{DraftHandle, Sent};
use crate::quota::Lane;

/// The largest MIME made into a draft in one request, a third under Graph's 4 MB once encoded.
const MIME_BUDGET: usize = 2_900_000;
/// Files at or above this size go through an upload session so base64 JSON stays under Graph's
/// 4 MB request limit.
const SESSION_FROM: usize = 2_900_000;
/// One range PUT to an upload session: 3 MiB (a multiple of 320 KiB, under Graph's 4 MB cap).
const UPLOAD_RANGE: usize = 3 << 20;
/// Stream chunk size for upload-session range bodies so progress updates smoothly.
const STREAM_CHUNK: usize = 64 << 10;
/// Tries at `send` before Graph's server errors fail it, as for Gmail's sends.
const SEND_ATTEMPTS: u32 = 4;
/// Tries per upload-session range on transient or rate-limit errors.
const UPLOAD_ATTEMPTS: u32 = 3;

/// A file taken out of a draft's MIME, to attach on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
struct File {
    name: String,
    content_type: String,
    content_id: Option<String>,
    inline: bool,
    bytes: Vec<u8>,
}

impl Outlook {
    /// The message draft `id` holds now: itself, while it's a draft. `None` once it's gone or
    /// sent.
    pub(super) async fn draft_message(&self, id: &str) -> Result<Option<String>, Failure> {
        match self
            .graph
            .call(Lane::Act, 2, |graph| {
                graph.mail().message(id).select("id,isDraft")
            })
            .await
        {
            Ok(message) => Ok(message.id.filter(|_| message.is_draft == Some(true))),
            Err(failure) if failure.is_not_found() => Ok(None),
            Err(failure) => Err(failure),
        }
    }

    /// The draft holding message `message`: the message, while it's a draft.
    pub(super) async fn find_draft(&self, message: &str) -> Result<Option<String>, Failure> {
        self.draft_message(message).await
    }

    /// The drafts whose Message-ID is `rfc822_id`.
    pub(super) async fn drafts_with(&self, rfc822_id: &str) -> Result<Vec<String>, Failure> {
        Ok(self
            .with_message_id(outlook::well_known::DRAFTS, rfc822_id)
            .await?
            .into_iter()
            .filter_map(|message| message.id)
            .collect())
    }

    /// A new draft holding the RFC 822 message `raw`. Its thread is the conversation Outlook
    /// puts it in, by its headers. Reports upload progress to `progress` across the MIME create
    /// and any separately attached files.
    pub(super) async fn create_draft(
        &self,
        raw: &[u8],
        progress: Option<&Progress>,
    ) -> Result<DraftHandle, Failure> {
        let (mime, files) = split(raw);
        let total = (mime.len() + files.iter().map(|file| file.bytes.len()).sum::<usize>()) as u64;
        let report = |done: u64| {
            if let Some(progress) = progress {
                progress(Transfer {
                    done: done.min(total),
                    total: Some(total),
                });
            }
        };
        let draft: Message = self
            .graph
            .call(Lane::Act, 1, |graph| {
                graph.mail().create_draft_mime(mime.clone())
            })
            .await?;
        let mut uploaded = mime.len() as u64;
        report(uploaded);
        let Some(id) = draft.id.clone() else {
            return Ok(DraftHandle {
                id: None,
                message: None,
                thread: draft.conversation_id,
            });
        };
        for file in &files {
            if let Err(failure) = self.attach(&id, file, uploaded, total, progress).await {
                // A draft without all its files isn't what was saved.
                if let Err(gone) = self.delete_draft(&id).await {
                    log::warn!("taking back a draft missing a file: {}", gone.message());
                }
                return Err(failure);
            }
            uploaded += file.bytes.len() as u64;
            report(uploaded);
        }
        Ok(DraftHandle {
            id: Some(id.clone()),
            message: Some(id),
            thread: draft.conversation_id,
        })
    }

    /// Draft `id` holding `raw` now: a new draft of it, and `id` deleted.
    pub(super) async fn update_draft(
        &self,
        id: &str,
        raw: &[u8],
        progress: Option<&Progress>,
    ) -> Result<DraftHandle, Failure> {
        let made = self.create_draft(raw, progress).await?;
        match self.delete_draft(id).await {
            Ok(()) => {}
            Err(failure) if failure.is_not_found() => {}
            // The new draft is the one kept; the old one stays in Drafts until it's deleted.
            Err(failure) => {
                log::warn!("deleting an edited draft's old copy: {}", failure.message())
            }
        }
        Ok(made)
    }

    /// Sends draft `id`. A send whose answer was lost, tried again, finds the draft sent: that's
    /// the send.
    pub(super) async fn send_draft(&self, id: &str) -> Result<Sent, Failure> {
        let sent = self
            .graph
            .call(Lane::Act, SEND_ATTEMPTS, |graph| {
                graph.mail().send_draft(id)
            })
            .await;
        match sent {
            // Graph's `/send` answers 202 with no body and may assign a new `conversationId` on
            // send; leave `thread` as `None` so the caller's single full message refresh resolves
            // the authoritative conversation id without an extra GET.
            Ok(()) => Ok(Sent {
                message: Some(id.to_owned()),
                thread: None,
            }),
            Err(
                failure @ (Failure::Offline(_) | Failure::Throttled(_) | Failure::Unauthorized(_)),
            ) => Err(failure),
            Err(failure) => {
                let now = self
                    .graph
                    .call(Lane::Act, 2, |graph| {
                        graph.mail().message(id).select("id,isDraft,conversationId")
                    })
                    .await;
                match now {
                    Ok(message) if message.is_draft == Some(false) => Ok(Sent {
                        message: message.id,
                        thread: message.conversation_id,
                    }),
                    _ => Err(failure),
                }
            }
        }
    }

    /// The sent message with Message-ID `rfc822_id`, if there is one.
    pub(super) async fn find_sent(&self, rfc822_id: &str) -> Result<Option<Sent>, Failure> {
        Ok(self
            .with_message_id(outlook::well_known::SENT_ITEMS, rfc822_id)
            .await?
            .into_iter()
            .next()
            .map(|message| Sent {
                message: message.id,
                thread: message.conversation_id,
            }))
    }

    /// Deletes draft `id`.
    pub(super) async fn delete_draft(&self, id: &str) -> Result<(), Failure> {
        self.graph
            .call(Lane::Act, 1, |graph| graph.mail().delete_message(id))
            .await
    }

    /// The messages in `folder` with Message-ID `rfc822_id`.
    async fn with_message_id(
        &self,
        folder: &str,
        rfc822_id: &str,
    ) -> Result<Vec<Message>, Failure> {
        let id = if rfc822_id.starts_with('<') {
            rfc822_id.to_owned()
        } else {
            format!("<{rfc822_id}>")
        };
        let filter = format!("internetMessageId eq {}", outlook::odata::quote(&id));
        Ok(self
            .graph
            .call(Lane::Act, 2, |graph| {
                graph
                    .mail()
                    .folder_messages(folder)
                    .filter(&filter)
                    .select("id,conversationId")
            })
            .await?
            .value)
    }

    /// Puts `file` on draft `id`, reporting streamed progress relative to `uploaded`/`total`.
    async fn attach(
        &self,
        id: &str,
        file: &File,
        uploaded: u64,
        total: u64,
        progress: Option<&Progress>,
    ) -> Result<(), Failure> {
        if file.bytes.len() < SESSION_FROM {
            return self.add_small_attachment(id, file).await;
        }
        let item = AttachmentItem {
            is_inline: Some(file.inline),
            content_id: file.content_id.clone(),
            ..AttachmentItem::file(&file.name, &file.content_type, file.bytes.len() as u64)
        };
        let session = match self
            .graph
            .call(Lane::Act, 1, |graph| {
                graph.mail().create_upload_session(id, &item)
            })
            .await
        {
            Ok(session) => session,
            // Some endpoints enforce a strict 3 MiB minimum on upload sessions; fall back to
            // a direct attachment POST when the server says the file is below its session floor.
            Err(Failure::Permanent(ProviderError::Outlook(ref error)))
                if error.code() == Some("ErrorAttachmentSizeShouldNotBeLessThanMinimumSize") =>
            {
                return self.add_small_attachment(id, file).await;
            }
            Err(failure) => return Err(failure),
        };
        self.upload_session(&session, &file.bytes, uploaded, total, progress)
            .await
    }

    /// Attaches a file in a single JSON `POST /messages/{id}/attachments`.
    async fn add_small_attachment(&self, id: &str, file: &File) -> Result<(), Failure> {
        let attachment = Attachment {
            is_inline: Some(file.inline),
            content_id: file.content_id.clone(),
            ..Attachment::file(&file.name, &file.content_type, file.bytes.clone())
        };
        self.graph
            .call(Lane::Act, 1, |graph| {
                graph.mail().add_attachment(id, &attachment)
            })
            .await
            .map(drop)
    }

    /// Uploads `bytes` to `session` in 3 MiB ranges, streaming each range in 64 KiB chunks so
    /// `progress` advances smoothly throughout large-attachment uploads.
    async fn upload_session(
        &self,
        session: &UploadSession,
        bytes: &[u8],
        uploaded: u64,
        total: u64,
        progress: Option<&Progress>,
    ) -> Result<(), Failure> {
        let size = bytes.len();
        for start in (0..size).step_by(UPLOAD_RANGE) {
            let end = (start + UPLOAD_RANGE).min(size);
            let range = &bytes[start..end];
            let content_range = format!("bytes {start}-{}/{size}", end - 1);
            let range_base = uploaded + start as u64;
            let url = session.upload_url.clone();
            let outcome = self
                .graph
                .call(Lane::Act, UPLOAD_ATTEMPTS, |_| {
                    let http = self.http.clone();
                    let url = url.clone();
                    let content_range = content_range.clone();
                    let data = range.to_vec();
                    let progress = progress.cloned();
                    async move {
                        let len = data.len();
                        let body = match progress {
                            Some(progress) => streamed_range(data, range_base, total, progress),
                            None => reqwest::Body::from(data),
                        };
                        let response = http
                            .put(url)
                            .header(header::CONTENT_TYPE, "application/octet-stream")
                            .header(header::CONTENT_LENGTH, len)
                            .header(header::CONTENT_RANGE, content_range)
                            .body(body)
                            .send()
                            .await?;
                        let status = response.status();
                        let retry_after = response
                            .headers()
                            .get(header::RETRY_AFTER)
                            .and_then(|value| value.to_str().ok())
                            .and_then(parse_retry_after);
                        let body = response.bytes().await?;
                        if status == reqwest::StatusCode::UNAUTHORIZED {
                            // `upload_url` is pre-authenticated without the account's OAuth token;
                            // 401 means the upload session expired, not that the account was signed
                            // out. Stop retrying this expired session and surface a transient error
                            // so the next attempt opens a fresh session.
                            return Ok(RangeOutcome::SessionExpired(decode_graph_error(
                                status.as_u16(),
                                &body,
                                retry_after,
                            )));
                        }
                        if !status.is_success() {
                            return Err(decode_graph_error(status.as_u16(), &body, retry_after));
                        }
                        Ok(RangeOutcome::Uploaded)
                    }
                })
                .await?;
            if let RangeOutcome::SessionExpired(error) = outcome {
                return Err(Failure::Transient(error.into()));
            }
        }
        Ok(())
    }
}

/// Result of uploading one range to a pre-authenticated Graph upload session.
enum RangeOutcome {
    Uploaded,
    SessionExpired(outlook::Error),
}

/// Streams `data` in [`STREAM_CHUNK`] pieces, reporting cumulative progress against `total`.
fn streamed_range(data: Vec<u8>, base: u64, total: u64, progress: Progress) -> reqwest::Body {
    let chunks = (0..data.len()).step_by(STREAM_CHUNK).map(move |start| {
        let end = (start + STREAM_CHUNK).min(data.len());
        progress(Transfer {
            done: (base + end as u64).min(total),
            total: Some(total),
        });
        Ok::<_, std::io::Error>(data[start..end].to_vec())
    });
    reqwest::Body::wrap_stream(futures::stream::iter(chunks))
}

/// Decodes a non-success upload-session response into [`outlook::Error`].
fn decode_graph_error(status: u16, body: &[u8], retry_after: Option<Duration>) -> outlook::Error {
    match serde_json::from_slice::<ErrorResponse>(body) {
        Ok(response) => outlook::Error::Api(ApiError {
            status,
            error: response.error,
            retry_after,
        }),
        Err(_) => outlook::Error::Decode {
            status,
            reason: format!(
                "not a Graph error body; body starts with {:?}",
                String::from_utf8_lossy(&body[..body.len().min(200)])
            ),
        },
    }
}

/// Parses an integer-seconds `Retry-After` header value.
fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Why Graph refused to send a draft, from its answer.
pub(super) fn refusal(error: &ProviderError) -> SendFailure {
    let ProviderError::Outlook(error) = error else {
        return SendFailure::Other;
    };
    if error.status() == Some(413) {
        return SendFailure::TooLarge;
    }
    match error.code() {
        Some("ErrorInvalidRecipients" | "ErrorInvalidRecipientsInternal") => {
            SendFailure::InvalidRecipients
        }
        Some("ErrorMessageSizeExceeded" | "ErrorRequestTooLarge") => SendFailure::TooLarge,
        Some("ErrorSubmissionQuotaExceeded" | "ErrorQuotaExceeded") => SendFailure::QuotaExceeded,
        _ => SendFailure::Other,
    }
}

/// `raw` to make a draft from, and the files taken out of it to fit Graph's requests, largest
/// first, until what's left fits.
fn split(raw: &[u8]) -> (Vec<u8>, Vec<File>) {
    if raw.len() <= MIME_BUDGET {
        return (raw.to_vec(), Vec::new());
    }
    let Some(message) = MessageParser::default().parse(raw) else {
        return (raw.to_vec(), Vec::new());
    };
    let mut parts: Vec<(usize, usize, File)> = message
        .attachments()
        .filter_map(|part| {
            let start = delimiter_before(raw, part.offset_header as usize)?;
            let end = (part.offset_end as usize).min(raw.len());
            if start >= end {
                return None;
            }
            let content_type = part.content_type().map_or_else(
                || "application/octet-stream".to_owned(),
                |kind| match kind.subtype() {
                    Some(sub) => format!("{}/{sub}", kind.ctype()),
                    None => kind.ctype().to_owned(),
                },
            );
            let inline = match part.content_disposition() {
                Some(disposition) => disposition.is_inline(),
                None => part.content_id().is_some(),
            };
            let file = File {
                name: part.attachment_name().unwrap_or("untitled").to_owned(),
                content_type,
                content_id: part
                    .content_id()
                    .map(|id| id.trim_matches(['<', '>']).to_owned()),
                inline,
                bytes: part.contents().to_vec(),
            };
            Some((start, end, file))
        })
        .collect();
    // Drop nested attachments (e.g. inside an attached `message/rfc822`) or any overlapping spans
    // so byte ranges are strictly disjoint before slicing `raw`.
    parts.sort_by_key(|(start, end, _)| (*start, std::cmp::Reverse(*end)));
    let mut top_level: Vec<(usize, usize, File)> = Vec::with_capacity(parts.len());
    let mut covered = 0usize;
    for part in parts {
        if part.0 < covered {
            continue;
        }
        covered = part.1;
        top_level.push(part);
    }
    top_level.sort_by_key(|(start, end, _)| std::cmp::Reverse(end - start));
    let mut size = raw.len();
    let mut taken: Vec<(usize, usize, File)> = Vec::new();
    for part in top_level {
        if size <= MIME_BUDGET {
            break;
        }
        size = size.saturating_sub(part.1 - part.0);
        taken.push(part);
    }
    taken.sort_by_key(|(start, ..)| *start);
    let mut mime = Vec::with_capacity(size);
    let mut at = 0;
    for (start, end, _) in &taken {
        mime.extend_from_slice(&raw[at..*start]);
        at = *end;
    }
    mime.extend_from_slice(&raw[at..]);
    (mime, taken.into_iter().map(|(.., file)| file).collect())
}

/// Where the delimiter line before a part whose headers start at `header` begins: the line
/// break that ends the part before it, as RFC 2046 has the break belong to the delimiter.
fn delimiter_before(raw: &[u8], header: usize) -> Option<usize> {
    let before = raw.get(..header)?;
    // The line before the headers is the delimiter (`--boundary`), and the break before it is
    // the delimiter's too.
    let line_end = before.len().checked_sub(1)?;
    let line_start = before[..line_end].iter().rposition(|&byte| byte == b'\n')?;
    if !before[line_start + 1..].starts_with(b"--") {
        return None;
    }
    Some(if line_start > 0 && before[line_start - 1] == b'\r' {
        line_start - 1
    } else {
        line_start
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(files: &[(&str, usize)]) -> Vec<u8> {
        let mut email = gmail::mime::Email {
            from: Some(gmail::mime::Mailbox::address("maya@brightmoor.co")),
            to: vec![gmail::mime::Mailbox::address("ada@analytical.example")],
            subject: "Plans".into(),
            text: Some("The plans are attached.".into()),
            ..gmail::mime::Email::default()
        };
        for (name, size) in files {
            email.attachments.push(gmail::mime::Attachment {
                filename: (*name).into(),
                content_type: "application/pdf".into(),
                data: vec![b'x'; *size],
                content_id: None,
            });
        }
        email.to_rfc822()
    }

    #[test]
    fn a_small_message_is_made_whole() {
        let raw = message(&[("small.pdf", 1000)]);
        let (mime, files) = split(&raw);
        assert_eq!(mime, raw);
        assert!(files.is_empty());
    }

    #[test]
    fn a_large_message_leaves_out_its_largest_files_until_it_fits() {
        let raw = message(&[
            ("big.pdf", 4 << 20),
            ("medium.pdf", 1 << 20),
            ("small.pdf", 10),
        ]);
        let (mime, files) = split(&raw);
        assert!(mime.len() <= MIME_BUDGET, "{}", mime.len());
        assert_eq!(
            files
                .iter()
                .map(|file| file.name.as_str())
                .collect::<Vec<_>>(),
            ["big.pdf"]
        );
        assert_eq!(files[0].bytes.len(), 4 << 20);
        assert_eq!(files[0].content_type, "application/pdf");
        let left = MessageParser::default().parse(&mime).expect("still MIME");
        let names: Vec<&str> = left
            .attachments()
            .filter_map(|part| part.attachment_name())
            .collect();
        assert_eq!(names, ["medium.pdf", "small.pdf"]);
        assert_eq!(
            left.body_text(0).as_deref(),
            Some("The plans are attached.")
        );
    }

    #[test]
    fn split_handles_nested_rfc822_attachments_and_omitted_disposition_inline_cid() {
        const { assert!(SESSION_FROM <= 2_900_000) };

        // Inner message with its own attachment, embedded inside an outer `message/rfc822` part
        // exceeding MIME_BUDGET so `split` must take the outer part without overlapping/panicking.
        let inner = message(&[("inner.pdf", 3 << 20)]);
        let outer = format!(
            "From: maya@brightmoor.co\r\n\
             To: ada@analytical.example\r\n\
             Subject: Forwarded\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"OUTER\"\r\n\
             \r\n\
             --OUTER\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             \r\n\
             See attached.\r\n\
             --OUTER\r\n\
             Content-Type: message/rfc822; name=\"forwarded.eml\"\r\n\
             Content-Disposition: attachment; filename=\"forwarded.eml\"\r\n\
             \r\n\
             {}\r\n\
             --OUTER--\r\n",
            String::from_utf8_lossy(&inner)
        );
        let (mime, files) = split(outer.as_bytes());
        assert!(mime.len() <= MIME_BUDGET, "{}", mime.len());
        assert_eq!(files.len(), 1, "only top-level attachment extracted");
        assert_eq!(files[0].content_type, "message/rfc822");

        // An attachment with Content-ID and omitted Content-Disposition is treated as inline.
        let payload = "A".repeat(4 << 20);
        let cid_msg = format!(
            "From: maya@brightmoor.co\r\n\
             To: ada@analytical.example\r\n\
             Subject: Inline\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/related; boundary=\"REL\"\r\n\
             \r\n\
             --REL\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             \r\n\
             <img src=\"cid:pic1\">\r\n\
             --REL\r\n\
             Content-Type: image/png; name=\"pic.png\"\r\n\
             Content-ID: <pic1>\r\n\
             \r\n\
             {payload}\r\n\
             --REL--\r\n"
        );
        let (_, files) = split(cid_msg.as_bytes());
        assert_eq!(files.len(), 1);
        assert!(files[0].inline, "Content-ID without disposition is inline");
        assert_eq!(files[0].content_id.as_deref(), Some("pic1"));
    }
}
