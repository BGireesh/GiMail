//! Outlook sender rules: blocking senders or domains via Inbox `messageRules` (moving mail to
//! Deleted Items) and routing senders between Important (Focused) and Other via
//! `inferenceClassification/overrides`.

use std::collections::HashSet;

use outlook::settings::{
    InferenceClassificationOverride, MessageRule, MessageRuleActions, MessageRulePredicates,
};
use outlook::{EmailAddress, InferenceClassificationType, Recipient, well_known};

use super::Outlook;
use super::mailbox::Role;
use crate::api::Failure;
use crate::bulk::{ATTEMPTS, SenderRule, SenderRuleUndo};
use crate::error::{Error, Result};
use crate::quota::Lane;

impl Outlook {
    /// Applies a provider-neutral [`SenderRule`] for `from` on Outlook.
    pub(super) async fn set_sender_rule(
        &self,
        from: &str,
        rule: SenderRule,
    ) -> Result<SenderRuleUndo> {
        let from = from.trim();
        match rule {
            SenderRule::Block => self.create_block_rule(from).await,
            SenderRule::Important => {
                self.set_override(from, InferenceClassificationType::Focused)
                    .await
            }
            SenderRule::Other => {
                self.set_override(from, InferenceClassificationType::Other)
                    .await
            }
        }
    }

    /// Reverses an Outlook [`SenderRuleUndo`].
    pub(super) async fn undo_sender_rule(&self, undo: &SenderRuleUndo) -> Result<()> {
        match undo {
            SenderRuleUndo::None => Ok(()),
            SenderRuleUndo::OutlookRule { id } => {
                match self
                    .graph
                    .call(Lane::Act, ATTEMPTS, |graph| {
                        graph.settings().delete_rule(id)
                    })
                    .await
                {
                    Err(failure) if failure.is_not_found() => Ok(()),
                    result => Ok(result?),
                }
            }
            SenderRuleUndo::OutlookOverride { id, previous } => {
                let result = match previous {
                    Some(previous) => self
                        .graph
                        .call(Lane::Act, ATTEMPTS, |graph| {
                            graph.settings().update_override(id, *previous)
                        })
                        .await
                        .map(drop),
                    None => {
                        self.graph
                            .call(Lane::Act, ATTEMPTS, |graph| {
                                graph.settings().delete_override(id)
                            })
                            .await
                    }
                };
                match result {
                    Err(failure) if failure.is_not_found() => Ok(()),
                    result => Ok(result?),
                }
            }
            SenderRuleUndo::Gmail { .. } => Err(Error::Unsupported("Gmail sender filters")),
        }
    }

    /// Deletes every Inbox rule blocking `from` (an address or domain), leaving non-block rules
    /// untouched. Returns how many rules were deleted.
    pub(super) async fn delete_block_rules(&self, from: &str) -> Result<usize> {
        let from = from.trim();
        let Some(target) = BlockTarget::parse(from) else {
            return Ok(0);
        };
        let deleted_id = self
            .mailbox()
            .await?
            .folder(Role::Deleted)
            .map(str::to_owned);
        let rules = self
            .graph
            .call(Lane::Act, ATTEMPTS, |graph| graph.settings().rules().all())
            .await?
            .value;
        let mut deleted = 0;
        for rule in rules {
            if !is_block_rule(&rule, &target, deleted_id.as_deref()) {
                continue;
            }
            let Some(id) = rule.id else {
                continue;
            };
            match self
                .graph
                .call(Lane::Act, ATTEMPTS, |graph| {
                    graph.settings().delete_rule(&id)
                })
                .await
            {
                Err(failure) if failure.is_not_found() => {}
                result => result?,
            }
            deleted += 1;
        }
        Ok(deleted)
    }

