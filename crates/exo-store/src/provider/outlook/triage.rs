//! Label changes as Graph makes them, message by message: a move to the folder the new labels put
//! the message in, then its read state, flag, categories and Focused verdict, then Exo's markers.
//!
//! Each change is worked out from the messages as Graph has them now, so a change sent again
//! after a lost answer does nothing twice. A message sits in one folder, picked from its labels
//! by precedence: Deleted Items, Junk Email, the Inbox, a user folder (the one added), Sent Items,
//! else Archive. So archiving a conversation leaves what's in Sent Items there, and moving it to
//! a folder takes all of it. Drafts move only to Deleted Items. Unstarring completes the flag, as
//! Outlook's own apps do. The markers go on after the moves, so a reminder's marker never depends
//! on surviving one.

use std::collections::BTreeSet;

use outlook::{
    Call, Client, FollowupFlag, FollowupFlagStatus, InferenceClassificationType, Message,
    MessagePatch, OpenTypeExtension,
};

use serde::de::IgnoredAny;

use super::Outlook;
use super::graph::failed;
use super::mailbox::{Mailbox, REMINDER_UNREAD, Role, SNOOZED};
use super::parse::{LABELS, expand};
use crate::api::Failure;
use crate::ids::labels::{
    CATEGORY_PERSONAL, DRAFT, EXO_REMINDER_UNREAD, EXO_SNOOZED, INBOX, SENT, SPAM, STARRED, TRASH,
    UNREAD,
};
use crate::mutation::Step;
use crate::provider::Labelled;
use crate::quota::Lane;

/// One call of a change.
#[derive(Debug, Clone, PartialEq)]
enum Op {
    /// Moves a message to a folder, by id.
    Move(String, String),
    /// Changes a message's state.
    Patch(String, MessagePatch),
    /// Puts one of Exo's markers on a message.
    Mark(String, &'static str),
    /// Takes one off.
    Unmark(String, &'static str),
}

impl Op {
    fn call<'a>(&self, graph: &'a Client) -> Call<'a, IgnoredAny> {
        let mail = graph.mail();
        match self {
            Self::Move(id, folder) => mail.move_message(id, folder).discarding(),
            Self::Patch(id, patch) => mail.update_message(id, patch).discarding(),
            Self::Mark(id, name) => mail
                .create_extension(id, &OpenTypeExtension::new(name, serde_json::Map::new()))
                .discarding(),
            Self::Unmark(id, name) => mail.delete_extension(id, name).discarding(),
        }
    }

    /// Whether `error` means there was nothing left to do: the message is gone, or already has
    /// (or hasn't) the marker.
    fn moot(&self, error: &outlook::Error) -> bool {
        error.is_not_found()
            || error.code() == Some("ErrorInvalidIdMalformed")
            || matches!(self, Self::Mark(..)) && error.status() == Some(409)
    }
}

impl Outlook {
    /// One outbox step, in one try.
    pub(super) async fn apply(&self, thread: &str, step: &Step) -> Result<(), Failure> {
        let (lane, attempts) = (Lane::Act, 1);
        match step {
            Step::RemoveFromMessages { ids, labels } => {
                self.relabel_messages_in(ids, &[], labels, lane, attempts)
                    .await
            }
            Step::ModifyMessages { ids, add, remove } => {
                self.relabel_messages_in(ids, add, remove, lane, attempts)
                    .await
            }
            Step::ModifyThread { add, remove } => {
                self.relabel_thread_in(thread, add, remove, lane, attempts)
                    .await
            }
            Step::AddToMessage { id, label } => {
                self.relabel_messages_in(
                    std::slice::from_ref(id),
                    std::slice::from_ref(label),
                    &[],
                    lane,
                    attempts,
                )
                .await
            }
            // A refresh reads the conversation again ([`crate::provider::Provider::rows`]).
            Step::Refresh => Ok(()),
        }
    }

