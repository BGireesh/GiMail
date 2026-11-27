//! Mail calls for the signed-in user (`/me`): folders, messages and their deltas, drafts and
//! sending, attachments and open extensions.

use reqwest::Method;
use serde::Serialize;

use crate::client::{Body, Call, Client, Decode, Media, Request};
use crate::error::Error;
use crate::model::*;
use crate::odata::{Page, quote};

/// How much of a large attachment each upload request carries. Microsoft asks for ranges under
/// 4 MB (learn.microsoft.com/graph/outlook-large-attachments).
const UPLOAD_CHUNK: usize = 3 << 20;

/// Mail calls. Get one with [`Client::mail`].
#[derive(Debug, Clone, Copy)]
pub struct Mail<'a> {
    client: &'a Client,
}

impl Client {
    /// Mail calls for the signed-in user.
    #[must_use]
    pub fn mail(&self) -> Mail<'_> {
        Mail { client: self }
    }
}

/// The body of `move` and `copy`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Destination<'a> {
    destination_id: &'a str,
}

impl<'a> Mail<'a> {
    fn call<T>(&self, method: Method, path: &[&str]) -> Call<'a, T> {
        self.client.me_call(method, path.iter().copied())
    }

    // Folders

    /// The top-level folders (`GET /me/mailFolders`).
    pub fn folders(&self) -> Call<'a, Page<MailFolder>> {
        self.call(Method::GET, &["mailFolders"])
    }

    /// A folder, by ID or [`well_known`] name.
    pub fn folder(&self, id: &str) -> Call<'a, MailFolder> {
        self.call(Method::GET, &["mailFolders", id])
    }

    /// A folder's subfolders (`childFolders`).
    pub fn child_folders(&self, id: &str) -> Call<'a, Page<MailFolder>> {
        self.call(Method::GET, &["mailFolders", id, "childFolders"])
    }

    /// Creates a folder (its `display_name`, and `is_hidden`) at the top level, or in `parent`.
    pub fn create_folder(&self, parent: Option<&str>, folder: &MailFolder) -> Call<'a, MailFolder> {
        match parent {
            Some(parent) => self.call(Method::POST, &["mailFolders", parent, "childFolders"]),
            None => self.call(Method::POST, &["mailFolders"]),
        }
        .json(folder)
    }

    /// Renames a folder.
    pub fn update_folder(&self, id: &str, folder: &MailFolder) -> Call<'a, MailFolder> {
        self.call(Method::PATCH, &["mailFolders", id]).json(folder)
    }

    /// Deletes a folder and what's in it.
    pub fn delete_folder(&self, id: &str) -> Call<'a, ()> {
        self.call(Method::DELETE, &["mailFolders", id])
    }

    /// The folder tree's delta (`/me/mailFolders/delta`): every folder in the first round, then
    /// those created, renamed, moved or deleted (learn.microsoft.com/graph/api/mailfolder-delta).
    pub fn folders_delta(&self) -> Call<'a, Page<MailFolder>> {
        self.call(Method::GET, &["mailFolders", "delta"])
    }

    // Messages

    /// Messages across folders (`GET /me/messages`), ten a page unless `top` says otherwise.
    pub fn messages(&self) -> Call<'a, Page<Message>> {
        self.call(Method::GET, &["messages"])
    }

    /// A folder's messages.
    pub fn folder_messages(&self, folder: &str) -> Call<'a, Page<Message>> {
        self.call(Method::GET, &["mailFolders", folder, "messages"])
    }

    /// A conversation's messages in every folder, oldest first. The filter leads with
    /// `receivedDateTime` because Outlook refuses ordering by a property the filter doesn't
    /// start with (learn.microsoft.com/graph/api/user-list-messages).
    pub fn conversation(&self, conversation_id: &str) -> Call<'a, Page<Message>> {
        self.messages()
            .filter(&format!(
                "receivedDateTime ge 1900-01-01T00:00:00Z and conversationId eq {}",
                quote(conversation_id)
            ))
            .order_by("receivedDateTime")
    }

    /// A folder's message delta (learn.microsoft.com/graph/api/message-delta): the folder's
    /// messages in the first round, newest first, then those created in, changed in or
    /// removed from it. Page size is [`Call::max_page_size`]; a delta link Graph no longer
    /// knows answers 410 `SyncStateNotFound`, after which the folder is listed again.
    pub fn messages_delta(&self, folder: &str) -> Call<'a, Page<Message>> {
        self.call(Method::GET, &["mailFolders", folder, "messages", "delta"])
    }

    /// A message.
    pub fn message(&self, id: &str) -> Call<'a, Message> {
        self.call(Method::GET, &["messages", id])
    }

    /// A message's MIME (`$value`), as Outlook exports it.
    pub fn mime(&self, id: &str) -> Call<'a, Media> {
        self.call(Method::GET, &["messages", id, "$value"])
    }

    /// Changes a message's read state, flag, categories or Focused verdict.
    pub fn update_message(&self, id: &str, patch: &MessagePatch) -> Call<'a, Message> {
        self.call(Method::PATCH, &["messages", id]).json(patch)
    }

    /// Moves a message to a folder (by ID or [`well_known`] name). Under immutable IDs, the
    /// message keeps its ID.
    pub fn move_message(&self, id: &str, destination: &str) -> Call<'a, Message> {
        self.call(Method::POST, &["messages", id, "move"])
            .json(&Destination {
                destination_id: destination,
            })
    }

    /// Copies a message to a folder. The copy has an ID of its own.
    pub fn copy_message(&self, id: &str, destination: &str) -> Call<'a, Message> {
        self.call(Method::POST, &["messages", id, "copy"])
            .json(&Destination {
                destination_id: destination,
            })
    }

    /// Deletes a message: it goes to Recoverable Items, out of every folder's view. Trashing a
    /// message is moving it to [`well_known::DELETED_ITEMS`].
    pub fn delete_message(&self, id: &str) -> Call<'a, ()> {
        self.call(Method::DELETE, &["messages", id])
    }

    // Drafts and sending

    /// Creates a draft in Drafts from JSON.
    pub fn create_draft(&self, message: &Message) -> Call<'a, Message> {
        self.call(Method::POST, &["messages"]).json(message)
    }

    /// Creates a draft in Drafts from an RFC 822 message. Graph can't change a draft's MIME
    /// afterwards, so a changed draft is created again.
    pub fn create_draft_mime(&self, rfc822: Vec<u8>) -> Call<'a, Message> {
        self.call(Method::POST, &["messages"]).mime(rfc822)
    }

    /// Changes a draft's content (subject, body, recipients, ...).
    pub fn update_draft(&self, id: &str, message: &Message) -> Call<'a, Message> {
        self.call(Method::PATCH, &["messages", id]).json(message)
    }

    /// A reply draft to the sender, in the message's conversation.
    pub fn create_reply(&self, id: &str, reply: &ReplyRequest) -> Call<'a, Message> {
        self.call(Method::POST, &["messages", id, "createReply"])
            .json(reply)
    }

    /// A reply draft to everyone.
    pub fn create_reply_all(&self, id: &str, reply: &ReplyRequest) -> Call<'a, Message> {
        self.call(Method::POST, &["messages", id, "createReplyAll"])
            .json(reply)
    }

    /// A forward draft, carrying the message's attachments.
    pub fn create_forward(&self, id: &str, forward: &ReplyRequest) -> Call<'a, Message> {
        self.call(Method::POST, &["messages", id, "createForward"])
            .json(forward)
    }

    /// Sends a draft as last saved. Its Sent Items copy keeps the draft's immutable ID.
    pub fn send_draft(&self, id: &str) -> Call<'a, ()> {
        self.call(Method::POST, &["messages", id, "send"])
    }

    /// Sends a message given as JSON without making a draft.
    pub fn send_mail(&self, mail: &SendMail) -> Call<'a, ()> {
        self.call(Method::POST, &["sendMail"]).json(mail)
    }

    /// Sends an RFC 822 message without making a draft. A copy goes to Sent Items.
    pub fn send_mime(&self, rfc822: Vec<u8>) -> Call<'a, ()> {
        self.call(Method::POST, &["sendMail"]).mime(rfc822)
    }

    // Attachments

    /// A message's attachments, without their bytes unless selected.
    pub fn attachments(&self, message: &str) -> Call<'a, Page<Attachment>> {
        self.call(Method::GET, &["messages", message, "attachments"])
    }

    /// An attachment, with its bytes for a file.
    pub fn attachment(&self, message: &str, id: &str) -> Call<'a, Attachment> {
        self.call(Method::GET, &["messages", message, "attachments", id])
    }

    /// A file attachment's bytes (`$value`).
    pub fn attachment_content(&self, message: &str, id: &str) -> Call<'a, Media> {
        self.call(
            Method::GET,
            &["messages", message, "attachments", id, "$value"],
        )
    }

    /// Attaches a file of up to 3 MB to a draft ([`Attachment::file`]).
    pub fn add_attachment(&self, message: &str, attachment: &Attachment) -> Call<'a, Attachment> {
        self.call(Method::POST, &["messages", message, "attachments"])
            .json(attachment)
    }

    /// Takes an attachment off a draft.
    pub fn delete_attachment(&self, message: &str, id: &str) -> Call<'a, ()> {
        self.call(Method::DELETE, &["messages", message, "attachments", id])
    }

    /// Starts uploading a file of 3 MB to 150 MB to a draft. Send its bytes with
    /// [`Mail::upload`].
    pub fn create_upload_session(
        &self,
        message: &str,
        item: &AttachmentItem,
    ) -> Call<'a, UploadSession> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            attachment_item: &'a AttachmentItem,
        }
        self.call(
            Method::POST,
            &["messages", message, "attachments", "createUploadSession"],
        )
        .json(&Body {
            attachment_item: item,
        })
    }

    /// Sends `content`, the whole file, to `session` in ranges, in order. The session's URL is
    /// pre-authenticated, so the ranges go without the access token.
    ///
    /// # Errors
    /// The first range Graph refused, or a transport error.
    pub async fn upload(&self, session: &UploadSession, content: &[u8]) -> Result<(), Error> {
        let total = content.len();
        for start in (0..total).step_by(UPLOAD_CHUNK) {
            let end = (start + UPLOAD_CHUNK).min(total);
            let request = Request {
                method: Method::PUT,
                url: session.upload_url.clone(),
                prefer: Vec::new(),
                body: Body::Chunk {
                    content_range: format!("bytes {start}-{}/{total}", end - 1),
                    data: content[start..end].to_vec(),
                },
                authenticated: false,
            };
            let answer = self.client.send(&request, <()>::throttled).await?;
            if !answer.status.is_success() {
                return Err(answer.error());
            }
        }
        Ok(())
    }

    // Open extensions

    /// A message's open extensions.
    pub fn extensions(&self, message: &str) -> Call<'a, Page<OpenTypeExtension>> {
        self.call(Method::GET, &["messages", message, "extensions"])
    }

    /// One of them, by name or ID.
    pub fn extension(&self, message: &str, name: &str) -> Call<'a, OpenTypeExtension> {
        self.call(Method::GET, &["messages", message, "extensions", name])
    }

    /// Adds an open extension to a message. A second one of the same name is 409
    /// `NameAlreadyExists`.
    pub fn create_extension(
        &self,
        message: &str,
        extension: &OpenTypeExtension,
    ) -> Call<'a, OpenTypeExtension> {
        self.call(Method::POST, &["messages", message, "extensions"])
            .json(extension)
    }

    /// Changes the properties `extension` names and keeps the rest.
    pub fn update_extension(
        &self,
        message: &str,
        name: &str,
        extension: &OpenTypeExtension,
    ) -> Call<'a, OpenTypeExtension> {
        self.call(Method::PATCH, &["messages", message, "extensions", name])
            .json(extension)
    }

    /// Deletes an open extension.
    pub fn delete_extension(&self, message: &str, name: &str) -> Call<'a, ()> {
        self.call(Method::DELETE, &["messages", message, "extensions", name])
    }
}
