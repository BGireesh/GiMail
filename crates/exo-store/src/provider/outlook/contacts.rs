//! Outlook contacts, organization directory, relevant people, and profile photos through
//! Microsoft Graph (`/me/contacts/delta`, `/users/delta`, `/me/people`, `/me/photo`,
//! `/users/{id}/photo/$value`).

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use outlook::people::{Contact, Person, User};
use outlook::{Call, Client, Page};
use serde::de::DeserializeOwned;
use url::Url;

use super::Outlook;
use crate::api::Failure;
use crate::contacts::{ContactList, PhotoMetadata, ProviderContact, ProviderContacts};
use crate::error::{ProviderError, Result};
use crate::quota::Lane;

/// Properties requested for saved contacts.
const CONTACT_SELECT: &str = "id,displayName,givenName,surname,companyName,jobTitle,emailAddresses";
/// Properties requested for directory users (omitting `companyName`, which isn't selected on all
/// directory projections).
const USER_SELECT: &str = "id,displayName,givenName,surname,mail,userPrincipalName,jobTitle";
/// Properties requested for `/me/people`.
const PERSON_SELECT: &str =
    "id,displayName,givenName,surname,userPrincipalName,scoredEmailAddresses,jobTitle,companyName";

/// Page size per people/contacts request.
const PAGE_SIZE: u32 = 100;
/// Maximum pages fetched per list so very large organizations stay bounded.
const CONTACTS_PAGES: usize = 20;
const DIRECTORY_PAGES: usize = 100;
const PEOPLE_PAGES: usize = 10;

/// Locator for the signed-in user's Outlook profile photo.
pub(crate) const ME_PHOTO_LOCATOR: &str = "outlook://me/photo";
/// Locator prefix for a directory user's Outlook profile photo (`outlook://users/{id}/photo`).
const USER_PHOTO_PREFIX: &str = "outlook://users/";
const USER_PHOTO_SUFFIX: &str = "/photo";

/// Why a delta list couldn't be read.
enum DeltaFailure {
    /// Graph expired or rejected the delta link; re-list in full.
    Expired,
    /// The account has no access to this list (e.g. `/users/delta` on a personal account).
    Unavailable,
    /// Network, throttling, auth, or other error.
    Failed,
}

/// A Graph delta resource (`Contact` or `User`) whose entries sync into a [`ContactList`].
trait DeltaItem: DeserializeOwned + Send {
    fn id(&self) -> Option<&str>;
    fn is_removed(&self) -> bool;
    fn into_entries(self) -> Vec<ProviderContact>;
}

impl DeltaItem for Contact {
    fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn is_removed(&self) -> bool {
        self.removed.is_some()
    }

    fn into_entries(self) -> Vec<ProviderContact> {
        contact_entries(self)
    }
}

impl DeltaItem for User {
    fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn is_removed(&self) -> bool {
        self.removed.is_some()
    }

    fn into_entries(self) -> Vec<ProviderContact> {
        user_entry(self).into_iter().collect()
    }
}

/// Configuration for syncing one Graph delta list.
struct DeltaSpec {
    label: &'static str,
    max_pages: usize,
    /// Whether HTTP 400 on a full list (without a delta token) means the account has no such list.
    bad_request_unavailable: bool,
}

impl Outlook {
    /// Syncs Outlook saved contacts (`/me/contacts/delta`), organization directory
    /// (`/users/delta`), and relevant people (`/me/people`). Any list that fails keeps its
    /// previous cached entries and reports `complete = false`.
    pub(super) async fn sync_contacts(
        &self,
        kept: Option<&ProviderContacts>,
        listed: Timestamp,
    ) -> Result<(ProviderContacts, bool)> {
        let kept_saved = kept.map(|kept| &kept.saved);
        let kept_directory = kept.map(|kept| &kept.directory);
        let kept_other = kept.map(|kept| &kept.other);

        let saved = self
            .sync_delta_list(
                kept_saved,
                DeltaSpec {
                    label: "Outlook contacts",
                    max_pages: CONTACTS_PAGES,
                    bad_request_unavailable: false,
                },
                |graph| {
                    graph
                        .people()
                        .contacts_delta()
                        .select(CONTACT_SELECT)
                        .max_page_size(PAGE_SIZE)
                },
            )
            .await;
        let directory = self
            .sync_delta_list(
                kept_directory,
                DeltaSpec {
                    label: "Outlook directory",
                    max_pages: DIRECTORY_PAGES,
                    bad_request_unavailable: true,
                },
                |graph| {
                    graph
                        .people()
                        .users_delta()
                        .select(USER_SELECT)
                        .max_page_size(PAGE_SIZE)
                },
            )
            .await;
        let other = self.sync_relevant_people().await;

        let complete = saved.is_some() && directory.is_some() && other.is_some();
        let contacts = ProviderContacts {
            listed,
            saved: saved.or_else(|| kept_saved.cloned()).unwrap_or_default(),
            directory: directory
                .or_else(|| kept_directory.cloned())
                .unwrap_or_default(),
            other: other.or_else(|| kept_other.cloned()).unwrap_or_default(),
        };
        Ok((contacts, complete))
    }

