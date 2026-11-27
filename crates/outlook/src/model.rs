//! Graph v1.0's mail resources (learn.microsoft.com/graph/api/resources/message and its
//! neighbours). Only the properties Exo reads or writes are modeled; serde skips the rest.
//!
//! A property is absent under `$select` and in a delta's removal, so scalars are `Option` and
//! collections default to empty. Graph sends `null` for unset scalars, which reads as `None`.
//! Writing skips `None` and empty collections, so the same types make creation bodies. Updates
//! that must clear a property have their own types ([`MessagePatch`]). Graph's evolvable enums
//! read members this client doesn't know as `UnknownFutureValue`.

use jiff::Timestamp;
use jiff::civil::DateTime;
use serde::de::Error as _;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use serde_with::{serde_as, skip_serializing_none};
use url::Url;

use crate::odata::Removed;
use crate::wire::Base64;

/// Names Graph accepts in place of a well-known folder's ID
/// (learn.microsoft.com/graph/api/resources/mailfolder).
pub mod well_known {
    /// The Inbox.
    pub const INBOX: &str = "inbox";
    /// Drafts.
    pub const DRAFTS: &str = "drafts";
    /// Sent Items.
    pub const SENT_ITEMS: &str = "sentitems";
    /// Deleted Items, where a moved-to-trash message goes.
    pub const DELETED_ITEMS: &str = "deleteditems";
    /// Junk Email.
    pub const JUNK_EMAIL: &str = "junkemail";
    /// Archive.
    pub const ARCHIVE: &str = "archive";
    /// The Outbox.
    pub const OUTBOX: &str = "outbox";
    /// The root of the folder tree, the top-level folders' parent.
    pub const ROOT: &str = "msgfolderroot";
}

/// A name and an address.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailAddress {
    /// The display name.
    pub name: Option<String>,
    /// The SMTP address.
    pub address: Option<String>,
}

/// A sender or recipient.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recipient {
    /// Who.
    pub email_address: Option<EmailAddress>,
}

impl Recipient {
    /// `address`, named `name` when there's one.
    #[must_use]
    pub fn new(address: &str, name: Option<&str>) -> Self {
        Self {
            email_address: Some(EmailAddress {
                name: name.map(str::to_owned),
                address: Some(address.to_owned()),
            }),
        }
    }
}

/// A body: HTML unless the call asked for text ([`crate::Call::text_body`]).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemBody {
    /// The content's type.
    pub content_type: Option<BodyType>,
    /// The content.
    pub content: Option<String>,
}

/// [`ItemBody::content_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BodyType {
    /// Plain text.
    Text,
    /// HTML.
    Html,
}

/// A local time and the zone it's in (Windows or IANA names), as Outlook writes times that
/// belong to a calendar or a flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DateTimeTimeZone {
    /// The time, without an offset (`2026-11-25T14:00:00.0000000`).
    pub date_time: DateTime,
    /// The zone, e.g. `UTC` or `Eastern Standard Time`.
    pub time_zone: String,
}

impl DateTimeTimeZone {
    /// A local date and time in `time_zone` (Windows or IANA name).
    #[must_use]
    pub fn new(date_time: DateTime, time_zone: impl Into<String>) -> Self {
        Self {
            date_time,
            time_zone: time_zone.into(),
        }
    }

    /// `at` expressed in UTC.
    #[must_use]
    pub fn utc(at: Timestamp) -> Self {
        Self {
            date_time: at.to_zoned(jiff::tz::TimeZone::UTC).datetime(),
            time_zone: "UTC".to_owned(),
        }
    }
}

/// A message's importance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Importance {
    /// Low.
    Low,
    /// Normal.
    Normal,
    /// High.
    High,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// Focused Inbox's verdict on a message, or an override's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InferenceClassificationType {
    /// Focused.
    Focused,
    /// Other.
    Other,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// A message's follow-up flag.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowupFlag {
    /// Flagged, complete or neither.
    pub flag_status: Option<FollowupFlagStatus>,
    /// When the follow-up starts.
    pub start_date_time: Option<DateTimeTimeZone>,
    /// When it's due.
    pub due_date_time: Option<DateTimeTimeZone>,
    /// When it was completed.
    pub completed_date_time: Option<DateTimeTimeZone>,
}

