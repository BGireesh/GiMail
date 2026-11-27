//! Mail settings: the master category list, mailbox settings (automatic replies), Inbox rules
//! and Focused Inbox's overrides.

use reqwest::Method;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_with::skip_serializing_none;

use crate::client::{Call, Client};
use crate::model::{
    DateTimeTimeZone, EmailAddress, Importance, InferenceClassificationType, Recipient,
};
use crate::odata::Page;

/// A category of the mailbox's master list (learn.microsoft.com/graph/api/resources/
/// outlookcategory). Its name can't change once made.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlookCategory {
    /// The category's ID.
    pub id: Option<String>,
    /// Its name, which messages' `categories` carry.
    pub display_name: Option<String>,
    /// Its color.
    pub color: Option<CategoryColor>,
}

/// One of Outlook's 25 category colors (`preset0` to `preset24`), or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CategoryColor {
    /// No color.
    None,
    /// `preset{n}`, 0 to 24: red, orange, brown, yellow, green, teal, olive, blue, purple,
    /// cranberry, steel, dark steel, gray, dark gray, black, then the dark shades of the first
    /// ten.
    Preset(u8),
}

impl Serialize for CategoryColor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::None => serializer.serialize_str("none"),
            Self::Preset(n) => serializer.serialize_str(&format!("preset{n}")),
        }
    }
}

/// An unknown color reads as none.
impl<'de> Deserialize<'de> for CategoryColor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(text
            .strip_prefix("preset")
            .and_then(|n| n.parse().ok())
            .filter(|n| *n <= 24)
            .map_or(Self::None, Self::Preset))
    }
}

/// The mailbox's settings (learn.microsoft.com/graph/api/resources/mailboxsettings). An update
/// sends only what's set.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxSettings {
    /// The Archive folder's ID.
    pub archive_folder: Option<String>,
    /// The mailbox's time zone, a Windows name (`Eastern Standard Time`) or IANA's.
    pub time_zone: Option<String>,
    /// Automatic replies.
    pub automatic_replies_setting: Option<AutomaticRepliesSetting>,
    /// The mailbox's language.
    pub language: Option<LocaleInfo>,
    /// The user's configured working hours.
    pub working_hours: Option<crate::calendar::WorkingHours>,
    /// The date format, e.g. `M/d/yyyy`.
    pub date_format: Option<String>,
    /// The time format, e.g. `h:mm tt`.
    pub time_format: Option<String>,
}

/// A language.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocaleInfo {
    /// Its tag, e.g. `en-US`.
    pub locale: Option<String>,
    /// Its name, e.g. `English (United States)`.
    pub display_name: Option<String>,
}

/// Automatic replies (learn.microsoft.com/graph/api/resources/automaticrepliessetting).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticRepliesSetting {
    /// Off, on, or on between the scheduled times.
    pub status: Option<AutomaticRepliesStatus>,
    /// Who outside the organization gets the external reply.
    pub external_audience: Option<ExternalAudienceScope>,
    /// The reply to people in the organization (HTML).
    pub internal_reply_message: Option<String>,
    /// The reply to people outside it (HTML).
    pub external_reply_message: Option<String>,
    /// When a scheduled reply starts.
    pub scheduled_start_date_time: Option<DateTimeTimeZone>,
    /// When it ends.
    pub scheduled_end_date_time: Option<DateTimeTimeZone>,
}

/// [`AutomaticRepliesSetting::status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AutomaticRepliesStatus {
    /// Off.
    Disabled,
    /// On until turned off.
    AlwaysEnabled,
    /// On between the scheduled times.
    Scheduled,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// [`AutomaticRepliesSetting::external_audience`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExternalAudienceScope {
    /// Nobody outside the organization.
    None,
    /// Only the mailbox's contacts.
    ContactsOnly,
    /// Everyone.
    All,
    /// A value this client doesn't know.
    #[serde(other)]
    UnknownFutureValue,
}

/// An Inbox rule (learn.microsoft.com/graph/api/resources/messagerule). Only the conditions and
/// actions Exo's filters use are modeled.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRule {
    /// The rule's ID.
    pub id: Option<String>,
    /// Its name.
    pub display_name: Option<String>,
    /// Its place in the order rules run in.
    pub sequence: Option<u32>,
    /// Whether it runs.
    pub is_enabled: Option<bool>,
    /// Whether Outlook found it broken.
    pub has_error: Option<bool>,
    /// Whether it can't be changed through Graph.
    pub is_read_only: Option<bool>,
    /// What a message must match.
    pub conditions: Option<MessageRulePredicates>,
    /// What happens to it.
    pub actions: Option<MessageRuleActions>,
    /// What spares a message that matched.
    pub exceptions: Option<MessageRulePredicates>,
}

/// [`MessageRule::conditions`] and [`MessageRule::exceptions`]. Every condition set must hold.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRulePredicates {
    /// From one of these addresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub from_addresses: Vec<Recipient>,
    /// The sender's name or address contains one of these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sender_contains: Vec<String>,
    /// Sent to one of these addresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sent_to_addresses: Vec<Recipient>,
    /// A recipient's name or address contains one of these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipient_contains: Vec<String>,
    /// The subject contains one of these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subject_contains: Vec<String>,
    /// The body contains one of these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body_contains: Vec<String>,
    /// The body or subject contains one of these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body_or_subject_contains: Vec<String>,
    /// Whether it has attachments.
    pub has_attachments: Option<bool>,
}