    /// Bulk's change to messages `ids`.
    pub(super) async fn relabel_messages(
        &self,
        ids: &[String],
        add: &[String],
        remove: &[String],
    ) -> Result<(), Failure> {
        self.relabel_messages_in(ids, add, remove, Lane::Sync, crate::bulk::ATTEMPTS)
            .await
    }

    /// Bulk's change to every message of conversation `id`.
    pub(super) async fn relabel_thread(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), Failure> {
        self.relabel_thread_in(id, add, remove, Lane::Sync, crate::bulk::ATTEMPTS)
            .await
    }

    /// Conversation `id`'s messages, each with its label keys. `None` when it's gone.
    pub(super) async fn thread_labels(&self, id: &str) -> Result<Option<Labelled>, Failure> {
        let messages = self.labelled(id, Lane::Act, 1).await?;
        let mailbox = self.mailbox_for(messages.iter()).await?;
        let labelled: Labelled = messages
            .iter()
            .filter(|message| {
                mailbox.is_read(message.parent_folder_id.as_deref().unwrap_or_default())
            })
            .filter_map(|message| Some((message.id.clone()?, mailbox.labels(message, ""))))
            .collect();
        Ok((!labelled.is_empty()).then_some(labelled))
    }

    async fn relabel_thread_in(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
        lane: Lane,
        attempts: u32,
    ) -> Result<(), Failure> {
        let messages = self.labelled(id, lane, attempts).await?;
        self.change(&messages, add, remove, lane, attempts).await
    }

    async fn relabel_messages_in(
        &self,
        ids: &[String],
        add: &[String],
        remove: &[String],
        lane: Lane,
        attempts: u32,
    ) -> Result<(), Failure> {
        let expand = expand(false);
        let messages: Vec<Message> = self
            .graph
            .each(lane, attempts, ids.len(), |graph, at| {
                graph
                    .mail()
                    .message(&ids[at])
                    .select(LABELS)
                    .expand(&expand)
            })
            .await?
            .into_iter()
            .filter_map(|found| match found {
                Ok(message) => Some(Ok(message)),
                // A message gone has nothing left to change.
                Err(error) if error.is_not_found() => None,
                Err(error) => Some(Err(failed(error))),
            })
            .collect::<Result<_, _>>()?;
        self.change(&messages, add, remove, lane, attempts).await
    }

    /// Conversation `id`'s messages, read for their labels.
    async fn labelled(&self, id: &str, lane: Lane, attempts: u32) -> Result<Vec<Message>, Failure> {
        let expand = expand(false);
        Ok(self
            .graph
            .call(lane, attempts, |graph| {
                graph
                    .mail()
                    .conversation(id)
                    .select(LABELS)
                    .expand(&expand)
                    .top(100)
                    .all()
            })
            .await?
            .value)
    }

    /// Adds `add` to and removes `remove` from `messages`, as calls in batches: every move, then
    /// every other change, then the markers.
    async fn change(
        &self,
        messages: &[Message],
        add: &[String],
        remove: &[String],
        lane: Lane,
        attempts: u32,
    ) -> Result<(), Failure> {
        let mailbox = self.mailbox_for(messages.iter()).await?;
        let mut ops: Vec<Op> = messages
            .iter()
            .filter(|message| {
                mailbox.is_read(message.parent_folder_id.as_deref().unwrap_or_default())
            })
            .flat_map(|message| ops(&mailbox, message, add, remove))
            .collect();
        // Moves first, and the markers last.
        ops.sort_by_key(|op| match op {
            Op::Move(..) => 0,
            Op::Patch(..) => 1,
            Op::Mark(..) | Op::Unmark(..) => 2,
        });
        if ops.is_empty() {
            return Ok(());
        }
        let answers = self
            .graph
            .each(lane, attempts, ops.len(), |graph, at| ops[at].call(graph))
            .await?;
        for (op, answer) in ops.iter().zip(answers) {
            match answer {
                Ok(_) => {}
                Err(error) if op.moot(&error) => {}
                Err(error) => {
                    log::debug!("Graph refused {op:?}: {error}");
                    return Err(failed(error));
                }
            }
        }
        Ok(())
    }
}