impl FollowupFlag {
    /// A flag of `status` alone.
    #[must_use]
    pub fn new(status: FollowupFlagStatus) -> Self {
        Self {
            flag_status: Some(status),
            ..Self::default()
        }
    }
}

/// [`FollowupFlag::flag_status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FollowupFlagStatus {
    /// Not flagged.
    NotFlagged,
    /// Flagged, then marked complete.
    Complete,
    /// Flagged.
    Flagged,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// One of a message's internet headers (`$select=internetMessageHeaders`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InternetMessageHeader {
    /// The header's name.
    pub name: String,
    /// Its value.
    pub value: String,
}

/// A meeting's place, as events and meeting mail describe it
/// (learn.microsoft.com/graph/api/resources/location).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    /// The place's name.
    pub display_name: Option<String>,
    /// What kind of place it is (default, conference room, address, ...).
    pub location_type: Option<LocationType>,
    /// Identifier within `unique_id_type`'s namespace.
    pub unique_id: Option<String>,
    /// Where `unique_id` came from.
    pub unique_id_type: Option<LocationUniqueIdType>,
    /// Room or resource SMTP address, when the location is a bookable room.
    pub location_email_address: Option<String>,
    /// URI for the location.
    pub location_uri: Option<String>,
}

impl Location {
    /// A location identified by its display name.
    #[must_use]
    pub fn new(display_name: impl Into<String>) -> Self {
        Self {
            display_name: Some(display_name.into()),
            ..Self::default()
        }
    }
}

/// [`Location::location_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LocationType {
    /// Default/typed location.
    Default,
    /// Conference room.
    ConferenceRoom,
    /// Home address.
    HomeAddress,
    /// Business address.
    BusinessAddress,
    /// Geographic coordinates.
    GeoCoordinates,
    /// Street address.
    StreetAddress,
    /// Hotel.
    Hotel,
    /// Restaurant.
    Restaurant,
    /// Local business.
    LocalBusiness,
    /// Postal address.
    PostalAddress,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// [`Location::unique_id_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LocationUniqueIdType {
    /// Unknown.
    Unknown,
    /// Location store.
    LocationStore,
    /// Directory.
    Directory,
    /// Private/user-entered.
    Private,
    /// Bing.
    Bing,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// A mail folder.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailFolder {
    /// The folder's ID.
    pub id: Option<String>,
    /// Its name.
    pub display_name: Option<String>,
    /// The folder it's in. Top-level folders are in [`well_known::ROOT`].
    pub parent_folder_id: Option<String>,
    /// How many folders it holds.
    pub child_folder_count: Option<u32>,
    /// Unread messages in it.
    pub unread_item_count: Option<u32>,
    /// Messages in it.
    pub total_item_count: Option<u32>,
    /// Whether it's hidden.
    pub is_hidden: Option<bool>,
    /// Its subfolders, with `$expand=childFolders`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_folders: Vec<MailFolder>,
    /// In a delta: the folder was deleted.
    #[serde(rename = "@removed")]
    pub removed: Option<Removed>,
}