/// [`MessageRule::actions`].
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRuleActions {
    /// Categories to add, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assign_categories: Vec<String>,
    /// The folder to move it to.
    pub move_to_folder: Option<String>,
    /// The folder to copy it to.
    pub copy_to_folder: Option<String>,
    /// Move it to Deleted Items.
    pub delete: Option<bool>,
    /// Delete it for good.
    pub permanent_delete: Option<bool>,
    /// Mark it read.
    pub mark_as_read: Option<bool>,
    /// Set its importance.
    pub mark_importance: Option<Importance>,
    /// Forward it to these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forward_to: Vec<Recipient>,
    /// Run no later rule on it.
    pub stop_processing_rules: Option<bool>,
}

/// A Focused Inbox override: where a sender's future mail goes
/// (learn.microsoft.com/graph/api/resources/inferenceclassificationoverride).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InferenceClassificationOverride {
    /// The override's ID.
    pub id: Option<String>,
    /// Focused or Other.
    pub classify_as: Option<InferenceClassificationType>,
    /// The sender it's for.
    pub sender_email_address: Option<EmailAddress>,
}

/// Settings calls. Get one with [`Client::settings`].
#[derive(Debug, Clone, Copy)]
pub struct Settings<'a> {
    client: &'a Client,
}

impl Client {
    /// Mail settings calls for the signed-in user.
    #[must_use]
    pub fn settings(&self) -> Settings<'_> {
        Settings { client: self }
    }
}

impl<'a> Settings<'a> {
    fn call<T>(&self, method: Method, path: &[&str]) -> Call<'a, T> {
        self.client.me_call(method, path.iter().copied())
    }

    /// The master category list.
    pub fn categories(&self) -> Call<'a, Page<OutlookCategory>> {
        self.call(Method::GET, &["outlook", "masterCategories"])
    }

    /// Adds a category (its name and color).
    pub fn create_category(&self, category: &OutlookCategory) -> Call<'a, OutlookCategory> {
        self.call(Method::POST, &["outlook", "masterCategories"])
            .json(category)
    }

    /// Changes a category's color.
    pub fn update_category(
        &self,
        id: &str,
        category: &OutlookCategory,
    ) -> Call<'a, OutlookCategory> {
        self.call(Method::PATCH, &["outlook", "masterCategories", id])
            .json(category)
    }

    /// Takes a category off the list. Messages keep it.
    pub fn delete_category(&self, id: &str) -> Call<'a, ()> {
        self.call(Method::DELETE, &["outlook", "masterCategories", id])
    }

    /// The mailbox's settings.
    pub fn mailbox_settings(&self) -> Call<'a, MailboxSettings> {
        self.call(Method::GET, &["mailboxSettings"])
    }

    /// Changes the settings `settings` sets, e.g. automatic replies.
    pub fn update_mailbox_settings(&self, settings: &MailboxSettings) -> Call<'a, MailboxSettings> {
        self.call(Method::PATCH, &["mailboxSettings"])
            .json(settings)
    }

    /// The Inbox's rules.
    pub fn rules(&self) -> Call<'a, Page<MessageRule>> {
        self.call(Method::GET, &["mailFolders", "inbox", "messageRules"])
    }

    /// Adds an Inbox rule.
    pub fn create_rule(&self, rule: &MessageRule) -> Call<'a, MessageRule> {
        self.call(Method::POST, &["mailFolders", "inbox", "messageRules"])
            .json(rule)
    }

    /// Changes what `rule` sets of an Inbox rule.
    pub fn update_rule(&self, id: &str, rule: &MessageRule) -> Call<'a, MessageRule> {
        self.call(Method::PATCH, &["mailFolders", "inbox", "messageRules", id])
            .json(rule)
    }

    /// Deletes an Inbox rule.
    pub fn delete_rule(&self, id: &str) -> Call<'a, ()> {
        self.call(
            Method::DELETE,
            &["mailFolders", "inbox", "messageRules", id],
        )
    }

    /// Focused Inbox's overrides.
    pub fn overrides(&self) -> Call<'a, Page<InferenceClassificationOverride>> {
        self.call(Method::GET, &["inferenceClassification", "overrides"])
    }

    /// Sends a sender's future mail to Focused or Other. A second override for the same
    /// address replaces the first.
    pub fn create_override(
        &self,
        rule: &InferenceClassificationOverride,
    ) -> Call<'a, InferenceClassificationOverride> {
        self.call(Method::POST, &["inferenceClassification", "overrides"])
            .json(rule)
    }

    /// Changes where an override sends the sender's mail.
    pub fn update_override(
        &self,
        id: &str,
        classify_as: InferenceClassificationType,
    ) -> Call<'a, InferenceClassificationOverride> {
        self.call(Method::PATCH, &["inferenceClassification", "overrides", id])
            .json(&InferenceClassificationOverride {
                classify_as: Some(classify_as),
                ..InferenceClassificationOverride::default()
            })
    }

    /// Deletes an override.
    pub fn delete_override(&self, id: &str) -> Call<'a, ()> {
        self.call(
            Method::DELETE,
            &["inferenceClassification", "overrides", id],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_colors_are_presets() {
        let color = |text: &str| serde_json::from_value::<CategoryColor>(text.into()).unwrap();
        assert_eq!(color("preset7"), CategoryColor::Preset(7));
        assert_eq!(color("none"), CategoryColor::None);
        assert_eq!(color("preset25"), CategoryColor::None);
        assert_eq!(
            serde_json::to_value(CategoryColor::Preset(24)).unwrap(),
            "preset24"
        );
    }
}
