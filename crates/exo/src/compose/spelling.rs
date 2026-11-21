//! Spelling and the right-click menu in a draft. macOS's spellchecker checks each draft's body,
//! which is corrected and completed as the account's autocorrect and autocomplete settings say.
//! The subject, and a snippet's name, are checked the way Chromium checks an `<input>`. A
//! right-click in any of them shows the shell's editable menu, with its spelling suggestions
//! first.

use exo_editor::{Named, People};
use exo_store::Address;

use crate::{
    Mail, desktop,
    format::names,
    spelling::{self, Done},
};

/// `address` named the way autocomplete takes it, with the first and last names from the
/// contact's display name. `None` without a display name.
fn named(address: &Address) -> Option<Named> {
    let full = address
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case(&address.email))?;
    let name = names::reverse_by_comma(full);
    let first = names::first_name(&name, &address.email);
    let last = name
        .split_once(first.as_str())
        .map(|(_, rest)| rest.trim())
        .filter(|rest| !rest.is_empty())
        .unwrap_or(&name)
        .to_owned();
    Some(Named {
        first,
        last,
        full: full.to_owned(),
    })
}

/// The people a draft is from and to, for autocomplete.
fn people(record: &exo_store::DraftRecord) -> People {
    let all = |addresses: &[Address]| addresses.iter().map(named).collect();
    People {
        from: record.from.as_ref().and_then(named),
        to: all(&record.to),
        cc: all(&record.cc),
        bcc: all(&record.bcc),
    }
}

/// The words of `names` that a spellchecker may not know, which autocorrect leaves alone.
fn name_words<'a>(names: impl Iterator<Item = &'a str>) -> impl Iterator<Item = String> {
    names
        .flat_map(|name| name.split(|ch: char| !ch.is_alphabetic() && ch != '\''))
        .filter(|word| word.chars().count() > 1)
        .map(str::to_owned)
}

impl Mail {
    /// The words in the names of the people in the address book, which autocorrect leaves alone
    /// in every editor.
    pub(crate) fn contact_names(&self) -> Vec<String> {
        let book = self.compose.book.as_ref().map(|(contacts, _)| contacts);
        name_words(
            book.into_iter()
                .flat_map(|book| book.iter().filter_map(|contact| contact.name.as_deref())),
        )
        .collect()
    }

    /// Call each frame, before anything draws. Gives the drafts' bodies and subjects their
    /// spellcheckers. After a right-click in one of them last frame, with the word the click
    /// selected now on screen, shows the menu and does what's chosen in it.
    pub(crate) fn compose_spelling(&mut self, ctx: &egui::Context) {
        let (autocorrect, autocomplete) =
            (self.autocorrect(), self.account.settings().autocomplete());
        let book = self
            .compose
            .book
            .as_ref()
            .map(|(contacts, _)| contacts.clone());
        for open in &mut self.compose.drafts {
            if !open.editor.has_spelling()
                && let Some(engine) = (self.spelling)()
            {
                open.editor.set_spelling(engine);
                open.editor
                    .set_greeting_languages(desktop::spelling::greeting_languages());
            }
            open.editor.set_people(people(&open.record));
            spelling::check_field(&mut open.subject_spelling, &*self.spelling);
            spelling::check_field(open.editor.link_spelling_mut(), &*self.spelling);
            spelling::check_field(&mut open.name_spelling, &*self.spelling);
            open.editor.set_autocorrect(autocorrect);
            open.editor.set_autocomplete(autocomplete);
            // The names of the people the draft may be to, which a spellchecker doesn't know.
            // They come from the address book and the draft's own recipients, and are set again
            // once the book is read.
            let from = book
                .as_ref()
                .map(|book| std::sync::Arc::as_ptr(book) as usize);
            if autocorrect && open.names_from != Some(from) {
                open.names_from = Some(from);
                let people = book
                    .iter()
                    .flat_map(|book| book.iter().map(|contact| contact.name.as_deref()));
                let recipients = [&open.record.to, &open.record.cc, &open.record.bcc]
                    .into_iter()
                    .flatten()
                    .map(|address| address.name.as_deref());
                open.editor
                    .set_names(name_words(people.chain(recipients).flatten()));
            }
        }
        if self.link_input_menu(ctx, |mail| {
            mail.compose.focused_mut().map(|open| &mut open.editor)
        }) {
            return;
        }
        let now = ctx.input(|input| input.time);
        let menu = self.compose.drafts.iter_mut().find_map(|open| {
            let id = open.id.clone();
            let (body, subject, name) = (
                open.context_menu.take(),
                open.subject_spelling.take_menu(),
                open.name_spelling.take_menu(),
            );
            body.map(|menu| (id.clone(), Menu::Body, menu))
                .or(subject.map(|menu| (id.clone(), Menu::Subject, menu)))
                .or(name.map(|menu| (id, Menu::Name, menu)))
        });
        let Some((id, which, menu)) = menu else {
            return;
        };
        let Some(choice) = self.editable_menu(&menu) else {
            return;
        };
        let Some(open) = self.compose.get_mut(&id) else {
            return;
        };
        let done = match which {
            Menu::Body => spelling::editor_choice(ctx, &mut open.editor, &menu, choice),
            Menu::Subject => spelling::field_choice(
                ctx,
                &mut open.subject_spelling,
                &mut open.record.subject,
                &menu,
                choice,
            ),
            Menu::Name => match &mut open.snippet {
                Some(snippet) => spelling::field_choice(
                    ctx,
                    &mut open.name_spelling,
                    &mut snippet.name,
                    &menu,
                    choice,
                ),
                None => Done::Nothing,
            },
        };
        match done {
            Done::Edited => open.touch(now),
            Done::Open(url) => self.open_chosen(&url),
            Done::Nothing => {}
        }
    }
}

/// Which of a draft's widgets a menu is for.
enum Menu {
    Body,
    Subject,
    Name,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_are_named_by_first_and_last_name() {
        let address = |name: Option<&str>, email: &str| Address {
            name: name.map(str::to_owned),
            email: email.to_owned(),
        };
        let priya = named(&address(Some("Raman, Priya"), "priya@brightmoor.co")).expect("named");
        assert_eq!(
            (
                priya.first.as_str(),
                priya.last.as_str(),
                priya.full.as_str()
            ),
            ("Priya", "Raman", "Raman, Priya")
        );
        let cher = named(&address(Some("Cher"), "cher@example.com")).expect("named");
        assert_eq!(cher.last, "Cher", "no more to the name: all of it");
        assert_eq!(named(&address(None, "tom@brightmoor.co")), None);
        assert_eq!(
            named(&address(Some("tom@brightmoor.co"), "tom@brightmoor.co")),
            None,
            "an address for a name is none"
        );
    }
}