    /// Reads `/me/photo` metadata, returning `Ok(None)` when no photo is set (`404 ImageNotFound`).
    pub(super) async fn my_photo_metadata(&self) -> Result<Option<PhotoMetadata>> {
        match self
            .graph
            .call(Lane::Sync, 2, |graph| graph.people().photo())
            .await
        {
            Ok(photo) => Ok(Some(PhotoMetadata {
                id: photo.id,
                width: photo.width,
                height: photo.height,
            })),
            Err(failure) if failure.is_not_found() => Ok(None),
            Err(failure) => Err(failure.into()),
        }
    }

    /// Fetches profile photo bytes for `outlook://me/photo` or `outlook://users/{id}/photo`.
    pub(super) async fn contact_photo(&self, locator: &str) -> Result<Option<Vec<u8>>> {
        let result = if locator == ME_PHOTO_LOCATOR
            || locator.starts_with(&format!("{ME_PHOTO_LOCATOR}/"))
        {
            self.graph
                .call(Lane::Show, 2, |graph| graph.people().photo_content())
                .await
        } else if let Some(id) = locator
            .strip_prefix(USER_PHOTO_PREFIX)
            .and_then(|rest| rest.strip_suffix(USER_PHOTO_SUFFIX))
            .filter(|id| !id.is_empty())
        {
            self.graph
                .call(Lane::Show, 2, |graph| graph.people().user_photo_content(id))
                .await
        } else {
            return Ok(None);
        };
        match result {
            Ok(media) => Ok(Some(media.data)),
            Err(failure) if failure.is_not_found() || is_forbidden(&failure) => Ok(None),
            Err(failure) => Err(failure.into()),
        }
    }

    /// Syncs a delta-capable Graph list, falling back to a full listing whenever an existing delta
    /// token or mid-sync paging link expires or is rejected.
    async fn sync_delta_list<T: DeltaItem>(
        &self,
        kept: Option<&ContactList>,
        spec: DeltaSpec,
        start: impl Fn(&Client) -> Call<'_, Page<T>>,
    ) -> Option<ContactList> {
        if let Some(kept) = kept
            && let Some(link) = kept
                .sync_token
                .as_deref()
                .and_then(|token| Url::parse(token).ok())
        {
            match self.delta_pages(Some(&link), &spec, &start).await {
                Ok((changed, token)) => {
                    return Some(apply_delta(Some(kept), changed, token));
                }
                Err(DeltaFailure::Expired) => {
                    log::info!("listing {} again: delta token expired", spec.label);
                }
                Err(DeltaFailure::Unavailable) => return Some(ContactList::default()),
                Err(DeltaFailure::Failed) => return None,
            }
        }
        let (all, token) = match self.delta_pages(None, &spec, &start).await {
            Ok(result) => result,
            Err(DeltaFailure::Unavailable) => return Some(ContactList::default()),
            Err(DeltaFailure::Expired | DeltaFailure::Failed) => return None,
        };
        Some(apply_delta(None, all, token))
    }

    async fn delta_pages<T: DeltaItem>(
        &self,
        delta_link: Option<&Url>,
        spec: &DeltaSpec,
        start: &impl Fn(&Client) -> Call<'_, Page<T>>,
    ) -> std::result::Result<(Vec<T>, Option<String>), DeltaFailure> {
        let mut next: Option<Url> = delta_link.cloned();
        let mut items = Vec::new();
        for _ in 0..spec.max_pages {
            let page = match &next {
                Some(link) => {
                    self.graph
                        .call(Lane::Sync, 2, |graph| {
                            graph.follow::<T>(link).max_page_size(PAGE_SIZE)
                        })
                        .await
                }
                None => self.graph.call(Lane::Sync, 2, start).await,
            };
            match page {
                Ok(page) => {
                    items.extend(page.value);
                    match page.next_link {
                        Some(link) => next = Some(link),
                        None => return Ok((items, page.delta_link.map(String::from))),
                    }
                }
                // Check stale delta / nextLink expiry and 400 before treating 400 as unavailable
                // so a stale `$deltatoken` re-lists instead of wiping the cached directory.
                Err(failure)
                    if delta_link.is_some()
                        && (is_expired(&failure) || is_bad_request(&failure)) =>
                {
                    return Err(DeltaFailure::Expired);
                }
                Err(failure)
                    if is_forbidden(&failure)
                        || (spec.bad_request_unavailable && is_bad_request(&failure)) =>
                {
                    log::debug!("{} unavailable: {}", spec.label, failure.message());
                    return Err(DeltaFailure::Unavailable);
                }
                Err(failure) => {
                    log::warn!("listing {}: {}", spec.label, failure.message());
                    return Err(DeltaFailure::Failed);
                }
            }
        }
        Ok((items, None))
    }

