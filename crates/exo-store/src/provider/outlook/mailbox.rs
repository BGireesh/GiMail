//! The mailbox's folders and categories, and the label keys a message carries for them.
//!
//! A message sits in one folder. The Inbox, Junk Email, Deleted Items, Drafts and Sent Items are
//! Exo's `INBOX`, `SPAM`, `TRASH`, `DRAFT` and `SENT`, and Archive is none of them, which is how
//! a thread is done. Every other folder is a label keyed by its id, as is each category of the
//! master list (never by name, so a category called `INBOX` isn't the role). A folder under
//! Deleted Items or Junk Email also carries `TRASH` or `SPAM`. Folders that hold no mail Exo
//! shows (the Outbox, Conversation History, sync issues) aren't read at all.

use std::collections::{BTreeSet, HashMap};

use outlook::settings::OutlookCategory;
use outlook::{MailFolder, Message, OpenTypeExtension};

use crate::ids::labels::{
    CATEGORY_PERSONAL, DRAFT, EXO_REMINDER_UNREAD, EXO_SNOOZED, INBOX, SENT, SPAM, STARRED, TRASH,
    UNREAD,
};
use crate::model::{Label, LabelKind};
use crate::provider::Markers;

/// The open extension that marks a message whose thread has a reminder set. Each extension name
/// takes one of the mailbox's named properties for good, so the names never change.
pub(super) const SNOOZED: &str = "build.miguel.exo.snoozed";
/// The open extension that marks a message a reminder brought back, until it's read.
pub(super) const REMINDER_UNREAD: &str = "build.miguel.exo.reminderUnread";

/// Exo's markers.
pub(super) const MARKERS: [&str; 2] = [SNOOZED, REMINDER_UNREAD];

/// The `$expand` that reads Exo's markers with each message.
pub(super) fn markers_expand() -> String {
    format!(
        "extensions($filter=id eq {} or id eq {})",
        outlook::odata::quote(SNOOZED),
        outlook::odata::quote(REMINDER_UNREAD)
    )
}

/// What a well-known folder is to Exo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Role {
    Inbox,
    Drafts,
    Sent,
    Deleted,
    Junk,
    Archive,
    /// A folder whose items aren't mail Exo shows.
    Skipped,
}

/// The well-known folders, by the names Graph takes in place of their ids
/// (learn.microsoft.com/graph/api/resources/mailfolder).
pub(super) const WELL_KNOWN: [(&str, Role); 11] = [
    (outlook::well_known::INBOX, Role::Inbox),
    (outlook::well_known::DRAFTS, Role::Drafts),
    (outlook::well_known::SENT_ITEMS, Role::Sent),
    (outlook::well_known::DELETED_ITEMS, Role::Deleted),
    (outlook::well_known::JUNK_EMAIL, Role::Junk),
    (outlook::well_known::ARCHIVE, Role::Archive),
    (outlook::well_known::OUTBOX, Role::Skipped),
    ("conversationhistory", Role::Skipped),
    ("syncissues", Role::Skipped),
    ("conflicts", Role::Skipped),
    ("localfailures", Role::Skipped),
];

/// One folder of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Folder {
    name: String,
    parent: Option<String>,
    hidden: bool,
}

/// The mailbox's folders, their roles and its categories, as last read.
#[derive(Debug, Clone, Default)]
pub(crate) struct Mailbox {
    folders: HashMap<String, Folder>,
    /// The well-known folders' ids, by role.
    roles: HashMap<String, Role>,
    /// The master list's categories: their ids by name, and their names by id.
    categories: HashMap<String, String>,
    category_names: HashMap<String, String>,
    /// Categories messages carry that the list didn't have when it was read again for them.
    off_list: BTreeSet<String>,
}

impl Mailbox {
    /// A mailbox with these well-known folders (by id), folders and categories.
    pub fn new(
        roles: HashMap<String, Role>,
        folders: &[MailFolder],
        categories: &[OutlookCategory],
    ) -> Self {
        let mut mailbox = Self {
            roles,
            ..Self::default()
        };
        for folder in folders {
            mailbox.put(folder);
        }
        mailbox.set_categories(categories);
        mailbox
    }

