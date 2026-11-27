//! Calendar accounts other than the mail account: the week sidebar's list of them, and signing
//! one in. The account holds the grants and does the reading
//! ([`crate::account::Account::sign_in_calendar`]). This module is what the calendar's UI asks
//! of the account.

use exo_store::{CalendarAccount, CalendarProviderKind, Settings};

use super::week::{AccountState, ListedAccount, ListedCalendar};
use crate::thread::invite::{Invite, Response};

/// Provider of a `settings.calendar_accounts` entry.
fn calendar_account_provider(key: &str, account: &CalendarAccount) -> CalendarProviderKind {
    let (parsed, _) = CalendarProviderKind::parse_account_key(key);
    if parsed == CalendarProviderKind::Microsoft || account.provider == "microsoft" {
        CalendarProviderKind::Microsoft
    } else {
        CalendarProviderKind::Google
    }
}

/// The winning signed-in `(account_key, provider)` for `email` under `settings`: the primary mail
/// account always wins for `own_email`; otherwise the first authed entry matching `email` in
/// `settings.calendar_accounts`.
fn winning_account_for_email(
    settings: &Settings,
    own_key: &str,
    own_email: &str,
    own_outlook: bool,
    email: &str,
) -> Option<(String, CalendarProviderKind)> {
    if email.eq_ignore_ascii_case(own_email) {
        let provider = if own_outlook {
            CalendarProviderKind::Microsoft
        } else {
            CalendarProviderKind::Google
        };
        return Some((own_key.to_owned(), provider));
    }
    settings
        .calendar_accounts
        .iter()
        .find(|(key, account)| account.authed && account.email_for(key).eq_ignore_ascii_case(email))
        .map(|(key, account)| (key.clone(), calendar_account_provider(key, account)))
}

/// Email addresses whose active winning calendar account is Microsoft/Outlook, matching
/// [`Mail::calendar_invite_target`] precedence.
fn outlook_emails_for(
    settings: &Settings,
    own_key: &str,
    own_email: &str,
    own_outlook: bool,
) -> Vec<String> {
    let mut emails = Vec::new();
    if own_outlook {
        emails.push(own_email.to_owned());
    }
    for (key, account) in &settings.calendar_accounts {
        let addr = account.email_for(key);
        if addr.eq_ignore_ascii_case(own_email)
            || emails
                .iter()
                .any(|seen: &String| seen.eq_ignore_ascii_case(addr))
        {
            continue;
        }
        if matches!(
            winning_account_for_email(settings, own_key, own_email, own_outlook, addr),
            Some((_, CalendarProviderKind::Microsoft))
        ) {
            emails.push(addr.to_owned());
        }
    }
    emails
}

/// What a calendar sign-in is for, which decides what its outcome does.
#[derive(Clone, Debug)]
pub(crate) enum SignInFor {
    /// From the list's `Sign in with Google` / `Sign in with Microsoft` or `Sign in again`,
    /// Calendar Settings' sign-in links, or the day view's `Add Calendar`, with nothing more to do
    /// once signed in.
    List,
    /// From the invite card's `RSVP` alert. Once the account signs in, the answer is sent.
    Invite(Box<Invite>, Response),
}

impl crate::Mail {
    /// The primary calendar's ID for the mail account: its loaded primary calendar ID, or
    /// `"primary"` on Outlook / the account email on Google before calendars finish loading.
    pub(super) fn calendar_primary_id(&self) -> String {
        let own_key = self.account.key();
        if let Some(found) = self
            .account
            .calendar()
            .data()
            .calendars
            .iter()
            .flatten()
            .find(|cal| cal.account == own_key && cal.calendar.primary)
        {
            return found.calendar.id.clone();
        }
        if self.account.is_outlook() {
            "primary".to_owned()
        } else {
            self.account.email().to_owned()
        }
    }

    /// The week sidebar's calendar accounts: those with calendars to list, those loading, and the
    /// signed-out ones. The signed-out ones go last, and within each group the mail account comes
    /// first and the others sort by address.
    pub(crate) fn calendar_accounts_listed(&self) -> Vec<ListedAccount> {
        let own_key = self.account.key();
        let settings = self.account.settings();
        let loading = self.account.calendar().data().loading_accounts.clone();
        let mut calendars = self.calendar_list();
        let mut listed: Vec<ListedAccount> = Vec::new();
        for (key, account) in &settings.calendar_accounts {
            let state = if !account.authed {
                AccountState::SignedOut
            } else if loading.contains(key) {
                AccountState::Loading
            } else {
                let (theirs, rest): (Vec<ListedCalendar>, Vec<ListedCalendar>) = calendars
                    .into_iter()
                    .partition(|calendar| calendar.account == *key);
                calendars = rest;
                if theirs.is_empty() {
                    continue;
                }
                AccountState::Calendars(theirs)
            };
            listed.push(ListedAccount {
                key: key.clone(),
                email: account.email_for(key).to_owned(),
                state,
            });
        }
        let sort_key = |account: &ListedAccount| {
            (
                matches!(account.state, AccountState::SignedOut),
                account.key != own_key,
                account.email.to_lowercase(),
                account.key.to_lowercase(),
            )
        };
        listed.sort_by_key(sort_key);
        listed
    }

