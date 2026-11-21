//! Deep links, parsed with the other links ([`crate::links`]). `exo://deeplink/{name}` opens a
//! setting in the account on screen, and `exo://{email}/deeplink/{name}` opens it in that
//! account, switching to it first. Each shows the home list first. A name for something Exo
//! leaves out (AI, what needs a server, teams) does only that. A name no deep link has also says
//! `Update Required`. `upgrade` checks for updates, as ⌘K's `Check for Updates` does, and
//! `split-library?role={name}` opens the split library on that role's templates.

use crate::{commands::Action, notice::Notice, settings};

/// What a deep link's name opens in Exo.
#[derive(Debug, PartialEq, Eq)]
enum Opens {
    /// Something Exo has.
    Action(Action),
    /// Something Exo leaves out, so only the home list shows.
    Nothing,
    /// A name no deep link has.
    UpdateRequired,
}

fn opens(name: &str, role: Option<&str>) -> Opens {
    use settings::{Action as Set, Dialog, Triage};
    let set = |action| Opens::Action(Action::Settings(action));
    match name {
        "emoji-settings" => set(Set::Open(Dialog::Emoji)),
        "reminder-settings" => set(Set::Open(Dialog::Reminders)),
        "calendar-settings" => set(Set::CalendarSettings),
        "split-inbox-settings" => set(Set::Open(Dialog::SplitInbox)),
        "split-library" => set(Set::SplitLibrary {
            role: role.map(str::to_owned),
        }),
        "hide-empty-split-inboxes" => set(Set::Open(Dialog::HideEmptySplits)),
        "get-me-to-zero" => set(Set::Triage(Triage::GetMeToZero)),
        "add-account" => Opens::Action(Action::Accounts(crate::accounts::Action::Add)),
        "create-snippet" => Opens::Action(Action::Snippets(crate::snippets::Action::New(
            String::new(),
        ))),
        "view-snippets" => Opens::Action(Action::Snippets(crate::snippets::Action::Go)),
        "upgrade" => Opens::Action(Action::Notice(crate::notice::Action::CheckForUpdates)),
        // AI, what needs a server and teams, which Exo leaves out, and feedback, which Exo
        // doesn't have yet.
        "auto-archive-settings"
        | "auto-labels-settings"
        | "auto-labels-library"
        | "auto-drafts-settings"
        | "ai-personalization"
        | "auto-archive"
        | "knowledge-base"
        | "open-ask-ai"
        | "share-feedback"
        | "add-to-team"
        | "create-team"
        | "manage-team"
        | "refer" => Opens::Nothing,
        _ => Opens::UpdateRequired,
    }
}

impl crate::Mail {
    /// Opens deep link `name` in this account, showing the home list and then what it names. A
    /// `role` is the link's `?role=`.
    pub(crate) fn open_deep_link(&mut self, name: &str, role: Option<&str>) {
        let now = self.ctx.input(|input| input.time);
        self.close_thread();
        self.run(
            Action::Go(crate::splits::home(self.account.settings())),
            now,
        );
        match opens(name, role) {
            Opens::Action(action) => self.run(action, now),
            Opens::Nothing => {}
            Opens::UpdateRequired => self.notice = Some(Notice::UpdateRequired),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_exo_leaves_out_do_nothing_and_unknown_ones_ask_for_an_update() {
        assert_eq!(
            opens("emoji-settings", None),
            Opens::Action(Action::Settings(settings::Action::Open(
                settings::Dialog::Emoji
            )))
        );
        assert_eq!(opens("auto-labels-settings", None), Opens::Nothing);
        assert_eq!(
            opens("upgrade", None),
            Opens::Action(Action::Notice(crate::notice::Action::CheckForUpdates))
        );
        assert_eq!(opens("teleport", None), Opens::UpdateRequired);
        assert_eq!(
            opens("split-library", Some("Sales")),
            Opens::Action(Action::Settings(settings::Action::SplitLibrary {
                role: Some("Sales".into())
            }))
        );
    }
}
