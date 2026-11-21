//! Google's lists of an account's people: its contacts, a Workspace's directory and "other
//! contacts". Each is listed in full the first time, with Google's token for what changes after,
//! and from then on only those changes are asked for, until Google expires the token and the
//! list is listed in full again.

use std::collections::HashSet;

use exo_store::{GoogleContact, GoogleContacts, GoogleList, Store};
use gmail::people::{FieldMetadata, PeopleQuery, Person};

/// The most pages of a thousand that are listed of each of Google's lists, and of a Workspace's
/// directory, which stops at a hundred thousand people.
const GOOGLE_PAGES: usize = 5;
const DIRECTORY_PAGES: usize = 100;
/// The fields read of saved contacts and directory people, and of "other contacts". `metadata`
/// says which of a sync's people were deleted.
const DETAILS: &str = "names,emailAddresses,photos,organizations,metadata";
const NAMES: &str = "names,emailAddresses,metadata";

/// One of Google's lists of people.
#[derive(Clone, Copy, PartialEq, Eq)]
enum List {
    /// The user's contacts.
    Saved,
    /// People they've written to (`otherContacts`).
    Other,
    /// A Workspace's domain contacts and profiles.
    Directory,
}

impl List {
    fn name(self) -> &'static str {
        match self {
            List::Saved => "contacts",
            List::Other => "other contacts",
            List::Directory => "directory",
        }
    }
}

/// Why a list couldn't be had.
enum Failure {
    /// Google no longer knows the sync token, so the list is listed in full.
    Expired,
    /// Google didn't answer every page.
    Failed,
}

/// `account`'s contacts, its Workspace colleagues (for an account outside Gmail's own domains) and
/// its "other contacts", each list brought up to date from `kept` when that has its token, and
/// whether all of them were. A list that fails keeps what `kept` has of it. A listing that gets no
/// answer shows the offline bar.
pub(crate) async fn list(
    store: &Store,
    account: &str,
    kept: Option<&GoogleContacts>,
    listed: jiff::Timestamp,
) -> (GoogleContacts, bool) {
    let saved = kept.map(|kept| &kept.saved);
    let directory = kept.map(|kept| &kept.directory);
    let other = kept.map(|kept| &kept.other);
    let lists = [
        (update(store, List::Saved, saved).await, saved),
        if is_workspace(account) {
            (update(store, List::Directory, directory).await, directory)
        } else {
            (Some(GoogleList::default()), None)
        },
        (update(store, List::Other, other).await, other),
    ];
    let complete = lists.iter().all(|(listed, _)| listed.is_some());
    let [saved, directory, other] =
        lists.map(|(listed, kept)| listed.or_else(|| kept.cloned()).unwrap_or_default());
    let contacts = GoogleContacts {
        listed,
        saved,
        directory,
        other,
    };
    (contacts, complete)
}

/// Whether `account` is a Workspace account, which has a directory: any outside Gmail's own
/// domains.
fn is_workspace(account: &str) -> bool {
    let domain = account
        .rsplit_once('@')
        .map(|(_, domain)| domain.to_lowercase());
    !matches!(domain.as_deref(), Some("gmail.com" | "googlemail.com"))
}

/// `list` as Google has it now: `kept` with the changes since its token, else listed in full.
/// `None` when Google didn't answer.
async fn update(store: &Store, list: List, kept: Option<&GoogleList>) -> Option<GoogleList> {
    if let Some(kept) = kept
        && let Some(token) = &kept.sync_token
    {
        match pages(store, list, Some(token)).await {
            Ok((changed, sync_token)) => return Some(apply(kept, list, changed, sync_token)),
            Err(Failure::Expired) => {
                log::info!(
                    "listing Google {} again: its sync token expired",
                    list.name()
                );
            }
            Err(Failure::Failed) => return None,
        }
    }
    let (people, sync_token) = pages(store, list, None).await.ok()?;
    let contacts = people
        .into_iter()
        .filter(|person| !is_deleted(person))
        .flat_map(|person| entries(person, list))
        .collect();
    Some(GoogleList {
        sync_token,
        contacts,
    })
}

/// `kept` with `changed` (People's answer to its token: everyone added, changed or deleted since)
/// applied, and Google's new token.
fn apply(
    kept: &GoogleList,
    list: List,
    changed: Vec<Person>,
    sync_token: Option<String>,
) -> GoogleList {
    let names: HashSet<&str> = changed
        .iter()
        .filter_map(|person| person.resource_name.as_deref())
        .collect();
    let mut contacts: Vec<GoogleContact> = kept
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
    GoogleList {
        sync_token,
        contacts,
    }
}

/// Every page of `list`, or of what changed on it since `sync_token`, and Google's token for what
/// changes next. A Workspace that keeps its directory from its people has an empty one.
async fn pages(
    store: &Store,
    list: List,
    sync_token: Option<&str>,
) -> Result<(Vec<Person>, Option<String>), Failure> {
    // A token's pages ask what the first page asked, which asked for a token.
    let mut query = PeopleQuery {
        page_size: Some(1000),
        request_sync_token: true,
        sync_token: sync_token.map(str::to_owned),
        ..PeopleQuery::default()
    };
    let pages = if list == List::Directory {
        DIRECTORY_PAGES
    } else {
        GOOGLE_PAGES
    };
    let mut people = Vec::new();
    for _ in 0..pages {
        let page = match list {
            List::Saved => store
                .google(1, |client| client.people().connections(DETAILS, &query))
                .await
                .map(|page| (page.connections, page.next_page_token, page.next_sync_token)),
            List::Other => store
                .google(1, |client| client.people().other_contacts(NAMES, &query))
                .await
                .map(|page| {
                    (
                        page.other_contacts,
                        page.next_page_token,
                        page.next_sync_token,
                    )
                }),
            List::Directory => store
                .google(1, |client| client.people().directory(DETAILS, &query))
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
            Err(exo_store::Error::Gmail(error))
                if sync_token.is_some() && matches!(error.status(), Some(400 | 410)) =>
            {
                return Err(Failure::Expired);
            }
            Err(exo_store::Error::Gmail(error))
                if list == List::Directory && matches!(error.status(), Some(400 | 403)) =>
            {
                log::debug!("no Workspace directory to list: {error}");
                return Ok((Vec::new(), None));
            }
            Err(error) => {
                log::warn!("listing Google {}: {error}", list.name());
                return Err(Failure::Failed);
            }
        }
    }
    // The pages ran out before the last, which has the token, so the next listing is a full one.
    Ok((people, None))
}

