//! Exo's offline-first data layer: one SQLite file per account caching its mail (Gmail's, or
//! Outlook's through Microsoft Graph), the sync engine that keeps it current, and the outbox that
//! sends the user's triage actions.
//!
//! ```no_run
//! # async fn demo(dir: &std::path::Path) -> exo_store::Result<()> {
//! use exo_store::{ListId, Mutation, Remote, Store};
//!
//! let store = Store::open(
//!     dir,
//!     "maya@brightmoor.co",
//!     Remote::gmail(gmail::Endpoints::google(), gmail::StaticToken("ya29...".into())),
//! )
//! .await?;
//! while store.sync().await?.backfilling {}
//! let inbox = store.list(&ListId::Inbox, None, 50).await?;
//! if let Some(first) = inbox.threads.first() {
//!     let undo = store.apply(&[first.id.clone()], Mutation::MarkDone).await?;
//!     store.flush().await?;
//!     store.undo(undo).await?;
//! }
//! # Ok(()) }
//! ```

mod address_book;
mod api;
mod authentication;
mod brands;
mod bulk;
mod bulk_log;
mod calendar;
mod connection;
mod contacts;
mod db;
mod drafts;
mod entities;
mod error;
mod evict;
mod ids;
mod migrations;
mod model;
mod mutation;
mod onboarding;
mod out_of_office;
mod outbox;
mod parts;
mod provider;
mod query;
mod queue;
mod quota;
mod reminders;
mod search;
mod settings;
mod source;
mod split;
mod statistics;
mod store;
mod sync;
mod text;
mod truth;
mod unsubscribes;
mod view;

pub use address_book::ScoredContact;
pub use brands::{BrandCheck, BrandRecord};
pub use bulk::{BulkTarget, ListCutoff, Progress, Relabelled, Scope, SenderRule, SenderRuleUndo};
pub use bulk_log::{BulkRecord, BulkState, LoggedBulk};
pub use calendar::model::{
    AccessRole, AccountCalendar, Calendar, CalendarEvent, CalendarProviderKind, Conference,
    ConferenceKind, ConferencePatch, EventAttachment, EventAttendee, EventCategory, EventPatch,
    EventPerson, EventReminders, EventStatus, EventTime, EventType, FreeBusySlot,
    OnlineMeetingKind, RsvpStatus, ShowAs, StoredEvent, Visibility,
};
pub use calendar::outbox::{
    CalendarChange, CalendarOutcome, Cut, InviteLinks, QueuedCalendarChange, Refusal,
};
pub use calendar::provider::CalendarRemote;
pub use calendar::recurrence::{End as RecurrenceEnd, Frequency, Nth, Recurrence};
pub use connection::{Connection, Debounce, Outage};
pub use contacts::{ContactList, PhotoMetadata, ProviderContact, ProviderContacts};
pub use drafts::{
    Address, AttachmentSource, Draft, DraftAction, DraftAttachment, DraftFailure, DraftRecord,
    DraftReminder, DraftReport, SendFailure, SendLater, SentDraft, StoredDraft, Upload, Uploads,
};
pub use error::{Error, Failing, ProviderError, Result, Unusable};
pub use evict::{Evicted, Eviction};
pub use ids::{Filter, ListId, ThreadId, labels};
pub use model::{
    Alias, Attachment, Attachments, Body, Contact, Cursor, Header, Label, LabelKind, ListCount,
    MessageDetail, RecentThread, ReminderStatus, ThreadDetail, ThreadPage, ThreadSummary,
};
pub use mutation::Mutation;
pub use onboarding::{PreviewEmail, PreviewList, SplitPreview};
pub use out_of_office::{
    AutoReply, AutoReplyStatus, DetectedEvent, DetectedEventTime, ExternalAudience,
    GoogleResponder, OUT_OF_OFFICE_LOOKAHEAD, OutOfOfficeDays, OutOfOfficeEventDetection,
    OutOfOfficeResponder, OutlookAutoReply, OutlookResponder,
};
pub use outbox::{FailedMutation, FlushReport, Undo, Unsent};
pub use parts::PartsCache;
pub use query::{LastToken, SearchQuery, TokenKind};
pub use quota::Quota;
pub use search::{GmailPage, Highlighted, Search, SearchPage, SearchResult};
pub use settings::{
    AdditionalTimezone, AutoAdvance, CalendarAccount, CalendarEventNotifications,
    CalendarNotifications, CalendarSetting, CalendarSettings, DeviceSettings, Document,
    EmojiSkinTone, MeetingLinks, Notifications, Nudge, PreviousDropdown, PreviousReminder,
    PreviousSchedule, PreviousSuggestion, QuickTips, ReminderSettings, RemoteContentBlocking,
    SearchHistoryItem, SearchHistoryKind, Settings, Snippet, SnippetAnalytics, SnippetSend,
    SplitsEngagement, Theme, TriggerCondition, TypingAssistance,
};
pub use split::{
    AutoLabels, CALENDAR_QUERY, Definitions, Invalidity, LabelIndex, LabelRef, LibrarySection,
    Matcher, Me, Preview, PreviewThread, Properties, QueryProperty, SPLIT_ID_PREFIX, Section,
    Senders, SplitChanges, SplitEntry, SplitInbox, SplitLibrary, SplitQuery, SystemSplit, Template,
    clean_whitespace, gmail_search, is_calendar_name, is_reserved_name, label_slug, name_error,
    query_too_long, slugify_name,
};
pub use statistics::{Achievements, Statistics};
pub use store::{Options, Remote, Store};
pub use sync::{Arrival, RECENT_THREADS, SyncProgress, SyncReport};
pub use text::{IndexText, TextSource};
pub use unsubscribes::PendingUnsubscribe;