/// A message, or one of the `eventMessage`s meeting mail arrives as.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// `#microsoft.graph.eventMessageRequest` and the like for meeting mail; usually absent
    /// for plain mail. In a delta, every item carries it.
    #[serde(rename = "@odata.type")]
    pub odata_type: Option<String>,
    /// The message's ID: immutable, since every request asks for immutable IDs.
    pub id: Option<String>,
    /// When it was created in the mailbox.
    pub created_date_time: Option<Timestamp>,
    /// When it last changed.
    pub last_modified_date_time: Option<Timestamp>,
    /// Its version, which changes with every change.
    pub change_key: Option<String>,
    /// Its categories, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    /// When it arrived.
    pub received_date_time: Option<Timestamp>,
    /// When it was sent.
    pub sent_date_time: Option<Timestamp>,
    /// Whether it has attachments other than inline ones.
    pub has_attachments: Option<bool>,
    /// The `Message-ID` header.
    pub internet_message_id: Option<String>,
    /// The subject.
    pub subject: Option<String>,
    /// The first 255 characters of the body, as text.
    pub body_preview: Option<String>,
    /// Its importance.
    pub importance: Option<Importance>,
    /// The folder it's in.
    pub parent_folder_id: Option<String>,
    /// The conversation it belongs to, Outlook's thread.
    pub conversation_id: Option<String>,
    /// Where it sits in its conversation (base64).
    pub conversation_index: Option<String>,
    /// Whether the sender asked for a delivery receipt.
    pub is_delivery_receipt_requested: Option<bool>,
    /// Whether the sender asked for a read receipt.
    pub is_read_receipt_requested: Option<bool>,
    /// Whether it was read.
    pub is_read: Option<bool>,
    /// Whether it's a draft.
    pub is_draft: Option<bool>,
    /// Outlook on the web's link to it.
    pub web_link: Option<String>,
    /// Focused or Other.
    pub inference_classification: Option<InferenceClassificationType>,
    /// The body.
    pub body: Option<ItemBody>,
    /// The part of the body new in this message (`$select=uniqueBody`).
    pub unique_body: Option<ItemBody>,
    /// Who sent it (`Sender`).
    pub sender: Option<Recipient>,
    /// Whom it's from (`From`).
    pub from: Option<Recipient>,
    /// To.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to_recipients: Vec<Recipient>,
    /// Cc.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc_recipients: Vec<Recipient>,
    /// Bcc.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bcc_recipients: Vec<Recipient>,
    /// Reply-To.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reply_to: Vec<Recipient>,
    /// The follow-up flag.
    pub flag: Option<FollowupFlag>,
    /// Its internet headers (`$select=internetMessageHeaders`). A draft created with them
    /// sends them, for headers named `X-…`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub internet_message_headers: Vec<InternetMessageHeader>,
    /// Its attachments (`$expand=attachments`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Its open extensions (`$expand=extensions`), or a new draft's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<OpenTypeExtension>,

    /// Meeting mail: request, cancellation or response.
    pub meeting_message_type: Option<MeetingMessageType>,
    /// Meeting requests: new, updated or outdated.
    pub meeting_request_type: Option<MeetingRequestType>,
    /// Meeting mail: when the meeting starts.
    pub start_date_time: Option<DateTimeTimeZone>,
    /// Meeting mail: when it ends.
    pub end_date_time: Option<DateTimeTimeZone>,
    /// Meeting mail: where it is.
    pub location: Option<Location>,
    /// Meeting mail: one meeting, or a series or part of one.
    #[serde(rename = "type")]
    pub kind: Option<EventType>,
    /// Meeting mail: whether it lasts all day.
    pub is_all_day: Option<bool>,
    /// Meeting mail: whether a later message superseded this one.
    pub is_out_of_date: Option<bool>,
    /// Meeting mail: whether it went to a delegate.
    pub is_delegated: Option<bool>,
    /// Meeting requests: whether the organizer wants an answer.
    pub response_requested: Option<bool>,
    /// Meeting requests: whether guests may propose another time.
    pub allow_new_time_proposals: Option<bool>,
    /// Updated meeting requests: when it started before.
    pub previous_start_date_time: Option<DateTimeTimeZone>,
    /// Updated meeting requests: when it ended before.
    pub previous_end_date_time: Option<DateTimeTimeZone>,
    /// Updated meeting requests: where it was before.
    pub previous_location: Option<Location>,
    /// Meeting mail: the series' recurrence pattern and range.
    pub recurrence: Option<crate::calendar::PatternedRecurrence>,
    /// Meeting responses: an alternate time proposed by the attendee.
    pub proposed_new_time: Option<crate::calendar::TimeSlot>,
    /// Meeting responses: how the sender responded.
    pub response_type: Option<crate::calendar::ResponseType>,
    /// Meeting mail: the linked calendar event (`$expand=microsoft.graph.eventMessage/event`).
    pub event: Option<Box<crate::calendar::Event>>,

    /// In a delta: the message left the folder (deleted or moved out).
    #[serde(rename = "@removed")]
    pub removed: Option<Removed>,
}