    /// Creates an Inbox rule moving mail from `from` (an address or domain) to Deleted Items.
    /// Returns [`SenderRuleUndo::None`] when an enabled matching block rule already exists.
    async fn create_block_rule(&self, from: &str) -> Result<SenderRuleUndo> {
        let Some(target) = BlockTarget::parse(from) else {
            return Ok(SenderRuleUndo::None);
        };
        let mailbox = self.mailbox().await?;
        let deleted_id = mailbox.folder(Role::Deleted).map(str::to_owned);
        let destination = deleted_id
            .clone()
            .unwrap_or_else(|| well_known::DELETED_ITEMS.to_owned());
        let existing = self
            .graph
            .call(Lane::Act, ATTEMPTS, |graph| graph.settings().rules().all())
            .await?
            .value;
        if existing.iter().any(|rule| {
            rule.is_enabled != Some(false) && is_block_rule(rule, &target, deleted_id.as_deref())
        }) {
            return Ok(SenderRuleUndo::None);
        }
        let existing_ids: HashSet<String> =
            existing.into_iter().filter_map(|rule| rule.id).collect();
        let rule = MessageRule {
            display_name: Some(format!("Block {}", target.label())),
            sequence: Some(1),
            is_enabled: Some(true),
            conditions: Some(target.predicates()),
            actions: Some(MessageRuleActions {
                move_to_folder: Some(destination),
                stop_processing_rules: Some(true),
                ..MessageRuleActions::default()
            }),
            ..MessageRule::default()
        };
        // `POST messageRules` is not idempotent, so attempt once and inspect the list only if the
        // response may have been lost in transit (offline, transient, or throttled).
        let id = match self
            .graph
            .call(Lane::Act, 1, |graph| graph.settings().create_rule(&rule))
            .await
        {
            Ok(created) => created.id.filter(|id| !id.trim().is_empty()),
            Err(
                failure @ (Failure::Offline(_) | Failure::Transient(_) | Failure::Throttled(_)),
            ) => {
                let recovered = self
                    .graph
                    .call(Lane::Act, ATTEMPTS, |graph| graph.settings().rules().all())
                    .await
                    .ok()
                    .and_then(|page| {
                        page.value.into_iter().find_map(|candidate| {
                            let id = candidate.id.clone().filter(|id| !id.trim().is_empty())?;
                            (!existing_ids.contains(&id)
                                && is_block_rule(&candidate, &target, deleted_id.as_deref()))
                            .then_some(id)
                        })
                    });
                Some(recovered.ok_or(failure)?)
            }
            Err(failure) => return Err(failure.into()),
        };
        Ok(match id {
            Some(id) => SenderRuleUndo::OutlookRule { id },
            None => SenderRuleUndo::None,
        })
    }

    /// Creates or updates the Focused Inbox override for `from`. Graph only accepts email
    /// addresses (not bare domains) for overrides.
    async fn set_override(
        &self,
        from: &str,
        classify_as: InferenceClassificationType,
    ) -> Result<SenderRuleUndo> {
        if !is_email(from) {
            return Err(Error::Unsupported("Focused Inbox overrides for domains"));
        }
        let overrides = self
            .graph
            .call(Lane::Act, ATTEMPTS, |graph| {
                graph.settings().overrides().all()
            })
            .await?
            .value;
        if let Some(existing) = overrides.into_iter().find(|rule| {
            rule.sender_email_address
                .as_ref()
                .and_then(|sender| sender.address.as_deref())
                .is_some_and(|address| address.eq_ignore_ascii_case(from))
        }) {
            if existing.classify_as == Some(classify_as) {
                return Ok(SenderRuleUndo::None);
            }
            if let Some(id) = existing.id.filter(|id| !id.trim().is_empty()) {
                self.graph
                    .call(Lane::Act, ATTEMPTS, |graph| {
                        graph.settings().update_override(&id, classify_as)
                    })
                    .await?;
                return Ok(match existing.classify_as {
                    Some(
                        previous @ (InferenceClassificationType::Focused
                        | InferenceClassificationType::Other),
                    ) => SenderRuleUndo::OutlookOverride {
                        id,
                        previous: Some(previous),
                    },
                    _ => SenderRuleUndo::None,
                });
            }
        }
        let request = InferenceClassificationOverride {
            classify_as: Some(classify_as),
            sender_email_address: Some(EmailAddress {
                name: None,
                address: Some(from.to_owned()),
            }),
            ..InferenceClassificationOverride::default()
        };
        let created = self
            .graph
            .call(Lane::Act, ATTEMPTS, |graph| {
                graph.settings().create_override(&request)
            })
            .await?;
        let Some(id) = created.id.filter(|id| !id.trim().is_empty()) else {
            return Ok(SenderRuleUndo::None);
        };
        Ok(SenderRuleUndo::OutlookOverride { id, previous: None })
    }
}

/// What a block rule targets: one email address or a whole domain.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BlockTarget {
    Email(String),
    Domain(String),
}

impl BlockTarget {
    fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
            return None;
        }
        if is_email(trimmed) {
            Some(Self::Email(trimmed.to_ascii_lowercase()))
        } else {
            let domain = trimmed
                .strip_prefix('@')
                .unwrap_or(trimmed)
                .to_ascii_lowercase();
            (!domain.is_empty() && !domain.contains('@')).then_some(Self::Domain(domain))
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Email(email) | Self::Domain(email) => email,
        }
    }

    fn predicates(&self) -> MessageRulePredicates {
        match self {
            Self::Email(email) => MessageRulePredicates {
                from_addresses: vec![Recipient::new(email, None)],
                ..MessageRulePredicates::default()
            },
            Self::Domain(domain) => MessageRulePredicates {
                sender_contains: vec![format!("@{domain}")],
                ..MessageRulePredicates::default()
            },
        }
    }
}

/// Whether `from` is a single email address (`local@domain`) without whitespace.
fn is_email(from: &str) -> bool {
    !from.chars().any(char::is_whitespace)
        && matches!(
            from.split_once('@'),
            Some((local, domain)) if !local.is_empty() && !domain.is_empty() && !domain.contains('@')
        )
}

