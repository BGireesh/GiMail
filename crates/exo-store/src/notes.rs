//! Thread notes: the user's own notes on a conversation or one of its messages, kept on this
//! device only. They never reach Gmail.

use serde::{Deserialize, Serialize};

/// A name and address of one of a noted thread's participants, as the thread had them when a
/// note last changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteParticipant {
    pub name: Option<String>,
    pub email: String,
}

/// A file attached to a note. Its bytes are kept under its `uuid`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteFile {
    pub uuid: String,
    pub name: String,
    pub mime_type: String,
    pub size: u64,
    /// Set for an inline image, which the note's HTML shows by `cid:`.
    pub content_id: Option<String>,
}
