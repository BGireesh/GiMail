//! The account as Gmail has it: where its history is, its labels (Exo's hidden ones among them)
//! and its send-as aliases.

use gmail::{LabelListVisibility, MessageListVisibility};

use super::{Gmail, cursors_at};
use crate::api::Failure;
use crate::error::Result;
use crate::ids::labels::{EXO_MUTED, EXO_PARENT, EXO_REMINDER_UNREAD, EXO_SNOOZED};
use crate::model::{Alias, Label, LabelKind};
use crate::provider::{Account, Cursors, Labels, Markers};
use crate::quota::Lane;
use crate::sync::ATTEMPTS;

impl Gmail {
    /// The cursors at the mailbox's `historyId` now (`users.getProfile`).
    pub(super) async fn start(&self) -> Result<Cursors> {
        let profile = self
            .api
            .call(Lane::Show, ATTEMPTS, |gmail| gmail.profile())
            .await?;
        Ok(cursors_at(profile.history_id.unwrap_or_default()))
    }

    /// The account's labels, creating Exo's hidden ones first if they're missing.
    pub(super) async fn labels(&self) -> Result<Labels> {
        let named = |labels: &[gmail::Label], name: &str| {
            labels
                .iter()
                .any(|label| label.name.as_deref() == Some(name))
        };
        let mut labels = self
            .api
            .call(Lane::Show, ATTEMPTS, |gmail| gmail.labels())
            .await?
            .labels;
        let missing: Vec<&str> = [EXO_PARENT, EXO_MUTED, EXO_SNOOZED, EXO_REMINDER_UNREAD]
            .into_iter()
            .filter(|name| !named(&labels, name))
            .collect();
        for name in &missing {
            let hidden = gmail::Label {
                name: Some((*name).to_owned()),
                label_list_visibility: Some(LabelListVisibility::LabelHide),
                message_list_visibility: Some(MessageListVisibility::Hide),
                ..gmail::Label::default()
            };
            match self
                .api
                .call(Lane::Show, ATTEMPTS, |gmail| gmail.create_label(&hidden))
                .await
            {
                // Another client created it meanwhile.
                Err(Failure::Permanent(error)) if error.status() == Some(409) => {}
                result => {
                    result?;
                }
            }
        }
        if !missing.is_empty() {
            labels = self
                .api
                .call(Lane::Show, ATTEMPTS, |gmail| gmail.labels())
                .await?
                .labels;
        }
        let id = |name: &str| {
            labels
                .iter()
                .find(|label| label.name.as_deref() == Some(name))
                .and_then(|label| label.id.clone())
        };
        let markers = Markers {
            muted: id(EXO_MUTED),
            snoozed: id(EXO_SNOOZED),
            reminder_unread: id(EXO_REMINDER_UNREAD),
        };
        let labels = labels
            .into_iter()
            .filter_map(|label| {
                let id = label.id?;
                let color = label.color.unwrap_or_default();
                Some(Label {
                    name: label.name.unwrap_or_else(|| id.clone()),
                    id,
                    kind: if label.kind == Some(gmail::LabelType::System) {
                        LabelKind::System
                    } else {
                        LabelKind::User
                    },
                    text_color: color.text_color,
                    background_color: color.background_color,
                    hidden: label.label_list_visibility == Some(LabelListVisibility::LabelHide),
                })
            })
            .collect();
        Ok(Labels { labels, markers })
    }

    /// Which addresses are "me" (the primary and the aliases Gmail treats as the account), and
    /// the send-as aliases with their signatures for compose.
    pub(super) async fn account(&self) -> Result<Account> {
        let send_as = self
            .api
            .call(Lane::Show, ATTEMPTS, |gmail| gmail.send_as_list())
            .await?
            .send_as;
        let mut addresses = Vec::new();
        let mut name = None;
        for alias in &send_as {
            let primary = alias.is_primary == Some(true);
            if primary {
                name = alias.display_name.clone().filter(|name| !name.is_empty());
            }
            if primary || alias.treat_as_alias == Some(true) {
                addresses.extend(
                    alias
                        .send_as_email
                        .as_ref()
                        .map(|email| email.to_lowercase()),
                );
            }
        }
        let aliases = send_as
            .iter()
            .filter_map(|alias| {
                let email = alias.send_as_email.clone()?;
                let default = alias.is_default == Some(true);
                let primary = alias.is_primary == Some(true);
                Some(Alias {
                    name: alias.display_name.clone().filter(|name| !name.is_empty()),
                    signature: alias
                        .signature
                        .clone()
                        .filter(|signature| !signature.is_empty()),
                    active: default
                        || primary
                        || alias.verification_status == Some(gmail::VerificationStatus::Accepted),
                    email,
                    default,
                    primary,
                })
            })
            .collect();
        Ok(Account {
            addresses,
            name,
            aliases,
        })
    }
}