    /// The week sidebar's calendar accounts as it lists them: each address, its calendars'
    /// names (none while they load), and whether it's signed out (for tests).
    pub fn calendar_account_list(&self) -> Vec<(String, Vec<String>, bool)> {
        self.calendar_accounts_listed()
            .into_iter()
            .map(|account| match account.state {
                AccountState::Calendars(calendars) => (
                    account.email,
                    calendars
                        .into_iter()
                        .map(|calendar| calendar.name)
                        .collect(),
                    false,
                ),
                AccountState::Loading => (account.email, Vec::new(), false),
                AccountState::SignedOut => (account.email, Vec::new(), true),
            })
            .collect()
    }

    /// The account's other calendar accounts' email addresses, signed in or not.
    pub(crate) fn calendar_others(&self) -> Vec<String> {
        let email = self.account.email();
        let mut others = Vec::new();
        for (key, account) in &self.account.settings().calendar_accounts {
            let addr = account.email_for(key);
            if !addr.eq_ignore_ascii_case(email)
                && !others
                    .iter()
                    .any(|seen: &String| seen.eq_ignore_ascii_case(addr))
            {
                others.push(addr.to_owned());
            }
        }
        others
    }

    /// The email addresses of other calendar accounts that are signed out (and have no other
    /// signed-in calendar account with the same address).
    pub(crate) fn calendar_signed_out(&self) -> Vec<String> {
        let primary_email = self.account.email();
        let settings = self.account.settings();
        let authed_emails: Vec<&str> = std::iter::once(primary_email)
            .chain(
                settings
                    .calendar_accounts
                    .iter()
                    .filter(|(_, account)| account.authed)
                    .map(|(key, account)| account.email_for(key)),
            )
            .collect();
        let mut signed_out = Vec::new();
        for (key, account) in &settings.calendar_accounts {
            if account.authed {
                continue;
            }
            let addr = account.email_for(key);
            if !authed_emails
                .iter()
                .any(|known| known.eq_ignore_ascii_case(addr))
                && !signed_out
                    .iter()
                    .any(|seen: &String| seen.eq_ignore_ascii_case(addr))
            {
                signed_out.push(addr.to_owned());
            }
        }
        signed_out
    }

    /// The email addresses whose active winning calendar account is Microsoft/Outlook.
    pub(crate) fn calendar_outlook_emails(&self) -> Vec<String> {
        outlook_emails_for(
            self.account.settings(),
            self.account.key(),
            self.account.email(),
            self.account.is_outlook(),
        )
    }

    /// The calendar account email `invite` was sent to, whose primary calendar answers it. That's
    /// the mail account, unless the only one invited is another calendar account of mine, signed in
    /// or not.
    pub(crate) fn calendar_invite_account(&self, invite: &Invite) -> String {
        invite
            .calendar_account(self.account.email(), &self.calendar_others())
            .to_owned()
    }

    /// Resolves `(calendar_id, account_key)` for answering `invite` via `RespondToInvite`.
    /// Prefers the primary calendar ID of the winning signed-in account (using the mail account
    /// first when both providers share an address), falling back to `"primary"` on Microsoft when
    /// calendars have not finished loading yet.
    pub(crate) fn calendar_invite_target(&self, invite: &Invite) -> (String, Option<String>) {
        let email = self.calendar_invite_account(invite);
        let winner = winning_account_for_email(
            self.account.settings(),
            self.account.key(),
            self.account.email(),
            self.account.is_outlook(),
            &email,
        );
        let data = self.account.calendar().data();
        if let Some((winner_key, provider)) = winner {
            if let Some(found) = data
                .calendars
                .iter()
                .flatten()
                .find(|cal| cal.account == winner_key && cal.calendar.primary)
            {
                return (found.calendar.id.clone(), Some(winner_key));
            }
            let calendar_id = match provider {
                CalendarProviderKind::Microsoft => "primary".to_owned(),
                CalendarProviderKind::Google => email,
            };
            return (calendar_id, Some(winner_key));
        }
        if let Some(found) = data
            .calendars
            .iter()
            .flatten()
            .find(|cal| cal.email.eq_ignore_ascii_case(&email) && cal.calendar.primary)
        {
            return (found.calendar.id.clone(), Some(found.account.clone()));
        }
        (email, None)
    }

    /// The calendar account email `invite` was sent to, if it's signed out and has to sign in
    /// before the invite can be answered.
    pub(crate) fn calendar_invite_needs_sign_in(&self, invite: &Invite) -> Option<String> {
        let email = self.calendar_invite_account(invite);
        self.calendar_signed_out()
            .iter()
            .any(|signed_out| signed_out.eq_ignore_ascii_case(&email))
            .then_some(email)
    }

    /// Starts signing a calendar account in with `provider` for `purpose`, with `hint` as the
    /// account to ask for.
    pub(crate) fn calendar_sign_in(
        &mut self,
        provider: exo_auth::Provider,
        hint: Option<&str>,
        purpose: SignInFor,
    ) {
        if self.account.sign_in_calendar(provider, hint) {
            self.calendar.signing_in = Some(purpose);
        } else {
            log::warn!(
                "signing a calendar account in needs Exo's sign-in, which this session wasn't started with"
            );
        }
    }

