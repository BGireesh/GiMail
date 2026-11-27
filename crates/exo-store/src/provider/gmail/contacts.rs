//! Google People sync for Gmail accounts: saved contacts, Workspace directory (for non-consumer
//! domains), and "other contacts", with incremental sync tokens and expired-token fallback.

use std::collections::HashSet;

use gmail::people::{FieldMetadata, PeopleQuery, Person};
use jiff::Timestamp;

use super::Gmail;
use crate::api::Failure as ApiFailure;
use crate::contacts::{ContactList, PhotoMetadata, ProviderContact, ProviderContacts};
use crate::error::{ProviderError, Result};

/// Maximum pages of 1,000 fetched for saved/other contacts and Workspace directory.
const GOOGLE_PAGES: usize = 5;
const DIRECTORY_PAGES: usize = 100;
/// Fields read for saved contacts and directory profiles, and for other contacts.
const DETAILS: &str = "names,emailAddresses,photos,organizations,metadata";
const NAMES: &str = "names,emailAddresses,metadata";

/// One of Google People's lists.
#[derive(Clone, Copy, PartialEq, Eq)]
enum List {
    Saved,
    Other,
    Directory,
}

impl List {
    fn name(self) -> &'static str {
        match self {
            Self::Saved => "contacts",
            Self::Other => "other contacts",
            Self::Directory => "directory",
        }
    }
}

/// Why a Google People list couldn't be read.
enum ListFailure {
    /// Google expired the sync token; list in full.
    Expired,
    /// Network, throttling, auth, or other error.
    Failed,
}

