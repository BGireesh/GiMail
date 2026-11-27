//! The account as Graph has it: who it is, its folder tree and categories, and so its labels.

use std::collections::HashMap;
use std::sync::Arc;

use outlook::MailFolder;
use url::Url;

use super::Outlook;
use super::mailbox::{Mailbox, Role, WELL_KNOWN};
use crate::api::Failure;
use crate::error::Result;
use crate::model::Alias;
use crate::provider::{Account, Labels};
use crate::quota::Lane;
use crate::sync::ATTEMPTS;

/// Folders a page of the tree's delta carries.
const PAGE_SIZE: u32 = 100;

impl Outlook {
    /// Asks Graph who the account is, once.
    pub(super) async fn check(&self) -> Result<(), Failure> {
        self.graph
            .call(Lane::Act, 1, |graph| graph.people().me().select("id"))
            .await
            .map(drop)
    }

    /// Reads the mailbox again: the well-known folders' ids, the folder tree (with where its
    /// delta's next round starts) and the master category list.
    pub(super) async fn load(&self) -> Result<(Arc<Mailbox>, Option<Url>), Failure> {
        let known = self
            .graph
            .each(Lane::Show, ATTEMPTS, WELL_KNOWN.len(), |graph, at| {
                graph.mail().folder(WELL_KNOWN[at].0).select("id")
            })
            .await?;
        let mut roles = HashMap::new();
        for (found, (_, role)) in known.into_iter().zip(WELL_KNOWN) {
            match found {
                Ok(folder) => roles.extend(folder.id.map(|id| (id, role))),
                // Not every mailbox has every optional well-known folder (Archive, or desktop sync
                // folders like `syncissues`, which Graph answers 404 or `ErrorInvalidIdMalformed`
                // for when the well-known name isn't recognized by that mailbox). Auth, throttle,
                // permission and server errors must propagate.
                Err(error)
                    if role != Role::Inbox
                        && (error.is_not_found()
                            || error.code() == Some("ErrorInvalidIdMalformed")) => {}
                Err(error) => return Err(super::graph::failed(error)),
            }
        }
        let tree = self
            .graph
            .call(Lane::Show, ATTEMPTS, |graph| {
                graph.mail().folders_delta().max_page_size(PAGE_SIZE).all()
            })
            .await?;
        let folders: Vec<MailFolder> = tree
            .value
            .into_iter()
            .filter(|folder| folder.removed.is_none())
            .collect();
        let categories = self
            .graph
            .call(Lane::Show, ATTEMPTS, |graph| {
                graph.settings().categories().all()
            })
            .await?
            .value;
        let mailbox = Mailbox::new(roles, &folders, &categories);
        Ok((self.keep(mailbox), tree.delta_link))
    }

    /// The account's labels: its folders, categories and Exo's markers, read again.
    pub(super) async fn labels(&self) -> Result<Labels> {
        let (mailbox, _) = self.load().await?;
        let (labels, markers) = mailbox.label_list();
        Ok(Labels { labels, markers })
    }

    /// Who the account is: its address (and the name it signs in with, when that's another),
    /// and its name. It sends from its own address alone.
    pub(super) async fn account(&self) -> Result<Account> {
        let me = self
            .graph
            .call(Lane::Show, ATTEMPTS, |graph| {
                graph
                    .people()
                    .me()
                    .select("displayName,mail,userPrincipalName")
            })
            .await?;
        let mut addresses: Vec<String> = Vec::new();
        for address in [me.mail.as_deref(), me.user_principal_name.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|address| !address.is_empty() && address.contains('@'))
            .map(str::to_lowercase)
        {
            if !addresses.contains(&address) {
                addresses.push(address);
            }
        }
        let name = me
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        let primary = [me.mail.as_deref(), me.user_principal_name.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|address| !address.is_empty());
        let aliases = primary
            .map(|email| Alias {
                email: email.to_owned(),
                name: name.clone(),
                signature: None,
                default: true,
                primary: true,
                active: true,
            })
            .into_iter()
            .collect();
        Ok(Account {
            addresses,
            name,
            aliases,
        })
    }
}