impl Message {
    /// `$expand` expression that includes [`Self::event`] on meeting messages.
    pub const EXPAND_EVENT: &str = "microsoft.graph.eventMessage/event";

    /// Whether it's meeting mail: an `eventMessage`, `eventMessageRequest` or
    /// `eventMessageResponse`.
    #[must_use]
    pub fn is_meeting_message(&self) -> bool {
        self.odata_type
            .as_deref()
            .is_some_and(|kind| kind.starts_with("#microsoft.graph.eventMessage"))
            || self.meeting_message_type.is_some()
    }
}

/// [`Message::meeting_message_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MeetingMessageType {
    /// Not meeting mail.
    None,
    /// An invitation.
    MeetingRequest,
    /// A cancellation.
    MeetingCancelled,
    /// A guest accepted.
    MeetingAccepted,
    /// A guest accepted tentatively (Graph's spelling).
    #[serde(rename = "meetingTenativelyAccepted")]
    MeetingTentativelyAccepted,
    /// A guest declined.
    MeetingDeclined,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// [`Message::meeting_request_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MeetingRequestType {
    /// Not a request.
    None,
    /// A new invitation.
    NewMeetingRequest,
    /// The meeting changed in full.
    FullUpdate,
    /// The meeting changed in a way that needs no answer.
    InformationalUpdate,
    /// A change sent without notice.
    SilentUpdate,
    /// A later request superseded this one.
    Outdated,
    /// A copy for the delegator.
    PrincipalWantsCopy,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// [`Message::kind`] and [`crate::calendar::Event::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventType {
    /// A single meeting.
    SingleInstance,
    /// One meeting of a series.
    Occurrence,
    /// A changed meeting of a series.
    Exception,
    /// A series.
    SeriesMaster,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// A change to a message's state (learn.microsoft.com/graph/api/message-update): only the
/// properties set are sent. Unlike content, these change on received and sent mail too.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePatch {
    /// Read or unread.
    pub is_read: Option<bool>,
    /// The follow-up flag.
    pub flag: Option<FollowupFlag>,
    /// Every category it should carry (an empty list clears them).
    pub categories: Option<Vec<String>>,
    /// Focused or Other.
    pub inference_classification: Option<InferenceClassificationType>,
}

/// A message's attachment: a `fileAttachment`, or an item or reference attachment (whose
/// content isn't modeled).
#[serde_as]
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    /// [`Attachment::FILE`], `#microsoft.graph.itemAttachment` or
    /// `#microsoft.graph.referenceAttachment`.
    #[serde(rename = "@odata.type")]
    pub odata_type: Option<String>,
    /// The attachment's ID.
    pub id: Option<String>,
    /// When it last changed.
    pub last_modified_date_time: Option<Timestamp>,
    /// Its file name.
    pub name: Option<String>,
    /// Its MIME type.
    pub content_type: Option<String>,
    /// Its size in bytes.
    pub size: Option<u64>,
    /// Whether it's shown in the body (`cid:` images).
    pub is_inline: Option<bool>,
    /// File attachments: the `Content-ID` an inline image is referred to by.
    pub content_id: Option<String>,
    /// File attachments: the content. Lists leave it out unless selected.
    #[serde_as(as = "Option<Base64>")]
    pub content_bytes: Option<Vec<u8>>,
}

impl Attachment {
    /// A file attachment's `@odata.type`.
    pub const FILE: &str = "#microsoft.graph.fileAttachment";

    /// A file attachment of `content`. Graph takes up to 3 MB this way; larger files go
    /// through an upload session ([`crate::Mail::create_upload_session`]).
    #[must_use]
    pub fn file(name: &str, content_type: &str, content: Vec<u8>) -> Self {
        Self {
            odata_type: Some(Self::FILE.to_owned()),
            name: Some(name.to_owned()),
            content_type: Some(content_type.to_owned()),
            content_bytes: Some(content),
            ..Self::default()
        }
    }

