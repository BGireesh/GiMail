//! Gmail's sender filters (`users.settings.filters`): blocking senders/domains (Trash) and
//! routing senders to Important (`CATEGORY_PERSONAL`) or Other (`CATEGORY_SOCIAL`).

use gmail::{FilterAction, FilterCriteria};

use super::Gmail;
use crate::bulk::{ATTEMPTS, SenderRule, SenderRuleUndo};
use crate::error::Result;
use crate::ids::labels::{CATEGORY_PERSONAL, CATEGORY_SOCIAL, TRASH};
use crate::quota::Lane;

/// The categories a sender filter can send mail to. A new filter for one replaces a filter on the
/// same sender for the other.
const CATEGORIES: [&str; 2] = [CATEGORY_PERSONAL, CATEGORY_SOCIAL];

impl Gmail {
    /// Applies a provider-neutral [`SenderRule`] for `from` using Gmail's filter API.
    pub(super) async fn set_sender_rule(
        &self,
        from: &str,
        rule: SenderRule,
    ) -> Result<SenderRuleUndo> {
        let label = match rule {
            SenderRule::Block => TRASH,
            SenderRule::Important => CATEGORY_PERSONAL,
            SenderRule::Other => CATEGORY_SOCIAL,
        };
        let (created, replaced) = self.create_sender_filter(from, label).await?;
        if created.is_none() && replaced.is_none() {
            Ok(SenderRuleUndo::None)
        } else {
            Ok(SenderRuleUndo::Gmail {
                created,
                replaced: replaced.map(Box::new),
            })
        }
    }

    /// Reverses a Gmail [`SenderRuleUndo`].
    pub(super) async fn undo_sender_rule(&self, undo: &SenderRuleUndo) -> Result<()> {
        match undo {
            SenderRuleUndo::None => Ok(()),
            SenderRuleUndo::Gmail { created, replaced } => {
                self.delete_sender_filter(created.as_deref(), replaced.as_deref())
                    .await
            }
            SenderRuleUndo::OutlookRule { .. } | SenderRuleUndo::OutlookOverride { .. } => {
                Err(crate::Error::Unsupported("Outlook sender rules"))
            }
        }
    }

    /// Creates the filter that gives mail from `from` the label `label`, shaped as Exo's own
    /// filters are (`from` alone, one label added). `TRASH` blocks a sender or domain, and a
    /// category sends them to Important or Other. A category filter replaces the sender's filter
    /// for the other category. Returns the new filter's id, or none when Gmail already had the
    /// filter, and the replaced filter.
    pub(crate) async fn create_sender_filter(
        &self,
        from: &str,
        label: &str,
    ) -> Result<(Option<String>, Option<gmail::Filter>)> {
        let criteria = FilterCriteria {
            from: Some(from.to_owned()),
            ..FilterCriteria::default()
        };
        let action = FilterAction {
            add_label_ids: vec![label.to_owned()],
            ..FilterAction::default()
        };
        let same = |filter: &gmail::Filter| {
            filter.criteria.as_ref() == Some(&criteria) && filter.action.as_ref() == Some(&action)
        };
        let filters = self
            .api
            .call(Lane::Act, ATTEMPTS, |gmail| gmail.filters())
            .await?
            .filter;
        let replaced = filters
            .iter()
            .find(|filter| {
                let adds = filter
                    .action
                    .as_ref()
                    .map(|action| action.add_label_ids.clone())
                    .unwrap_or_default();
                CATEGORIES.contains(&label)
                    && filter.criteria.as_ref() == Some(&criteria)
                    && adds
                        .iter()
                        .any(|added| CATEGORIES.contains(&added.as_str()) && added != label)
            })
            .cloned();
        let filter = gmail::Filter {
            id: None,
            criteria: Some(criteria.clone()),
            action: Some(action.clone()),
        };
        // The new filter first: if Gmail won't make it, the old one stays. If the old one then
        // can't go, the new one goes again, since a failed job leaves undo nothing to undo.
        let id = if filters.iter().any(same) {
            None
        } else {
            Some(
                match self
                    .api
                    .call(Lane::Act, ATTEMPTS, |gmail| gmail.create_filter(&filter))
                    .await
                {
                    Ok(created) => created.id.unwrap_or_default(),
                    // An attempt whose answer was lost made it, so the retry was refused as a
                    // duplicate. The filter there now is this one, since it wasn't there before.
                    Err(failure) => {
                        let made = self
                            .api
                            .call(Lane::Act, ATTEMPTS, |gmail| gmail.filters())
                            .await
                            .ok()
                            .and_then(|list| {
                                list.filter
                                    .into_iter()
                                    .find(|filter| same(filter))
                                    .and_then(|filter| filter.id)
                            });
                        made.ok_or(failure)?
                    }
                },
            )
        };
        if let Some(old) = replaced.as_ref().and_then(|old| old.id.clone())
            && let Err(failure) = self
                .api
                .call(Lane::Act, ATTEMPTS, |gmail| gmail.delete_filter(&old))
                .await
        {
            if let Some(id) = &id
                && let Err(error) = self
                    .api
                    .call(Lane::Act, ATTEMPTS, |gmail| gmail.delete_filter(id))
                    .await
            {
                log::warn!("taking back filter {id}: {}", error.message());
            }
            return Err(failure.into());
        }
        Ok((id, replaced))
    }

    /// Deletes filter `id`, if it was made, and puts back `replaced`.
    pub(crate) async fn delete_sender_filter(
        &self,
        id: Option<&str>,
        replaced: Option<&gmail::Filter>,
    ) -> Result<()> {
        if let Some(id) = id {
            match self
                .api
                .call(Lane::Act, ATTEMPTS, |gmail| gmail.delete_filter(id))
                .await
            {
                Err(failure) if failure.is_not_found() => {}
                result => result?,
            }
        }
        if let Some(filter) = replaced {
            let filter = gmail::Filter {
                id: None,
                ..filter.clone()
            };
            self.api
                .call(Lane::Act, ATTEMPTS, |gmail| gmail.create_filter(&filter))
                .await?;
        }
        Ok(())
    }

    /// Deletes the Gmail filters that block `from` (`from` alone, Trash added and nothing else).
    pub(super) async fn delete_block_rules(&self, from: &str) -> Result<usize> {
        let block = (
            FilterCriteria {
                from: Some(from.to_owned()),
                ..FilterCriteria::default()
            },
            FilterAction {
                add_label_ids: vec![TRASH.to_owned()],
                ..FilterAction::default()
            },
        );
        let filters = self
            .api
            .call(Lane::Act, ATTEMPTS, |gmail| gmail.filters())
            .await?
            .filter;
        let mut deleted = 0;
        for filter in filters {
            let shape = (
                filter.criteria.clone().unwrap_or_default(),
                filter.action.clone().unwrap_or_default(),
            );
            let Some(id) = filter.id.filter(|_| shape == block) else {
                continue;
            };
            match self
                .api
                .call(Lane::Act, ATTEMPTS, |gmail| gmail.delete_filter(&id))
                .await
            {
                Err(failure) if failure.is_not_found() => {}
                result => result?,
            }
            deleted += 1;
        }
        Ok(deleted)
    }
}
