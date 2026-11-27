//! People: the signed-in user and the organization's directory, the people the user writes with
//! most, contacts and their delta, and photos. Only the properties Exo reads are modeled.

use jiff::Timestamp;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::client::{Call, Client, Media};
use crate::model::EmailAddress;
use crate::odata::{Page, Removed};

/// A user: the signed-in one (`/me`) or one of the directory's (work and school accounts).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    /// The user's object ID (an ID token's `oid` for work accounts).
    pub id: Option<String>,
    /// Their full name.
    pub display_name: Option<String>,
    /// Their first name.
    pub given_name: Option<String>,
    /// Their last name.
    pub surname: Option<String>,
    /// Their primary SMTP address. Can be empty, as for some personal accounts.
    pub mail: Option<String>,
    /// The name they sign in with, usually their address.
    pub user_principal_name: Option<String>,
    /// Their job title.
    pub job_title: Option<String>,
    /// Their mobile number.
    pub mobile_phone: Option<String>,
    /// Their work numbers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub business_phones: Vec<String>,
    /// In a delta: the user left the directory.
    #[serde(rename = "@removed")]
    pub removed: Option<Removed>,
}

impl User {
    /// The mailbox's address: `mail`, else the sign-in name (trimmed, omitting empty values).
    #[must_use]
    pub fn address(&self) -> Option<&str> {
        [self.mail.as_deref(), self.user_principal_name.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|text| !text.is_empty())
    }
}

/// Someone the user communicates with, ranked by how much (`/me/people`,
/// learn.microsoft.com/graph/people-insights-overview).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    /// The person's ID.
    pub id: Option<String>,
    /// Their full name.
    pub display_name: Option<String>,
    /// Their first name.
    pub given_name: Option<String>,
    /// Their last name.
    pub surname: Option<String>,
    /// Their sign-in name, for people in the organization.
    pub user_principal_name: Option<String>,
    /// Their addresses, most relevant first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scored_email_addresses: Vec<ScoredEmailAddress>,
    /// Their job title.
    pub job_title: Option<String>,
    /// Their company.
    pub company_name: Option<String>,
}

/// An address and how relevant it is.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoredEmailAddress {
    /// The address.
    pub address: Option<String>,
    /// How relevant, higher first.
    pub relevance_score: Option<f64>,
}

/// A contact in the default Contacts folder.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    /// The contact's ID.
    pub id: Option<String>,
    /// When it last changed.
    pub last_modified_date_time: Option<Timestamp>,
    /// The folder it's in.
    pub parent_folder_id: Option<String>,
    /// Its full name.
    pub display_name: Option<String>,
    /// Its first name.
    pub given_name: Option<String>,
    /// Its last name.
    pub surname: Option<String>,
    /// Its company.
    pub company_name: Option<String>,
    /// Its job title.
    pub job_title: Option<String>,
    /// Its addresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub email_addresses: Vec<EmailAddress>,
    /// Its mobile number.
    pub mobile_phone: Option<String>,
    /// Its work numbers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub business_phones: Vec<String>,
    /// Its home numbers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub home_phones: Vec<String>,
    /// In a delta: the contact was deleted.
    #[serde(rename = "@removed")]
    pub removed: Option<Removed>,
}

/// A photo's metadata.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilePhoto {
    /// The size it's stored in, e.g. `240X240`.
    pub id: Option<String>,
    /// Its width in pixels.
    pub width: Option<u32>,
    /// Its height in pixels.
    pub height: Option<u32>,
}

/// People calls. Get one with [`Client::people`].
#[derive(Debug, Clone, Copy)]
pub struct People<'a> {
    client: &'a Client,
}

impl Client {
    /// People calls.
    #[must_use]
    pub fn people(&self) -> People<'_> {
        People { client: self }
    }
}

impl<'a> People<'a> {
    /// The signed-in user (`GET /me`).
    pub fn me(&self) -> Call<'a, User> {
        self.client.call(Method::GET, ["me"])
    }

    /// The organization's directory. Personal accounts have none (403).
    pub fn users(&self) -> Call<'a, Page<User>> {
        self.client.call(Method::GET, ["users"])
    }

    /// A user in the directory, by ID or sign-in name.
    pub fn user(&self, id: &str) -> Call<'a, User> {
        self.client.call(Method::GET, ["users", id])
    }

    /// The directory's delta: everyone in the first round, then who joined, changed or left
    /// (learn.microsoft.com/graph/api/user-delta).
    pub fn users_delta(&self) -> Call<'a, Page<User>> {
        self.client.call(Method::GET, ["users", "delta"])
    }

    /// The people the user writes with, most first.
    pub fn relevant(&self) -> Call<'a, Page<Person>> {
        self.client.me_call(Method::GET, ["people"])
    }

    /// The default Contacts folder's contacts.
    pub fn contacts(&self) -> Call<'a, Page<Contact>> {
        self.client.me_call(Method::GET, ["contacts"])
    }

    /// A contact.
    pub fn contact(&self, id: &str) -> Call<'a, Contact> {
        self.client.me_call(Method::GET, ["contacts", id])
    }

    /// The default Contacts folder's delta (learn.microsoft.com/graph/api/contact-delta).
    pub fn contacts_delta(&self) -> Call<'a, Page<Contact>> {
        self.client.me_call(Method::GET, ["contacts", "delta"])
    }

    /// The signed-in user's photo's metadata. No photo is 404 `ImageNotFound`.
    pub fn photo(&self) -> Call<'a, ProfilePhoto> {
        self.client.me_call(Method::GET, ["photo"])
    }

    /// The signed-in user's photo (learn.microsoft.com/graph/api/profilephoto-get).
    pub fn photo_content(&self) -> Call<'a, Media> {
        self.client.me_call(Method::GET, ["photo", "$value"])
    }

    /// A directory user's photo.
    pub fn user_photo_content(&self, id: &str) -> Call<'a, Media> {
        self.client
            .call(Method::GET, ["users", id, "photo", "$value"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_falls_back_to_the_sign_in_name() {
        let user = |mail: Option<&str>, upn: Option<&str>| User {
            mail: mail.map(str::to_owned),
            user_principal_name: upn.map(str::to_owned),
            ..User::default()
        };
        assert_eq!(
            user(Some("ada@example.com"), Some("ada@outlook.com")).address(),
            Some("ada@example.com")
        );
        assert_eq!(
            user(None, Some("ada@outlook.com")).address(),
            Some("ada@outlook.com")
        );
        assert_eq!(
            user(Some("  "), Some(" ada@outlook.com ")).address(),
            Some("ada@outlook.com")
        );
        assert_eq!(user(None, Some("")).address(), None);
        assert_eq!(user(Some(" "), Some("\t")).address(), None);
    }
}