    /// Whether it's a file attachment, whose bytes [`crate::Mail::attachment_content`] reads.
    #[must_use]
    pub fn is_file(&self) -> bool {
        self.odata_type.as_deref() == Some(Self::FILE)
    }
}

/// What an upload session will create (learn.microsoft.com/graph/outlook-large-attachments).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentItem {
    /// Always [`AttachmentType::File`] for mail.
    pub attachment_type: AttachmentType,
    /// The file's name.
    pub name: String,
    /// Its size in bytes, 3 MB to 150 MB.
    pub size: u64,
    /// Its MIME type.
    pub content_type: Option<String>,
    /// Whether it's shown in the body.
    pub is_inline: Option<bool>,
    /// The `Content-ID` an inline image is referred to by.
    pub content_id: Option<String>,
}

impl AttachmentItem {
    /// A file of `size` bytes.
    #[must_use]
    pub fn file(name: &str, content_type: &str, size: u64) -> Self {
        Self {
            attachment_type: AttachmentType::File,
            name: name.to_owned(),
            size,
            content_type: Some(content_type.to_owned()),
            is_inline: None,
            content_id: None,
        }
    }
}

/// [`AttachmentItem::attachment_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttachmentType {
    /// A file.
    File,
    /// An Outlook item.
    Item,
    /// A link to a file in the cloud.
    Reference,
}

/// Where to upload a large attachment's bytes ([`crate::Mail::upload`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadSession {
    /// The pre-authenticated URL the ranges go to, without the access token.
    pub upload_url: Url,
    /// When the session expires.
    pub expiration_date_time: Option<Timestamp>,
    /// The ranges still to send (`0-`).
    #[serde(default)]
    pub next_expected_ranges: Vec<String>,
}

/// An open extension on a message: an app's own named data, which Outlook's apps don't show
/// (learn.microsoft.com/graph/api/resources/opentypeextension). Each new name takes up one of
/// the mailbox's named properties for good, so names should be few and fixed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenTypeExtension {
    /// The name (`dev.exo.example`), a reverse domain name.
    pub extension_name: Option<String>,
    /// Graph's ID for it, the name with a type prefix.
    pub id: Option<String>,
    /// The app's properties: primitives or arrays of them.
    pub data: Map<String, Value>,
}

impl OpenTypeExtension {
    /// The `@odata.type` Graph creates open extensions from.
    const ODATA_TYPE: &str = "microsoft.graph.openTypeExtension";

    /// An extension named `name` holding `data`.
    #[must_use]
    pub fn new(name: &str, data: Map<String, Value>) -> Self {
        Self {
            extension_name: Some(name.to_owned()),
            id: None,
            data,
        }
    }

    /// The `$filter` finding messages that carry extension `name`, across folders or in one
    /// (learn.microsoft.com/graph/api/opentypeextension-get).
    #[must_use]
    pub fn filter(name: &str) -> String {
        format!("Extensions/any(f:f/id eq {})", crate::odata::quote(name))
    }

    /// The `$expand` reading extension `name` along with each message.
    #[must_use]
    pub fn expand(name: &str) -> String {
        format!("extensions($filter=id eq {})", crate::odata::quote(name))
    }
}

impl Serialize for OpenTypeExtension {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("@odata.type", Self::ODATA_TYPE)?;
        if let Some(name) = &self.extension_name {
            map.serialize_entry("extensionName", name)?;
        }
        if let Some(id) = &self.id {
            map.serialize_entry("id", id)?;
        }
        for (key, value) in &self.data {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// Everything but the name, the ID and OData's annotations is the app's data.
impl<'de> Deserialize<'de> for OpenTypeExtension {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut data = Map::deserialize(deserializer)?;
        let mut text = |key: &str| match data.remove(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(other) => Err(D::Error::custom(format!("{key} is {other}, not a string"))),
        };
        let extension_name = text("extensionName")?;
        let id = text("id")?;
        data.retain(|key, _| !key.starts_with('@'));
        Ok(Self {
            extension_name,
            id,
            data,
        })
    }
}

/// The body of `createReply`, `createReplyAll` and `createForward`
/// (learn.microsoft.com/graph/api/message-createreply).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplyRequest {
    /// Text put above the quoted message.
    pub comment: Option<String>,
    /// Forwards: whom to.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub to_recipients: Vec<Recipient>,
    /// Properties of the new draft that override the defaults.
    pub message: Option<Message>,
}

