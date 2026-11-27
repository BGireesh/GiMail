//! Provider-neutral contacts, organization directory, and profile photo sync.
//!
//! Each account's provider syncs three lists — saved contacts, organization directory, and
//! relevant/other contacts — together with provider sync/delta tokens so later refreshes fetch
//! only what changed. The listing is stored as a JSON value in `kv`, so it needs no SQLite schema
//! change and Clear Cache drops it cleanly.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::calendar::model::CalendarProviderKind;
use crate::db::{self, KvRow};
use crate::error::Result;
use crate::store::Store;

/// Where the listing is kept in `kv`.
const KEY: &str = "contacts";
/// Legacy `kv` key read as fallback so existing caches upgrade without losing data.
const LEGACY_KEY: &str = "google_contacts";

/// One address a provider knows, with what it says about its person.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderContact {
    /// The provider's stable identifier for the person (Google resource name or Graph object id),
    /// which incremental syncs reference on update or deletion.
    pub person: String,
    /// The email address, as the provider has it.
    pub email: String,
    /// The person's display name.
    pub name: Option<String>,
    /// A photo URL (Google People) or provider photo locator (`outlook://users/{id}/photo`) for
    /// fetching the person's photo via [`Store::contact_photo`].
    pub photo: Option<String>,
    /// The person's company, when known.
    pub company: Option<String>,
    /// Their job title, when known.
    pub title: Option<String>,
}

/// A provider's cached contacts, directory, and relevant/other contacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderContacts {
    /// When the provider was last asked.
    pub listed: Timestamp,
    /// The user's saved contacts.
    pub saved: ContactList,
    /// The organization's directory (Google Workspace or Microsoft 365).
    pub directory: ContactList,
    /// Other people the user communicates with (Google "other contacts" or Graph `/me/people`).
    pub other: ContactList,
}

/// One of a provider's lists of people, as listed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactList {
    /// The provider's token or delta link for what changed since, when it gave one.
    pub sync_token: Option<String>,
    /// Everyone on the list, each address an entry.
    pub contacts: Vec<ProviderContact>,
}

/// Metadata for an account's profile photo (such as Graph `/me/photo`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhotoMetadata {
    /// The photo's size or version identifier (e.g. `240X240`).
    pub id: Option<String>,
    /// Width in pixels, when reported.
    pub width: Option<u32>,
    /// Height in pixels, when reported.
    pub height: Option<u32>,
}

impl ProviderContacts {
    /// Everyone listed, saved contacts first, then directory people, then other/relevant people.
    pub fn contacts(&self) -> impl Iterator<Item = &ProviderContact> {
        self.saved
            .contacts
            .iter()
            .chain(&self.directory.contacts)
            .chain(&self.other.contacts)
    }
}

impl Store {
    /// The cached provider contacts saved by [`save_contacts`](Self::save_contacts) or
    /// [`sync_contacts`](Self::sync_contacts), if any.
    ///
    /// # Errors
    /// On database failures.
    pub async fn contacts(&self) -> Result<Option<ProviderContacts>> {
        let mut db = self.inner.db.clone();
        if let Some(found) = db::kv_get(&mut db, KEY).await? {
            return Ok(Some(found));
        }
        db::kv_get(&mut db, LEGACY_KEY).await
    }