    /// Once a frame, picks up the outcome of a calendar sign-in and any grants that providers have
    /// refused for the calendar accounts.
    pub(crate) fn calendar_accounts_frame(&mut self) {
        self.account.check_calendar_grants();
        let (others, signed_out, outlook_emails) = (
            self.calendar_others(),
            self.calendar_signed_out(),
            self.calendar_outlook_emails(),
        );
        if let Some(thread) = &mut self.thread {
            thread.set_calendar_accounts(others, signed_out, outlook_emails);
        }
        let Some(outcome) = self.account.poll_calendar_sign_in() else {
            return;
        };
        let purpose = self.calendar.signing_in.take();
        match (outcome, purpose) {
            (Ok(_), Some(SignInFor::Invite(invite, response))) => {
                self.calendar_close_sign_in_prompt();
                let mut answered = *invite;
                answered.answer(&self.calendar_invite_account(&answered), response);
                self.calendar_respond_to_invite(answered, response, false);
            }
            (Ok(_), _) => {}
            // A failed sign-in from the list shows nothing, whether the user declined or signed
            // in with another account.
            (Err(error), Some(SignInFor::List) | None) => {
                log::info!("a calendar account didn't sign in: {error}")
            }
            (Err(error), Some(SignInFor::Invite(invite, _))) => {
                self.calendar_close_sign_in_prompt();
                log::info!("an invite's calendar account didn't sign in: {error}");
                let email = self.calendar_invite_account(&invite);
                match error {
                    // Declined on the provider's screen, or the browser never came back.
                    exo_auth::Error::AccessDenied | exo_auth::Error::TimedOut => {}
                    exo_auth::Error::WrongAccount { .. } => self.pickers.inform(
                        "Wrong email",
                        format!("Please ensure you sign in with {email}"),
                    ),
                    exo_auth::Error::Google(gmail::Error::Http(_))
                    | exo_auth::Error::Microsoft(outlook::Error::Http(_)) => self.pickers.inform(
                        "Offline",
                        "Please go online to respond to this invitation.".to_owned(),
                    ),
                    _ => self.pickers.inform(
                        "Sorry!",
                        "Something went wrong. Please try again in a minute.".to_owned(),
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(email: &str, provider: &str, authed: bool) -> CalendarAccount {
        CalendarAccount {
            email: email.into(),
            provider: provider.into(),
            authed,
            calendars: Default::default(),
        }
    }

    #[test]
    fn outlook_emails_respect_primary_precedence_and_auth_state() {
        // Google mail + added Microsoft calendar for the same email: primary Google wins,
        // so alice@company.com must NOT be treated as Outlook for invite RSVP UI.
        let mut settings = Settings::default();
        settings.calendar_accounts.insert(
            "alice@company.com".into(),
            account("alice@company.com", "google", true),
        );
        settings.calendar_accounts.insert(
            "microsoft alice@company.com".into(),
            account("alice@company.com", "microsoft", true),
        );
        // Signed-out Microsoft secondary calendar must NOT mark bob@company.com as Outlook.
        settings.calendar_accounts.insert(
            "microsoft bob@company.com".into(),
            account("bob@company.com", "microsoft", false),
        );
        // Dual-provider secondary email where Google is authed and Microsoft is authed:
        // Google sorts first in BTreeMap and wins both invite target and outlook_emails.
        settings.calendar_accounts.insert(
            "carol@company.com".into(),
            account("carol@company.com", "google", true),
        );
        settings.calendar_accounts.insert(
            "microsoft carol@company.com".into(),
            account("carol@company.com", "microsoft", true),
        );
        // Active secondary Microsoft calendar with no Google entry wins as Outlook.
        settings.calendar_accounts.insert(
            "microsoft dana@company.com".into(),
            account("dana@company.com", "microsoft", true),
        );

        assert_eq!(
            outlook_emails_for(&settings, "alice@company.com", "alice@company.com", false),
            vec!["dana@company.com"]
        );
        assert_eq!(
            winning_account_for_email(
                &settings,
                "alice@company.com",
                "alice@company.com",
                false,
                "alice@company.com"
            ),
            Some(("alice@company.com".into(), CalendarProviderKind::Google))
        );
        assert_eq!(
            winning_account_for_email(
                &settings,
                "alice@company.com",
                "alice@company.com",
                false,
                "dana@company.com"
            ),
            Some((
                "microsoft dana@company.com".into(),
                CalendarProviderKind::Microsoft
            ))
        );
        assert_eq!(
            winning_account_for_email(
                &settings,
                "alice@company.com",
                "alice@company.com",
                false,
                "bob@company.com"
            ),
            None
        );

        // Conversely, Microsoft mail + added Google calendar for the same email keeps Outlook.
        assert_eq!(
            outlook_emails_for(
                &settings,
                "microsoft alice@company.com",
                "alice@company.com",
                true
            ),
            vec!["alice@company.com", "dana@company.com"]
        );
    }
}