/// The calls that add `add` to and remove `remove` from `message`, as `mailbox` keys its labels.
fn ops(mailbox: &Mailbox, message: &Message, add: &[String], remove: &[String]) -> Vec<Op> {
    let Some(id) = message.id.clone() else {
        return Vec::new();
    };
    let now: BTreeSet<String> = mailbox.labels(message, "").into_iter().collect();
    let mut then = now.clone();
    then.extend(add.iter().cloned());
    for label in remove {
        then.remove(label);
    }
    let has = |labels: &BTreeSet<String>, label: &str| labels.contains(label);
    let changed = |label: &str| has(&now, label) != has(&then, label);
    let mut ops = Vec::new();

    let here = message.parent_folder_id.as_deref().unwrap_or_default();
    if let Some(to) = folder(mailbox, message, &then, add).filter(|to| *to != here) {
        ops.push(Op::Move(id.clone(), to.to_owned()));
    }

    let mut patch = MessagePatch::default();
    if changed(UNREAD) {
        patch.is_read = Some(!has(&then, UNREAD));
    }
    if changed(STARRED) {
        patch.flag = Some(FollowupFlag::new(if has(&then, STARRED) {
            FollowupFlagStatus::Flagged
        } else {
            FollowupFlagStatus::Complete
        }));
    }
    if changed(CATEGORY_PERSONAL) {
        patch.inference_classification = Some(if has(&then, CATEGORY_PERSONAL) {
            InferenceClassificationType::Focused
        } else {
            InferenceClassificationType::Other
        });
    }
    let categories = |labels: &BTreeSet<String>| -> BTreeSet<String> {
        labels
            .iter()
            .filter_map(|key| mailbox.category_name(key))
            .map(str::to_owned)
            .collect()
    };
    let (had, has_now) = (categories(&now), categories(&then));
    if had != has_now {
        // Every category it carries, those off the master list included, less those taken off.
        let mut names: Vec<String> = message
            .categories
            .iter()
            .filter(|name| !had.contains(*name) || has_now.contains(*name))
            .cloned()
            .collect();
        names.extend(has_now.difference(&had).cloned());
        patch.categories = Some(names);
    }
    if patch != MessagePatch::default() {
        ops.push(Op::Patch(id.clone(), patch));
    }

    for (key, name) in [
        (EXO_SNOOZED, SNOOZED),
        (EXO_REMINDER_UNREAD, REMINDER_UNREAD),
    ] {
        if changed(key) {
            ops.push(if has(&then, key) {
                Op::Mark(id.clone(), name)
            } else {
                Op::Unmark(id.clone(), name)
            });
        }
    }
    ops
}