    /// Saves `contacts` as the latest provider contact listing.
    ///
    /// # Errors
    /// On database failures.
    pub async fn save_contacts(&self, contacts: &ProviderContacts) -> Result<()> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        db::kv_set(write.exec(), KEY, contacts).await?;
        KvRow::filter_by_key(LEGACY_KEY)
            .delete()
            .exec(write.exec())
            .await?;
        write.commit().await
    }

    /// Syncs contacts, directory, and relevant/other contacts from the account's provider (Google
    /// People or Microsoft Graph), incrementally updating from the cached listing when tokens are
    /// present, persists the result, and returns `(contacts, complete)` where `complete` is `true`
    /// if every list succeeded.
    ///
    /// # Errors
    /// On database failures, or when the provider fails fatally.
    pub async fn sync_contacts(&self) -> Result<(ProviderContacts, bool)> {
        let kept = self.contacts().await?;
        let account = self
            .inner
            .me()
            .addresses
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                CalendarProviderKind::parse_account_key(&self.inner.account)
                    .1
                    .to_owned()
            });
        let (contacts, complete) = self
            .inner
            .mail
            .sync_contacts(&account, kept.as_ref(), Timestamp::now())
            .await?;
        self.save_contacts(&contacts).await?;
        Ok((contacts, complete))
    }

    /// The signed-in user's profile photo metadata, if the provider exposes one and the account
    /// has a photo set.
    ///
    /// # Errors
    /// When the provider can't be reached or rejects the request.
    pub async fn my_photo_metadata(&self) -> Result<Option<PhotoMetadata>> {
        self.inner.mail.my_photo_metadata().await
    }

    /// Fetches profile photo bytes for a provider photo locator (such as
    /// `outlook://me/photo` or `outlook://users/{id}/photo`). Returns `Ok(None)` when the person
    /// has no photo or the locator isn't served by this provider.
    ///
    /// # Errors
    /// When the provider can't be reached or rejects the request.
    pub async fn contact_photo(&self, locator: &str) -> Result<Option<Vec<u8>>> {
        self.inner.mail.contact_photo(locator).await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn a_listing_saved_during_another_write_waits_its_turn() {
        let dir =
            std::env::temp_dir().join(format!("exo-store-contacts-turn-{}", std::process::id()));
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            crate::Remote::gmail(
                gmail::Endpoints::google(),
                gmail::StaticToken("unused".into()),
            ),
        )
        .await
        .expect("opens");
        let mut db = store.inner.db.clone();
        let mut write = store.inner.begin(&mut db).await.expect("a write");
        // It reads first, as sync's writes do, which pins the database as it read it.
        db::kv_get::<serde_json::Value>(write.exec(), KEY)
            .await
            .expect("reads");
        let saving = tokio::spawn({
            let store = store.clone();
            async move {
                let listing = ProviderContacts {
                    listed: Timestamp::now(),
                    saved: ContactList::default(),
                    directory: ContactList::default(),
                    other: ContactList::default(),
                };
                store.save_contacts(&listing).await
            }
        });
        // Long enough for a save that didn't wait to have landed.
        tokio::time::sleep(Duration::from_millis(200)).await;
        db::kv_set(write.exec(), "turn", &1)
            .await
            .expect("the write isn't refused as locked");
        write.commit().await.expect("commits");
        saving.await.expect("joins").expect("saves after it");
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn reads_legacy_google_contacts_key_and_migrates_on_save() {
        let dir =
            std::env::temp_dir().join(format!("exo-store-contacts-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            crate::Remote::gmail(
                gmail::Endpoints::google(),
                gmail::StaticToken("unused".into()),
            ),
        )
        .await
        .expect("opens");
        let legacy = ProviderContacts {
            listed: Timestamp::now(),
            saved: ContactList {
                sync_token: Some("tok1".into()),
                contacts: vec![ProviderContact {
                    person: "people/1".into(),
                    email: "ada@example.com".into(),
                    name: Some("Ada".into()),
                    photo: None,
                    company: None,
                    title: None,
                }],
            },
            directory: ContactList::default(),
            other: ContactList::default(),
        };
        let mut db = store.inner.db.clone();
        db::kv_set(&mut db, LEGACY_KEY, &legacy)
            .await
            .expect("writes legacy");
        assert_eq!(store.contacts().await.expect("reads"), Some(legacy.clone()));
        store.save_contacts(&legacy).await.expect("migrates");
        assert_eq!(
            db::kv_get::<serde_json::Value>(&mut db, LEGACY_KEY)
                .await
                .expect("reads legacy"),
            None,
            "legacy key cleaned up"
        );
        assert_eq!(store.contacts().await.expect("reads"), Some(legacy));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }
}