/// Whether `rule` is a pure block rule for `target`: matching only `target` as sender, with no
/// exceptions, and only deleting or moving to Deleted Items.
fn is_block_rule(rule: &MessageRule, target: &BlockTarget, deleted_id: Option<&str>) -> bool {
    if rule
        .exceptions
        .as_ref()
        .is_some_and(|exceptions| *exceptions != MessageRulePredicates::default())
    {
        return false;
    }
    let Some(conditions) = &rule.conditions else {
        return false;
    };
    if !conditions.sent_to_addresses.is_empty()
        || !conditions.recipient_contains.is_empty()
        || !conditions.subject_contains.is_empty()
        || !conditions.body_contains.is_empty()
        || !conditions.body_or_subject_contains.is_empty()
        || conditions.has_attachments.is_some()
    {
        return false;
    }
    let matches_sender = match target {
        BlockTarget::Email(email) => {
            let by_address = conditions.sender_contains.is_empty()
                && conditions.from_addresses.len() == 1
                && conditions.from_addresses[0]
                    .email_address
                    .as_ref()
                    .and_then(|address| address.address.as_deref())
                    .is_some_and(|address| address.eq_ignore_ascii_case(email));
            let by_contains = conditions.from_addresses.is_empty()
                && conditions.sender_contains.len() == 1
                && conditions.sender_contains[0].eq_ignore_ascii_case(email);
            by_address || by_contains
        }
        BlockTarget::Domain(domain) => {
            conditions.from_addresses.is_empty() && conditions.sender_contains.len() == 1 && {
                let needle = conditions.sender_contains[0]
                    .trim_start_matches('@')
                    .to_ascii_lowercase();
                needle == *domain
            }
        }
    };
    if !matches_sender {
        return false;
    }
    let Some(actions) = &rule.actions else {
        return false;
    };
    if !actions.assign_categories.is_empty()
        || actions.copy_to_folder.is_some()
        || actions.permanent_delete == Some(true)
        || actions.mark_as_read == Some(true)
        || actions.mark_importance.is_some()
        || !actions.forward_to.is_empty()
    {
        return false;
    }
    let moves_to_deleted = actions.move_to_folder.as_deref().is_some_and(|folder| {
        folder.eq_ignore_ascii_case(well_known::DELETED_ITEMS) || deleted_id == Some(folder)
    });
    let deletes = actions.delete == Some(true);
    (moves_to_deleted && !deletes) || (deletes && actions.move_to_folder.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_rule_matching_distinguishes_pure_blocks_from_other_rules() {
        let email = BlockTarget::parse("spam@example.com").unwrap();
        let domain = BlockTarget::parse("example.com").unwrap();
        let deleted = Some("folder-del");

        let move_email = MessageRule {
            conditions: Some(email.predicates()),
            actions: Some(MessageRuleActions {
                move_to_folder: Some("folder-del".into()),
                stop_processing_rules: Some(true),
                ..MessageRuleActions::default()
            }),
            ..MessageRule::default()
        };
        assert!(is_block_rule(&move_email, &email, deleted));
        assert!(!is_block_rule(&move_email, &domain, deleted));

        // Matches rules created with `delete: true` or bare domain without `@`.
        let delete_domain = MessageRule {
            conditions: Some(MessageRulePredicates {
                sender_contains: vec!["example.com".into()],
                ..MessageRulePredicates::default()
            }),
            actions: Some(MessageRuleActions {
                delete: Some(true),
                ..MessageRuleActions::default()
            }),
            ..MessageRule::default()
        };
        assert!(is_block_rule(&delete_domain, &domain, deleted));

        // Extra condition or non-block action disqualifies the rule.
        let with_subject = MessageRule {
            conditions: Some(MessageRulePredicates {
                subject_contains: vec!["promo".into()],
                ..email.predicates()
            }),
            ..move_email.clone()
        };
        assert!(!is_block_rule(&with_subject, &email, deleted));

        let to_archive = MessageRule {
            actions: Some(MessageRuleActions {
                move_to_folder: Some("folder-arch".into()),
                ..MessageRuleActions::default()
            }),
            ..move_email
        };
        assert!(!is_block_rule(&to_archive, &email, deleted));
    }

    #[test]
    fn block_target_and_email_validation_reject_whitespace_and_multiple_at_signs() {
        assert_eq!(
            BlockTarget::parse("  User@Example.COM "),
            Some(BlockTarget::Email("user@example.com".into()))
        );
        assert_eq!(
            BlockTarget::parse("example.com"),
            Some(BlockTarget::Domain("example.com".into()))
        );
        assert_eq!(
            BlockTarget::parse("@Example.COM"),
            Some(BlockTarget::Domain("example.com".into()))
        );
        for invalid in [
            "",
            "   ",
            "@",
            "a@b c",
            "a b@example.com",
            "a@b@c",
            "@@example.com",
            "bad domain.com",
        ] {
            assert_eq!(BlockTarget::parse(invalid), None, "input={invalid:?}");
            assert!(!is_email(invalid), "input={invalid:?}");
        }
    }
}