/// Whether a sync answer says `person` was deleted.
fn is_deleted(person: &Person) -> bool {
    person
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.deleted)
        == Some(true)
}

/// A listed person's addresses, each with their name and, from a saved contact or a colleague,
/// their own photo, company and title.
fn entries(person: Person, list: List) -> impl Iterator<Item = GoogleContact> {
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
        .map(move |email| GoogleContact {
            person: resource.clone(),
            email,
            name: name.clone(),
            photo: photo.clone(),
            company: company.clone(),
            title: title.clone(),
        })
}

/// The value Google marks primary, else the first.
fn primary<T>(values: &[T], metadata: impl Fn(&T) -> Option<&FieldMetadata>) -> Option<&T> {
    values
        .iter()
        .find(|value| metadata(value).and_then(|metadata| metadata.primary) == Some(true))
        .or(values.first())
}

#[cfg(test)]
mod tests {
    use gmail::people::{EmailAddress, Name, Organization, Photo};

    use super::*;

    #[test]
    fn only_workspace_accounts_have_a_directory() {
        assert!(is_workspace("maya@brightmoor.co"));
        assert!(!is_workspace("ngozi.okafor@gmail.com"));
        assert!(!is_workspace("someone@GoogleMail.com"));
    }

    fn person(resource: &str, name: &str, emails: &[&str]) -> Person {
        Person {
            resource_name: Some(resource.into()),
            names: vec![Name {
                display_name: Some(name.into()),
                ..Name::default()
            }],
            email_addresses: emails
                .iter()
                .map(|email| EmailAddress {
                    value: Some((*email).into()),
                    ..EmailAddress::default()
                })
                .collect(),
            ..Person::default()
        }
    }

    #[test]
    fn a_listed_person_brings_their_primary_details_to_each_address() {
        let primary = || {
            Some(FieldMetadata {
                primary: Some(true),
                ..FieldMetadata::default()
            })
        };
        let person = Person {
            photos: vec![
                Photo {
                    url: Some("https://lh3.googleusercontent.com/old=s100".into()),
                    ..Photo::default()
                },
                Photo {
                    metadata: primary(),
                    url: Some("https://lh3.googleusercontent.com/tom=s100".into()),
                    default: None,
                },
            ],
            organizations: vec![Organization {
                name: Some("Brightmoor".into()),
                title: Some("Product Designer".into()),
                ..Organization::default()
            }],
            ..person(
                "people/c1",
                "Tom Becker",
                &["tom@brightmoor.co", "tom@becker.design"],
            )
        };
        let saved: Vec<GoogleContact> = entries(person.clone(), List::Saved).collect();
        assert_eq!(saved.len(), 2);
        assert!(saved.iter().all(|contact| {
            contact.person == "people/c1"
                && contact.photo.as_deref() == Some("https://lh3.googleusercontent.com/tom=s100")
                && contact.company.as_deref() == Some("Brightmoor")
                && contact.title.as_deref() == Some("Product Designer")
        }));
        let other: Vec<GoogleContact> = entries(person, List::Other).collect();
        assert!(
            other.iter().all(|contact| contact.photo.is_none()
                && contact.company.is_none()
                && contact.name.as_deref() == Some("Tom Becker")),
            "other contacts bring their names only"
        );
    }

    #[test]
    fn changes_replace_their_peoples_entries_and_deletions_remove_them() {
        let kept = GoogleList {
            sync_token: Some("^old".into()),
            contacts: [
                person("people/1", "Ana", &["ana@x.co"]),
                person("people/2", "Ben", &["ben@x.co", "ben@y.co"]),
                person("people/3", "Cy", &["cy@x.co"]),
            ]
            .into_iter()
            .flat_map(|person| entries(person, List::Directory))
            .collect(),
        };
        let mut gone = person("people/3", "Cy", &[]);
        gone.metadata = Some(gmail::people::PersonMetadata {
            deleted: Some(true),
            ..gmail::people::PersonMetadata::default()
        });
        let changed = vec![
            person("people/2", "Benjamin", &["ben@x.co"]),
            gone,
            person("people/4", "Dee", &["dee@x.co"]),
        ];
        let updated = apply(&kept, List::Directory, changed, Some("^new".into()));
        let entries: Vec<(&str, &str)> = updated
            .contacts
            .iter()
            .map(|contact| {
                (
                    contact.email.as_str(),
                    contact.name.as_deref().unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(
            entries,
            [
                ("ana@x.co", "Ana"),
                ("ben@x.co", "Benjamin"),
                ("dee@x.co", "Dee")
            ]
        );
        assert_eq!(updated.sync_token.as_deref(), Some("^new"));
    }
}