    /// Adds or changes `folder`, as the tree's delta reports it. Returns whether its name or
    /// place changed (it's new, renamed, moved, hidden or shown), which changes the labels.
    pub fn put(&mut self, folder: &MailFolder) -> bool {
        let Some(id) = folder.id.clone() else {
            return false;
        };
        let folder = Folder {
            name: folder.display_name.clone().unwrap_or_default(),
            parent: folder.parent_folder_id.clone(),
            hidden: folder.is_hidden == Some(true),
        };
        self.folders.insert(id, folder.clone()) != Some(folder)
    }

    /// Takes away folder `id`, deleted. Returns whether the mailbox had it.
    pub fn remove(&mut self, id: &str) -> bool {
        self.folders.remove(id).is_some()
    }

    /// The master list's categories, as listed now.
    pub fn set_categories(&mut self, categories: &[OutlookCategory]) {
        self.categories = categories
            .iter()
            .filter_map(|category| Some((category.display_name.clone()?, category.id.clone()?)))
            .collect();
        self.category_names = self
            .categories
            .iter()
            .map(|(name, id)| (id.clone(), name.clone()))
            .collect();
    }

    /// The well-known folder with `role`.
    pub fn folder(&self, role: Role) -> Option<&str> {
        self.roles
            .iter()
            .find(|(_, known)| **known == role)
            .map(|(id, _)| id.as_str())
    }

    /// The folders whose mail Exo reads: every folder of the tree but those that hold no mail
    /// Exo shows, and the folders under them.
    pub fn read(&self) -> Vec<&str> {
        self.folders
            .keys()
            .filter(|id| self.is_read(id))
            .map(String::as_str)
            .collect()
    }

    /// Whether Exo reads folder `id`'s mail. A folder it doesn't know yet (new since the tree
    /// was read) is read.
    pub fn is_read(&self, id: &str) -> bool {
        !self
            .ancestry(id)
            .any(|folder| self.roles.get(folder) == Some(&Role::Skipped))
    }