/// The body of `sendMail` as JSON (learn.microsoft.com/graph/api/user-sendmail).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMail {
    /// The message to send.
    pub message: Message,
    /// `false` leaves no copy in Sent Items. Graph's default is `true`.
    pub save_to_sent_items: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_meeting_request() {
        let message: Message = serde_json::from_value(serde_json::json!({
            "@odata.type": "#microsoft.graph.eventMessageRequest",
            "id": "AAkALg",
            "receivedDateTime": "2026-11-17T13:10:00Z",
            "categories": [],
            "flag": { "flagStatus": "notFlagged" },
            "inferenceClassification": "focused",
            "meetingMessageType": "meetingTenativelyAccepted",
            "meetingRequestType": "newMeetingRequest",
            "startDateTime": { "dateTime": "2026-11-25T14:00:00.0000000", "timeZone": "UTC" },
            "type": "singleInstance",
            "recurrence": null,
            "importance": "somethingNew",
        }))
        .unwrap();
        assert!(message.is_meeting_message());
        assert_eq!(
            message.meeting_message_type,
            Some(MeetingMessageType::MeetingTentativelyAccepted)
        );
        assert_eq!(message.kind, Some(EventType::SingleInstance));
        assert_eq!(message.importance, Some(Importance::UnknownFutureValue));
        assert_eq!(
            message.start_date_time.unwrap().date_time,
            "2026-11-25T14:00:00".parse::<DateTime>().unwrap()
        );
    }

    #[test]
    fn a_patch_sends_only_what_it_changes() {
        let patch = MessagePatch {
            flag: Some(FollowupFlag::new(FollowupFlagStatus::Flagged)),
            categories: Some(Vec::new()),
            ..MessagePatch::default()
        };
        assert_eq!(
            serde_json::to_value(&patch).unwrap(),
            serde_json::json!({ "flag": { "flagStatus": "flagged" }, "categories": [] })
        );
    }

    #[test]
    fn an_extensions_data_is_everything_else() {
        let extension: OpenTypeExtension = serde_json::from_value(serde_json::json!({
            "@odata.context": "https://graph.microsoft.com/v1.0/$metadata#…",
            "@odata.type": "#microsoft.graph.openTypeExtension",
            "id": "Microsoft.OutlookServices.OpenTypeExtension.dev.exo.test",
            "extensionName": "dev.exo.test",
            "until": "2026-11-20T09:00:00Z",
        }))
        .unwrap();
        assert_eq!(extension.extension_name.as_deref(), Some("dev.exo.test"));
        assert_eq!(
            extension.data.keys().collect::<Vec<_>>(),
            ["until"],
            "annotations aren't data"
        );
        let written = serde_json::to_value(OpenTypeExtension::new(
            "dev.exo.test",
            extension.data.clone(),
        ))
        .unwrap();
        assert_eq!(
            written,
            serde_json::json!({
                "@odata.type": "microsoft.graph.openTypeExtension",
                "extensionName": "dev.exo.test",
                "until": "2026-11-20T09:00:00Z",
            })
        );
        assert_eq!(
            OpenTypeExtension::filter("dev.exo.test"),
            "Extensions/any(f:f/id eq 'dev.exo.test')"
        );
    }

    #[test]
    fn attachment_bytes_are_base64() {
        let attachment = Attachment::file("a.txt", "text/plain", b"hi\xff".to_vec());
        let json = serde_json::to_value(&attachment).unwrap();
        assert_eq!(json["contentBytes"], "aGn/");
        assert_eq!(json["@odata.type"], Attachment::FILE);
        let back: Attachment = serde_json::from_value(json).unwrap();
        assert!(back.is_file());
        assert_eq!(back.content_bytes.as_deref(), Some(&b"hi\xff"[..]));
    }
}