    async fn sync_relevant_people(&self) -> Option<ContactList> {
        let mut next: Option<Url> = None;
        let mut contacts = Vec::new();
        for _ in 0..PEOPLE_PAGES {
            let page = match &next {
                Some(link) => {
                    self.graph
                        .call(Lane::Sync, 2, |graph| graph.follow::<Person>(link))
                        .await
                }
                None => {
                    self.graph
                        .call(Lane::Sync, 2, |graph| {
                            graph
                                .people()
                                .relevant()
                                .select(PERSON_SELECT)
                                .top(PAGE_SIZE)
                        })
                        .await
                }
            };
            match page {
                Ok(page) => {
                    contacts.extend(page.value.into_iter().flat_map(person_entries));
                    match page.next_link {
                        Some(link) => next = Some(link),
                        None => {
                            return Some(ContactList {
                                sync_token: None,
                                contacts,
                            });
                        }
                    }
                }
                Err(failure) if is_forbidden(&failure) || is_bad_request(&failure) => {
                    log::debug!("Outlook relevant people unavailable: {}", failure.message());
                    return Some(ContactList::default());
                }
                Err(failure) => {
                    log::warn!("listing Outlook relevant people: {}", failure.message());
                    return None;
                }
            }
        }
        Some(ContactList {
            sync_token: None,
            contacts,
        })
    }
}

/// Collapses repeated IDs across delta pages so the last occurrence of each ID wins (preventing
/// duplicate entries and preventing earlier snapshots from resurrecting `@removed` items).
fn collapse_delta<T: DeltaItem>(items: Vec<T>) -> Vec<T> {
    let mut last_by_id: HashMap<String, usize> = HashMap::new();
    for (idx, item) in items.iter().enumerate() {
        if let Some(id) = item.id() {
            last_by_id.insert(id.to_owned(), idx);
        }
    }
    items
        .into_iter()
        .enumerate()
        .filter(|(idx, item)| item.id().is_none_or(|id| last_by_id.get(id) == Some(idx)))
        .map(|(_, item)| item)
        .collect()
}

/// Applies a Graph delta page set to `kept` (`None` for a full listing).
fn apply_delta<T: DeltaItem>(
    kept: Option<&ContactList>,
    changed: Vec<T>,
    sync_token: Option<String>,
) -> ContactList {
    let collapsed = collapse_delta(changed);
    let mut contacts = match kept {
        Some(kept) => {
            let ids: HashSet<&str> = collapsed.iter().filter_map(DeltaItem::id).collect();
            kept.contacts
                .iter()
                .filter(|contact| !ids.contains(contact.person.as_str()))
                .cloned()
                .collect()
        }
        None => Vec::new(),
    };
    contacts.extend(
        collapsed
            .into_iter()
            .filter(|item| !item.is_removed())
            .flat_map(DeltaItem::into_entries),
    );
    ContactList {
        sync_token,
        contacts,
    }
}

/// Converts a Graph `Contact` into one [`ProviderContact`] per non-empty email address.
fn contact_entries(contact: Contact) -> Vec<ProviderContact> {
    let person = contact.id.unwrap_or_default();
    let name = display_name(
        contact.display_name.as_deref(),
        contact.given_name.as_deref(),
        contact.surname.as_deref(),
    );
    let company = non_empty(contact.company_name);
    let title = non_empty(contact.job_title);
    let mut seen = HashSet::new();
    contact
        .email_addresses
        .into_iter()
        .filter_map(|entry| entry.address)
        .map(|email| email.trim().to_owned())
        .filter(|email| !email.is_empty() && seen.insert(email.to_ascii_lowercase()))
        .map(|email| ProviderContact {
            person: person.clone(),
            email,
            name: name.clone(),
            photo: None,
            company: company.clone(),
            title: title.clone(),
        })
        .collect()
}