    /// Whether a message of `messages` carries a category the list may have gained since it
    /// was read.
    pub fn misses_categories<'a>(&self, mut messages: impl Iterator<Item = &'a Message>) -> bool {
        messages.any(|message| {
            message
                .categories
                .iter()
                .any(|name| !self.categories.contains_key(name) && !self.off_list.contains(name))
        })
    }

    /// The master list read again for `messages`: what it still lacks of their categories is
    /// off the list, and isn't read again for.
    pub fn with_categories<'a>(
        &self,
        categories: &[OutlookCategory],
        messages: impl Iterator<Item = &'a Message>,
    ) -> Self {
        let mut mailbox = self.clone();
        mailbox.set_categories(categories);
        for message in messages {
            for name in &message.categories {
                if !mailbox.categories.contains_key(name) {
                    mailbox.off_list.insert(name.clone());
                }
            }
        }
        mailbox
    }

    /// Category `id`'s name, for one of the master list's.
    pub fn category_name(&self, id: &str) -> Option<&str> {
        self.category_names.get(id).map(String::as_str)
    }

    /// The category key named `name`, for one of the master list's.
    pub fn category(&self, name: &str) -> Option<&str> {
        self.categories.get(name).map(String::as_str)
    }

    /// Whether `key` is a folder's: a role's (`INBOX`, …) or a user folder's id.
    pub fn is_folder_key(&self, key: &str) -> bool {
        [INBOX, SPAM, TRASH, DRAFT, SENT].contains(&key) || self.is_user_folder(key)
    }

    /// Whether `key` is a user folder's id.
    pub fn is_user_folder(&self, key: &str) -> bool {
        self.folders.contains_key(key) && !self.roles.contains_key(key)
    }

    /// The folder a message moves to for folder key `key`: a role's well-known folder, or a user
    /// folder by id.
    pub fn folder_for(&self, key: &str) -> Option<&str> {
        let role = match key {
            INBOX => Role::Inbox,
            SPAM => Role::Junk,
            TRASH => Role::Deleted,
            DRAFT => Role::Drafts,
            SENT => Role::Sent,
            folder => {
                return self
                    .folders
                    .get_key_value(folder)
                    .map(|(id, _)| id.as_str());
            }
        };
        self.folder(role)
    }

    /// The label keys of a message in folder `id`.
    pub fn folder_labels<'a>(&'a self, id: &'a str) -> Vec<&'a str> {
        match self.roles.get(id) {
            Some(Role::Inbox) => vec![INBOX],
            Some(Role::Drafts) => vec![DRAFT],
            Some(Role::Sent) => vec![SENT],
            Some(Role::Deleted) => vec![TRASH],
            Some(Role::Junk) => vec![SPAM],
            Some(Role::Archive | Role::Skipped) => Vec::new(),
            None => {
                let mut keys = vec![id];
                for ancestor in self.ancestry(id).skip(1) {
                    match self.roles.get(ancestor) {
                        Some(Role::Deleted) => keys.push(TRASH),
                        Some(Role::Junk) => keys.push(SPAM),
                        _ => {}
                    }
                }
                keys
            }
        }
    }

    /// `message`'s label keys: its folder's (`folder` when it doesn't say), unread, flagged,
    /// its categories, Focused, and Exo's markers.
    pub fn labels(&self, message: &Message, folder: &str) -> Vec<String> {
        let folder = message.parent_folder_id.as_deref().unwrap_or(folder);
        let mut labels: BTreeSet<&str> = self.folder_labels(folder).into_iter().collect();
        if message.is_read == Some(false) {
            labels.insert(UNREAD);
        }
        if message
            .flag
            .as_ref()
            .and_then(|flag| flag.flag_status)
            .is_some_and(|status| status == outlook::FollowupFlagStatus::Flagged)
        {
            labels.insert(STARRED);
        }
        if message.inference_classification == Some(outlook::InferenceClassificationType::Focused) {
            labels.insert(CATEGORY_PERSONAL);
        }
        labels.extend(
            message
                .categories
                .iter()
                .filter_map(|name| self.category(name)),
        );
        for extension in &message.extensions {
            match marker(extension) {
                Some(SNOOZED) => labels.insert(EXO_SNOOZED),
                Some(REMINDER_UNREAD) => labels.insert(EXO_REMINDER_UNREAD),
                _ => false,
            };
        }
        labels.into_iter().map(str::to_owned).collect()
    }

    /// The account's labels: the roles, the folders and categories, and Exo's markers.
    pub fn label_list(&self) -> (Vec<Label>, Markers) {
        let label = |id: &str, name: String, kind: LabelKind, hidden: bool| Label {
            id: id.to_owned(),
            name,
            kind,
            text_color: None,
            background_color: None,
            hidden,
        };
        let mut labels: Vec<Label> = [
            INBOX,
            SENT,
            DRAFT,
            SPAM,
            TRASH,
            UNREAD,
            STARRED,
            CATEGORY_PERSONAL,
        ]
        .into_iter()
        .map(|id| label(id, id.to_owned(), LabelKind::System, false))
        .collect();
        let mut folders: Vec<(&String, &Folder)> = self
            .folders
            .iter()
            .filter(|(id, _)| !self.roles.contains_key(*id) && self.is_read(id))
            .collect();
        folders.sort_by_key(|(id, _)| *id);
        labels.extend(
            folders
                .into_iter()
                .map(|(id, folder)| label(id, self.path(id), LabelKind::User, folder.hidden)),
        );
        let mut categories: Vec<(&String, &String)> = self.categories.iter().collect();
        categories.sort();
        labels.extend(
            categories
                .into_iter()
                .map(|(name, id)| label(id, name.clone(), LabelKind::User, false)),
        );
        labels.extend(
            [EXO_SNOOZED, EXO_REMINDER_UNREAD]
                .map(|id| label(id, id.to_owned(), LabelKind::User, true)),
        );
        let markers = Markers {
            muted: None,
            snoozed: Some(EXO_SNOOZED.to_owned()),
            reminder_unread: Some(EXO_REMINDER_UNREAD.to_owned()),
        };
        (labels, markers)
    }

    /// Folder `id`'s name with its parents', `/` between them, from the top of the tree.
    fn path(&self, id: &str) -> String {
        let mut names: Vec<&str> = self
            .ancestry(id)
            .filter_map(|folder| self.folders.get(folder))
            .map(|folder| folder.name.as_str())
            .collect();
        names.reverse();
        names.join("/")
    }

    /// Folder `id`, then its parent, and so on up the folders the mailbox knows.
    fn ancestry<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        let mut next = Some(id);
        let mut steps = 0;
        std::iter::from_fn(move || {
            let current = next?;
            // A tree has no cycles, but a mailbox read halfway through a move might.
            steps += 1;
            next = (steps < 64)
                .then(|| self.folders.get(current)?.parent.as_deref())
                .flatten();
            Some(current)
        })
    }
}