/// The folder `message` belongs in with labels `then`, of which `add` were added. `None` when it
/// stays where it is: a draft goes only to Deleted Items.
fn folder<'a>(
    mailbox: &'a Mailbox,
    message: &'a Message,
    then: &BTreeSet<String>,
    add: &'a [String],
) -> Option<&'a str> {
    let here = message.parent_folder_id.as_deref()?;
    let has = |label: &str| then.contains(label);
    if message.is_draft == Some(true) || has(DRAFT) {
        return has(TRASH).then(|| mailbox.folder(Role::Deleted)).flatten();
    }
    let role = |role| mailbox.folder(role);
    if has(TRASH) {
        return role(Role::Deleted);
    }
    if has(SPAM) {
        return role(Role::Junk);
    }
    if has(INBOX) {
        return role(Role::Inbox);
    }
    let user = |key: &&String| mailbox.is_user_folder(key);
    if let Some(added) = add.iter().filter(user).find(|key| has(key)) {
        return mailbox.folder_for(added);
    }
    let here_trashed = mailbox
        .folder_labels(here)
        .iter()
        .any(|label| *label == TRASH || *label == SPAM);
    if !here_trashed && then.iter().filter(user).any(|key| key == here) {
        return Some(here);
    }
    if has(SENT) {
        return role(Role::Sent);
    }
    role(Role::Archive).or(Some(outlook::well_known::ARCHIVE))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use outlook::MailFolder;
    use outlook::settings::OutlookCategory;

    use super::*;

    fn mailbox() -> Mailbox {
        let roles = HashMap::from(
            [
                ("in", Role::Inbox),
                ("sent", Role::Sent),
                ("del", Role::Deleted),
                ("junk", Role::Junk),
                ("arch", Role::Archive),
                ("drafts", Role::Drafts),
            ]
            .map(|(id, role)| (id.to_owned(), role)),
        );
        let folder = |id: &str, parent: &str| MailFolder {
            id: Some(id.into()),
            display_name: Some(id.into()),
            parent_folder_id: Some(parent.into()),
            ..MailFolder::default()
        };
        let folders = ["in", "sent", "del", "junk", "arch", "drafts"]
            .map(|id| folder(id, "root"))
            .into_iter()
            .chain([folder("rec", "in")])
            .collect::<Vec<_>>();
        let categories = [OutlookCategory {
            id: Some("cat-red".into()),
            display_name: Some("Red".into()),
            color: None,
        }];
        Mailbox::new(roles, &folders, &categories)
    }

    fn message(folder: &str) -> Message {
        Message {
            id: Some("m".into()),
            parent_folder_id: Some(folder.into()),
            is_read: Some(true),
            ..Message::default()
        }
    }

    fn owned(labels: &[&str]) -> Vec<String> {
        labels.iter().map(|label| (*label).to_owned()).collect()
    }

    fn changes(message: &Message, add: &[&str], remove: &[&str]) -> Vec<Op> {
        ops(&mailbox(), message, &owned(add), &owned(remove))
    }

    #[test]
    fn done_archives_but_leaves_sent_items() {
        assert_eq!(
            changes(&message("in"), &[], &[INBOX]),
            [Op::Move("m".into(), "arch".into())]
        );
        assert!(changes(&message("sent"), &[], &[INBOX]).is_empty());
        assert!(
            changes(&message("rec"), &[], &[INBOX]).is_empty(),
            "a folder's mail isn't in the Inbox"
        );
        assert_eq!(
            changes(&message("arch"), &[INBOX], &[]),
            [Op::Move("m".into(), "in".into())]
        );
    }

    #[test]
    fn trash_spam_and_folders_take_the_whole_conversation() {
        assert_eq!(
            changes(&message("sent"), &[TRASH], &[SPAM, INBOX]),
            [Op::Move("m".into(), "del".into())]
        );
        assert_eq!(
            changes(&message("in"), &[SPAM], &[INBOX, TRASH]),
            [Op::Move("m".into(), "junk".into())]
        );
        assert_eq!(
            changes(&message("sent"), &["rec"], &[INBOX, SPAM, TRASH]),
            [Op::Move("m".into(), "rec".into())]
        );
        assert_eq!(
            changes(&message("del"), &[INBOX], &[SPAM, TRASH]),
            [Op::Move("m".into(), "in".into())],
            "out of Trash"
        );
    }

    #[test]
    fn drafts_only_go_to_deleted_items() {
        let draft = Message {
            is_draft: Some(true),
            ..message("drafts")
        };
        assert!(changes(&draft, &[], &[INBOX]).is_empty());
        assert!(changes(&draft, &["rec"], &[INBOX]).is_empty());
        assert_eq!(
            changes(&draft, &[TRASH], &[INBOX]),
            [Op::Move("m".into(), "del".into())]
        );
    }

    #[test]
    fn read_flags_focused_and_categories_are_one_patch() {
        let read = changes(&message("in"), &[UNREAD, STARRED, "cat-red"], &[]);
        let Op::Patch(_, patch) = &read[0] else {
            panic!("{read:?}");
        };
        assert_eq!(patch.is_read, Some(false));
        assert_eq!(
            patch.flag.as_ref().and_then(|flag| flag.flag_status),
            Some(FollowupFlagStatus::Flagged)
        );
        assert_eq!(patch.categories.as_deref(), Some(&["Red".to_owned()][..]));

        let starred = Message {
            flag: Some(FollowupFlag::new(FollowupFlagStatus::Flagged)),
            categories: vec!["Red".into(), "Unlisted".into()],
            inference_classification: Some(InferenceClassificationType::Focused),
            ..message("in")
        };
        let unstarred = changes(
            &starred,
            &["CATEGORY_SOCIAL"],
            &[STARRED, "cat-red", CATEGORY_PERSONAL],
        );
        let Op::Patch(_, patch) = &unstarred[0] else {
            panic!("{unstarred:?}");
        };
        assert_eq!(
            patch.flag.as_ref().and_then(|flag| flag.flag_status),
            Some(FollowupFlagStatus::Complete),
            "unstarring completes the flag"
        );
        assert_eq!(
            patch.categories.as_deref(),
            Some(&["Unlisted".to_owned()][..]),
            "a category off the list stays"
        );
        assert_eq!(
            patch.inference_classification,
            Some(InferenceClassificationType::Other)
        );
        assert!(
            changes(&starred, &[STARRED], &[]).is_empty(),
            "nothing to do"
        );
    }

    #[test]
    fn markers_go_on_after_the_move() {
        let snoozed = changes(&message("in"), &[EXO_SNOOZED], &[INBOX]);
        assert_eq!(
            snoozed,
            [
                Op::Move("m".into(), "arch".into()),
                Op::Mark("m".into(), SNOOZED)
            ]
        );
        let marked = Message {
            extensions: vec![OpenTypeExtension::new(SNOOZED, serde_json::Map::new())],
            ..message("arch")
        };
        assert_eq!(
            changes(&marked, &[INBOX, EXO_REMINDER_UNREAD], &[EXO_SNOOZED]),
            [
                Op::Move("m".into(), "in".into()),
                Op::Unmark("m".into(), SNOOZED),
                Op::Mark("m".into(), REMINDER_UNREAD),
            ]
        );
    }

    #[test]
    fn untrashing_from_deleted_or_junk_subfolder_leaves_it_and_archive_falls_back_to_well_known() {
        let mut roles = HashMap::from(
            [
                ("in", Role::Inbox),
                ("sent", Role::Sent),
                ("del", Role::Deleted),
                ("junk", Role::Junk),
                ("drafts", Role::Drafts),
            ]
            .map(|(id, role)| (id.to_owned(), role)),
        );
        let mk_folder = |id: &str, parent: &str| MailFolder {
            id: Some(id.into()),
            display_name: Some(id.into()),
            parent_folder_id: Some(parent.into()),
            ..MailFolder::default()
        };
        let folders = vec![
            mk_folder("in", "root"),
            mk_folder("sent", "root"),
            mk_folder("del", "root"),
            mk_folder("junk", "root"),
            mk_folder("drafts", "root"),
            mk_folder("old", "del"),
            mk_folder("junk_sub", "junk"),
        ];

        // No Archive folder in `roles`: Done/Archive falls back to well-known `"archive"`.
        let without_archive = Mailbox::new(roles.clone(), &folders, &[]);
        assert_eq!(
            ops(&without_archive, &message("in"), &[], &owned(&[INBOX])),
            [Op::Move("m".into(), outlook::well_known::ARCHIVE.into())]
        );

        // Untrashing / moving to Done from a subfolder of Deleted Items or Junk Email moves out
        // instead of keeping the subfolder.
        roles.insert("arch".to_owned(), Role::Archive);
        let with_archive = Mailbox::new(roles, &folders, &[]);
        assert_eq!(
            ops(
                &with_archive,
                &message("old"),
                &[],
                &owned(&[INBOX, SPAM, TRASH])
            ),
            [Op::Move("m".into(), "arch".into())],
            "untrashing to Done leaves Deleted Items subfolder"
        );
        assert_eq!(
            ops(
                &with_archive,
                &message("junk_sub"),
                &[],
                &owned(&[INBOX, SPAM, TRASH])
            ),
            [Op::Move("m".into(), "arch".into())],
            "unspamming to Done leaves Junk Email subfolder"
        );
    }
}