/// Converts a Graph directory `User` into a [`ProviderContact`].
fn user_entry(user: User) -> Option<ProviderContact> {
    let email = [user.mail.as_deref(), user.user_principal_name.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|address| !address.is_empty() && address.contains('@'))?
        .to_owned();
    let person = user.id.clone().unwrap_or_else(|| email.clone());
    let photo = user
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(|id| format!("{USER_PHOTO_PREFIX}{id}{USER_PHOTO_SUFFIX}"));
    let name = display_name(
        user.display_name.as_deref(),
        user.given_name.as_deref(),
        user.surname.as_deref(),
    );
    Some(ProviderContact {
        person,
        email,
        name,
        photo,
        company: None,
        title: non_empty(user.job_title),
    })
}

/// Converts a Graph `/me/people` `Person` into one [`ProviderContact`] per scored address.
fn person_entries(person: Person) -> Vec<ProviderContact> {
    let id = person.id.unwrap_or_default();
    let name = display_name(
        person.display_name.as_deref(),
        person.given_name.as_deref(),
        person.surname.as_deref(),
    );
    let company = non_empty(person.company_name);
    let title = non_empty(person.job_title);
    let mut emails: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for scored in person.scored_email_addresses {
        if let Some(address) = scored
            .address
            .as_deref()
            .map(str::trim)
            .filter(|address| !address.is_empty())
            && seen.insert(address.to_ascii_lowercase())
        {
            emails.push(address.to_owned());
        }
    }
    if emails.is_empty()
        && let Some(upn) = person
            .user_principal_name
            .as_deref()
            .map(str::trim)
            .filter(|address| !address.is_empty() && address.contains('@'))
    {
        emails.push(upn.to_owned());
    }
    emails
        .into_iter()
        .map(|email| ProviderContact {
            person: id.clone(),
            email,
            name: name.clone(),
            photo: None,
            company: company.clone(),
            title: title.clone(),
        })
        .collect()
}

fn display_name(
    display: Option<&str>,
    given: Option<&str>,
    surname: Option<&str>,
) -> Option<String> {
    if let Some(name) = display.map(str::trim).filter(|name| !name.is_empty()) {
        return Some(name.to_owned());
    }
    let parts: Vec<&str> = [given, surname]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn is_expired(failure: &Failure) -> bool {
    matches!(
        failure,
        Failure::Permanent(ProviderError::Outlook(error))
            if error.status() == Some(410) || error.code() == Some("SyncStateNotFound")
    )
}

fn is_forbidden(failure: &Failure) -> bool {
    matches!(
        failure,
        Failure::Permanent(ProviderError::Outlook(error)) if error.status() == Some(403)
    )
}

fn is_bad_request(failure: &Failure) -> bool {
    matches!(
        failure,
        Failure::Permanent(ProviderError::Outlook(error)) if error.status() == Some(400)
    )
}

#[cfg(test)]
mod tests {
    use outlook::EmailAddress;
    use outlook::odata::{Removed, RemovedReason};

    use super::*;

    fn contact(id: &str, name: &str, email: &str) -> Contact {
        Contact {
            id: Some(id.into()),
            display_name: Some(name.into()),
            email_addresses: vec![EmailAddress {
                name: Some(name.into()),
                address: Some(email.into()),
            }],
            ..Contact::default()
        }
    }

    fn removed_contact(id: &str) -> Contact {
        Contact {
            id: Some(id.into()),
            removed: Some(Removed {
                reason: RemovedReason::Deleted,
            }),
            ..Contact::default()
        }
    }

    #[test]
    fn apply_delta_collapses_repeated_ids_without_duplicates_or_resurrection() {
        let kept = ContactList {
            sync_token: Some("old".into()),
            contacts: vec![
                ProviderContact {
                    person: "c1".into(),
                    email: "old@example.com".into(),
                    name: Some("Old".into()),
                    photo: None,
                    company: None,
                    title: None,
                },
                ProviderContact {
                    person: "c2".into(),
                    email: "stay@example.com".into(),
                    name: Some("Stay".into()),
                    photo: None,
                    company: None,
                    title: None,
                },
            ],
        };
        let delta = vec![
            contact("c1", "Updated then deleted", "mid@example.com"),
            removed_contact("c1"),
            contact("c3", "V1", "v1@example.com"),
            contact("c3", "V2", "v2@example.com"),
            removed_contact("c4"),
            contact("c4", "Recreated", "c4@example.com"),
        ];

        let incremental = apply_delta(Some(&kept), delta.clone(), Some("new".into()));
        let emails: Vec<&str> = incremental
            .contacts
            .iter()
            .map(|c| c.email.as_str())
            .collect();
        assert_eq!(
            emails,
            ["stay@example.com", "v2@example.com", "c4@example.com"]
        );

        let full = apply_delta(None, delta, Some("full".into()));
        let full_emails: Vec<&str> = full.contacts.iter().map(|c| c.email.as_str()).collect();
        assert_eq!(full_emails, ["v2@example.com", "c4@example.com"]);
    }
}