/// Which of Exo's markers `extension` is.
pub(super) fn marker(extension: &OpenTypeExtension) -> Option<&'static str> {
    let named = |name: &str| {
        let short = |text: &str| {
            text.rsplit_once(".OpenTypeExtension.")
                .or_else(|| text.rsplit_once(".openTypeExtension."))
                .map_or(text, |(_, name)| name)
                .eq_ignore_ascii_case(name)
        };
        extension.extension_name.as_deref().is_some_and(short)
            || extension.id.as_deref().is_some_and(short)
    };
    [SNOOZED, REMINDER_UNREAD]
        .into_iter()
        .find(|name| named(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, name: &str, parent: &str) -> MailFolder {
        MailFolder {
            id: Some(id.into()),
            display_name: Some(name.into()),
            parent_folder_id: Some(parent.into()),
            ..MailFolder::default()
        }
    }

    fn mailbox() -> Mailbox {
        let roles = HashMap::from([
            ("in".to_owned(), Role::Inbox),
            ("del".to_owned(), Role::Deleted),
            ("arch".to_owned(), Role::Archive),
            ("out".to_owned(), Role::Skipped),
        ]);
        let folders = [
            folder("in", "Inbox", "root"),
            folder("del", "Deleted Items", "root"),
            folder("arch", "Archive", "root"),
            folder("out", "Outbox", "root"),
            folder("rec", "Receipts", "in"),
            folder("old", "Old", "del"),
            folder("stuck", "Stuck", "out"),
        ];
        let categories = [OutlookCategory {
            id: Some("cat-1".into()),
            display_name: Some("INBOX".into()),
            color: None,
        }];
        Mailbox::new(roles, &folders, &categories)
    }

    #[test]
    fn folders_and_categories_are_label_keys_by_id() {
        let mailbox = mailbox();
        assert_eq!(mailbox.folder_labels("in"), [INBOX]);
        assert!(mailbox.folder_labels("arch").is_empty(), "Archive is done");
        assert_eq!(mailbox.folder_labels("rec"), ["rec"], "not the Inbox");
        assert_eq!(mailbox.folder_labels("old"), ["old", TRASH]);
        let message = Message {
            is_read: Some(false),
            flag: Some(outlook::FollowupFlag::new(
                outlook::FollowupFlagStatus::Complete,
            )),
            categories: vec!["INBOX".into(), "Unlisted".into()],
            inference_classification: Some(outlook::InferenceClassificationType::Focused),
            extensions: vec![OpenTypeExtension {
                extension_name: Some(SNOOZED.into()),
                id: Some(format!(
                    "Microsoft.OutlookServices.OpenTypeExtension.{SNOOZED}"
                )),
                data: serde_json::Map::new(),
            }],
            ..Message::default()
        };
        assert_eq!(
            mailbox.labels(&message, "arch"),
            [CATEGORY_PERSONAL, UNREAD, EXO_SNOOZED, "cat-1"],
            "a category named INBOX is its id, a completed flag isn't a star, and a category off \
             the list is left out"
        );
        assert!(
            mailbox
                .read()
                .iter()
                .all(|id| !["out", "stuck"].contains(id))
        );
        let (labels, markers) = mailbox.label_list();
        let names: Vec<(&str, &str)> = labels
            .iter()
            .filter(|label| label.kind == LabelKind::User)
            .map(|label| (label.id.as_str(), label.name.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("old", "Deleted Items/Old"),
                ("rec", "Inbox/Receipts"),
                ("cat-1", "INBOX"),
                (EXO_SNOOZED, EXO_SNOOZED),
                (EXO_REMINDER_UNREAD, EXO_REMINDER_UNREAD)
            ]
        );
        assert_eq!(markers.muted, None, "Outlook has no mute");
        assert_eq!(mailbox.folder_for(TRASH), Some("del"));
        assert_eq!(mailbox.folder_for("rec"), Some("rec"));
        assert!(mailbox.is_folder_key("rec") && !mailbox.is_folder_key("cat-1"));
    }
}
