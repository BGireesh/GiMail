//! The people Google lists for the account (its contacts, "other contacts" and, on Workspace, the
//! directory), as the app last listed them, with Google's tokens for what changed since. The app
//! asks Google for those changes when the listing is stale; keeping it lets the next launch
//! suggest them, and show their photos and companies, before Google answers. They're a JSON value in `kv`, so they need no schema change, and Clear Cache drops
//! them since Google has them.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::db;
use crate::error::Result;
use crate::store::Store;

/// Where the listing is kept in `kv`.
const KEY: &str = "google_contacts";

/// One address Google knows, with what it says about its person.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleContact {
    /// Google's name for the person (`people/c123`, `otherContacts/c123`), which its changes name.
    pub person: String,
    /// The address, as Google has it.
    pub email: String,
    /// The person's display name.
    pub name: Option<String>,
    /// The URL of the person's own photo (not Google's generated one), for a saved contact or a
    /// directory person.
    pub photo: Option<String>,
    /// The person's company, for a saved contact or a directory person.
    pub company: Option<String>,
    /// Their job title there.
    pub title: Option<String>,
}

/// A listing of Google contacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleContacts {
    /// When Google was asked.
    pub listed: Timestamp,
    /// The user's contacts.
    pub saved: GoogleList,
    /// A Workspace's directory: its domain contacts and profiles.
    pub directory: GoogleList,
    /// The people the user has written to ("other contacts").
    pub other: GoogleList,
}

/// One of Google's lists of people, as listed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleList {
    /// Google's token for what changed since, when it gave one.
    pub sync_token: Option<String>,
    /// Everyone on it, each address an entry.
    pub contacts: Vec<GoogleContact>,
}

impl GoogleContacts {
    /// Everyone listed, saved contacts first, then directory people, then "other contacts".
    pub fn contacts(&self) -> impl Iterator<Item = &GoogleContact> {
        self.saved
            .contacts
            .iter()
            .chain(&self.directory.contacts)
            .chain(&self.other.contacts)
    }
}

impl Store {
    /// The Google contacts saved by [`save_google_contacts`](Self::save_google_contacts), if any.
    ///
    /// # Errors
    /// On database failures.
    pub async fn google_contacts(&self) -> Result<Option<GoogleContacts>> {
        let mut db = self.inner.db.clone();
        db::kv_get(&mut db, KEY).await
    }

    /// Saves `contacts` as the latest listing.
    ///
    /// # Errors
    /// On database failures.
    pub async fn save_google_contacts(&self, contacts: &GoogleContacts) -> Result<()> {
        let mut db = self.inner.db.clone();
        db::kv_set(&mut db, KEY, contacts).await
    }
}