impl Gmail {
    /// Syncs Google saved contacts, Workspace directory (for non-consumer domains), and other
    /// contacts. Any list that fails preserves its previous cached state and sets `complete = false`.
    pub(super) async fn sync_contacts(
        &self,
        account: &str,
        kept: Option<&ProviderContacts>,
        listed: Timestamp,
    ) -> Result<(ProviderContacts, bool)> {
        let kept_saved = kept.map(|kept| &kept.saved);
        let kept_directory = kept.map(|kept| &kept.directory);
        let kept_other = kept.map(|kept| &kept.other);

        let saved = self.update_list(List::Saved, kept_saved).await;
        let directory = if is_workspace(account) {
            self.update_list(List::Directory, kept_directory).await
        } else {
            Some(ContactList::default())
        };
        let other = self.update_list(List::Other, kept_other).await;

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

    /// Google profile photos are served via direct URLs in People/userinfo rather than a separate
    /// metadata endpoint.
    pub(super) async fn my_photo_metadata(&self) -> Result<Option<PhotoMetadata>> {
        Ok(None)
    }

    /// Google contact photos are plain HTTPS URLs carried on [`ProviderContact::photo`].
    pub(super) async fn contact_photo(&self, _locator: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    async fn update_list(&self, list: List, kept: Option<&ContactList>) -> Option<ContactList> {
        if let Some(kept) = kept
            && let Some(token) = &kept.sync_token
        {
            match self.pages(list, Some(token)).await {
                Ok((changed, sync_token)) => return Some(apply(kept, list, changed, sync_token)),
                Err(ListFailure::Expired) => {
                    log::info!(
                        "listing Google {} again: its sync token expired",
                        list.name()
                    );
                }
                Err(ListFailure::Failed) => return None,
            }
        }
        let (people, sync_token) = self.pages(list, None).await.ok()?;
        let contacts = people
            .into_iter()
            .filter(|person| !is_deleted(person))
            .flat_map(|person| entries(person, list))
            .collect();
        Some(ContactList {
            sync_token,
            contacts,
        })
    }

    async fn pages(
        &self,
        list: List,
        sync_token: Option<&str>,
    ) -> std::result::Result<(Vec<Person>, Option<String>), ListFailure> {
        let mut query = PeopleQuery {
            page_size: Some(1000),
            request_sync_token: true,
            sync_token: sync_token.map(str::to_owned),
            ..PeopleQuery::default()
        };
        let max_pages = if list == List::Directory {
            DIRECTORY_PAGES
        } else {
            GOOGLE_PAGES
        };
        let mut people = Vec::new();
        for _ in 0..max_pages {
            let page = match list {
                List::Saved => self
                    .api
                    .request(1, |client| client.people().connections(DETAILS, &query))
                    .await
                    .map(|page| (page.connections, page.next_page_token, page.next_sync_token)),
                List::Other => self
                    .api
                    .request(1, |client| client.people().other_contacts(NAMES, &query))
                    .await
                    .map(|page| {
                        (
                            page.other_contacts,
                            page.next_page_token,
                            page.next_sync_token,
                        )
                    }),
                List::Directory => self
                    .api
                    .request(1, |client| client.people().directory(DETAILS, &query))
                    .await
                    .map(|page| (page.people, page.next_page_token, page.next_sync_token)),
            };
            match page {
                Ok((page, next, token)) => {
                    people.extend(page);
                    match next {
                        Some(next) => query.page_token = Some(next),
                        None => return Ok((people, token)),
                    }
                }
                Err(ApiFailure::Permanent(ProviderError::Gmail(error)))
                    if sync_token.is_some() && matches!(error.status(), Some(400 | 410)) =>
                {
                    return Err(ListFailure::Expired);
                }
                Err(ApiFailure::Permanent(ProviderError::Gmail(error)))
                    if list == List::Directory && matches!(error.status(), Some(400 | 403)) =>
                {
                    log::debug!("no Workspace directory to list: {error}");
                    return Ok((Vec::new(), None));
                }
                Err(failure) => {
                    log::warn!("listing Google {}: {}", list.name(), failure.message());
                    return Err(ListFailure::Failed);
                }
            }
        }
        Ok((people, None))
    }
}

/// Whether `account` is a Workspace account (outside Gmail's consumer domains).
fn is_workspace(account: &str) -> bool {
    let domain = account
        .rsplit_once('@')
        .map(|(_, domain)| domain.to_lowercase());
    !matches!(domain.as_deref(), Some("gmail.com" | "googlemail.com"))
}

fn apply(
    kept: &ContactList,
    list: List,
    changed: Vec<Person>,
    sync_token: Option<String>,
) -> ContactList {
    let names: HashSet<&str> = changed
        .iter()
        .filter_map(|person| person.resource_name.as_deref())
        .collect();
    let mut contacts: Vec<ProviderContact> = kept
        .contacts
        .iter()
        .filter(|contact| !names.contains(contact.person.as_str()))
        .cloned()
        .collect();
    contacts.extend(
        changed
            .into_iter()
            .filter(|person| !is_deleted(person))
            .flat_map(|person| entries(person, list)),
    );
    ContactList {
        sync_token,
        contacts,
    }
}

fn is_deleted(person: &Person) -> bool {
    person
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.deleted)
        == Some(true)
}

fn entries(person: Person, list: List) -> impl Iterator<Item = ProviderContact> {
    let name = primary(&person.names, |name| name.metadata.as_ref())
        .and_then(|name| name.display_name.clone())
        .filter(|name| !name.is_empty());
    let details = list != List::Other;
    let photo = primary(&person.photos, |photo| photo.metadata.as_ref())
        .filter(|photo| details && photo.default != Some(true))
        .and_then(|photo| photo.url.clone());
    let organization = primary(&person.organizations, |organization| {
        organization.metadata.as_ref()
    })
    .filter(|_| details);
    let text = |text: Option<&String>| text.filter(|text| !text.is_empty()).cloned();
    let company = text(organization.and_then(|organization| organization.name.as_ref()));
    let title = text(organization.and_then(|organization| organization.title.as_ref()));
    let resource = person.resource_name.unwrap_or_default();
    person
        .email_addresses
        .into_iter()
        // A saved contact linked to a colleague lists the colleague's addresses too, which the
        // directory has.
        .filter(move |address| {
            list != List::Saved
                || address
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.source.as_ref()?.kind.as_deref())
                    != Some("DOMAIN_PROFILE")
        })
        .filter_map(|address| address.value)
        .map(move |email| ProviderContact {
            person: resource.clone(),
            email,
            name: name.clone(),
            photo: photo.clone(),
            company: company.clone(),
            title: title.clone(),
        })
}

fn primary<T>(values: &[T], metadata: impl Fn(&T) -> Option<&FieldMetadata>) -> Option<&T> {
    values
        .iter()
        .find(|value| metadata(value).and_then(|metadata| metadata.primary) == Some(true))
        .or(values.first())
}
