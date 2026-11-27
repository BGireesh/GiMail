//! The signed-in accounts, in the user's order, kept in `accounts.json` in the state folder.
//!
//! New accounts are appended, signing in again keeps an account's place, and the order is the one
//! ctrl+1…9 and the switchers use. An account its provider signed out stays listed until it signs
//! in again, and one account is the one launch opens. Only metadata lives here. Refresh tokens are
//! in the [`SecretStore`](crate::SecretStore).
//!
//! An account is its provider and its address ([`Account::key`]): a Microsoft account can have
//! any address, a Gmail one included, so a Google and a Microsoft account may share one.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Error;

/// The file's name in the state folder.
const FILE_NAME: &str = "accounts.json";

/// What a Microsoft account's key starts with. No address has a space, so no Google account's
/// key does.
const MICROSOFT_KEY: &str = "microsoft ";

/// What a calendar grant's secret-store key starts with (`calendar {calendar_key} for {owner}`).
pub(crate) const CALENDAR_KEY: &str = "calendar ";

/// Who an account signs in with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// Gmail and Google Workspace. Accounts listed before Exo knew other providers are Google's.
    #[default]
    Google,
    /// Outlook.com and Microsoft 365, through Microsoft Graph.
    Microsoft,
}

impl Provider {
    fn is_google(&self) -> bool {
        *self == Self::Google
    }
}

/// An account that signed in to Exo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// The mailbox's address.
    pub email: String,
    /// Who it signs in with. A Google account's entry leaves it out, as it always has.
    #[serde(default, skip_serializing_if = "Provider::is_google")]
    pub provider: Provider,
    /// The account's full name, when Google has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its provider refused its grant. It's listed, but has to sign in again before its mail
    /// opens.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub signed_out: bool,
    /// The account used last, which launch opens.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub last_used: bool,
    /// The color picked for it in the Accounts dialog, by name (`thistle`, `sea`, …). Without one,
    /// it's computed from the address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// When it first signed in to Exo on this Mac, which is the account's join date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joined: Option<jiff::Timestamp>,
    /// The other accounts signed in for this account's calendars alone, whose grants Exo keeps for
    /// it, named by each calendar account's [`Account::key`] (`{email}` for Google,
    /// `microsoft {email}` for Microsoft).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calendars: Vec<String>,
}

impl Account {
    /// What names the account in the list, the secret store and every [`crate::Auth`] call: a
    /// Google account's address, or `microsoft {address}`.
    #[must_use]
    pub fn key(&self) -> String {
        key(self.provider, &self.email)
    }

    /// The canonical key for `provider`'s account at `email` (trimmed and lowercased): `{email}`
    /// for Google, or `microsoft {email}` for Microsoft.
    #[must_use]
    pub fn key_for(provider: Provider, email: &str) -> String {
        key(provider, &normalize_email(email))
    }

    /// The provider and address a key names.
    #[must_use]
    pub fn parse_key(key: &str) -> (Provider, &str) {
        parse_key(key)
    }
}

/// The key of `provider`'s account at `email`.
pub(crate) fn key(provider: Provider, email: &str) -> String {
    match provider {
        Provider::Google => email.to_owned(),
        Provider::Microsoft => format!("{MICROSOFT_KEY}{email}"),
    }
}

/// The provider and address a key names.
pub(crate) fn parse_key(key: &str) -> (Provider, &str) {
    let trimmed = key.trim();
    if let Some((prefix, rest)) = trimmed.split_at_checked(MICROSOFT_KEY.len())
        && prefix.eq_ignore_ascii_case(MICROSOFT_KEY)
    {
        (Provider::Microsoft, rest.trim_start())
    } else {
        (Provider::Google, trimmed)
    }
}

/// The account list and the file it's saved in.
#[derive(Debug)]
pub(crate) struct Registry {
    path: PathBuf,
    accounts: Vec<Account>,
}

/// Canonical address: trimmed ASCII lowercase.
pub(crate) fn normalize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

/// Canonical account key: trimmed ASCII lowercase, with canonical `microsoft ` prefix spacing.
pub(crate) fn normalize_key(input: &str) -> String {
    let (provider, email) = parse_key(input);
    key(provider, &normalize_email(email))
}

fn find_in<'a>(accounts: &'a mut [Account], key: &str) -> Result<&'a mut Account, Error> {
    accounts
        .iter_mut()
        .find(|account| account.key() == key)
        .ok_or_else(|| Error::UnknownAccount(key.to_owned()))
}

impl Registry {
    /// The list saved in `dir`, empty when there's none yet.
    pub(crate) fn load(dir: &Path) -> Result<Self, Error> {
        let path = dir.join(FILE_NAME);
        let mut accounts: Vec<Account> = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| Error::Accounts {
                path: path.clone(),
                source: error.into(),
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(source) => return Err(Error::Accounts { path, source }),
        };
        for account in &mut accounts {
            account.email = normalize_email(&account.email);
            for calendar in &mut account.calendars {
                *calendar = normalize_key(calendar);
            }
        }
        Ok(Self { path, accounts })
    }

    pub(crate) fn accounts(&self) -> &[Account] {
        &self.accounts
    }

    pub(crate) fn contains(&self, key: &str) -> bool {
        self.accounts.iter().any(|account| account.key() == key)
    }

    /// The account launch opens: the one used last, unless it's signed out, in which case the
    /// first that isn't.
    pub(crate) fn default_account(&self) -> Option<&Account> {
        let signed_in = |account: &&Account| !account.signed_out;
        self.accounts
            .iter()
            .filter(signed_in)
            .find(|account| account.last_used)
            .or_else(|| self.accounts.iter().find(signed_in))
    }

    /// Records a sign-in. A new account goes at the end, joined now. An account already listed
    /// keeps its place, its color and its join date, and is signed in again. Either way its name
    /// is its provider's latest, and it becomes the one used last.
    pub(crate) fn sign_in(&mut self, mut account: Account) -> Result<(), Error> {
        account.email = normalize_email(&account.email);
        let key = account.key();
        self.commit(|accounts| {
            match accounts.iter_mut().find(|known| known.key() == key) {
                Some(known) => {
                    known.name = account.name;
                    known.signed_out = false;
                }
                None => accounts.push(Account {
                    signed_out: false,
                    joined: Some(jiff::Timestamp::now()),
                    ..account
                }),
            }
            for known in accounts {
                known.last_used = known.key() == key;
            }
            Ok(true)
        })
        .map(|_| ())
    }

    /// Makes account `key` the one used last.
    pub(crate) fn use_account(&mut self, key: &str) -> Result<(), Error> {
        self.commit(|accounts| {
            if find_in(accounts, key)?.last_used {
                return Ok(false);
            }
            for account in accounts {
                account.last_used = account.key() == key;
            }
            Ok(true)
        })
        .map(|_| ())
    }

    /// Marks account `key` signed out by its provider.
    pub(crate) fn sign_out(&mut self, key: &str) -> Result<(), Error> {
        self.commit(|accounts| {
            let account = find_in(accounts, key)?;
            if account.signed_out {
                return Ok(false);
            }
            account.signed_out = true;
            Ok(true)
        })
        .map(|_| ())
    }

    /// Sets the color account `key` shows in. `None` means the one computed from its address.
    pub(crate) fn set_color(&mut self, key: &str, color: Option<String>) -> Result<(), Error> {
        self.commit(|accounts| {
            find_in(accounts, key)?.color = color;
            Ok(true)
        })
        .map(|_| ())
    }

    /// `owner` holds a grant for calendar account `email`.
    pub(crate) fn add_calendar(&mut self, owner: &str, email: &str) -> Result<(), Error> {
        self.commit(|accounts| {
            let account = find_in(accounts, owner)?;
            if account.calendars.iter().any(|known| known == email) {
                return Ok(false);
            }
            account.calendars.push(email.to_owned());
            Ok(true)
        })
        .map(|_| ())
    }

    /// `owner` no longer holds a grant for calendar account `email`.
    pub(crate) fn remove_calendar(&mut self, owner: &str, email: &str) -> Result<(), Error> {
        self.commit(|accounts| {
            let account = find_in(accounts, owner)?;
            let before = account.calendars.len();
            account.calendars.retain(|known| known != email);
            Ok(account.calendars.len() != before)
        })
        .map(|_| ())
    }

    /// Takes account `key` out of the list, and returns whether it was there.
    pub(crate) fn remove(&mut self, key: &str) -> Result<bool, Error> {
        self.commit(|accounts| {
            let before = accounts.len();
            accounts.retain(|account| account.key() != key);
            Ok(accounts.len() != before)
        })
    }

    /// Moves account `key` to position `to` (clamped to the end), shifting the accounts in
    /// between.
    pub(crate) fn move_to(&mut self, key: &str, to: usize) -> Result<(), Error> {
        self.commit(|accounts| {
            let from = accounts
                .iter()
                .position(|account| account.key() == key)
                .ok_or_else(|| Error::UnknownAccount(key.to_owned()))?;
            if from == to.min(accounts.len() - 1) {
                return Ok(false);
            }
            let account = accounts.remove(from);
            accounts.insert(to.min(accounts.len()), account);
            Ok(true)
        })
        .map(|_| ())
    }

    /// Applies `mutate` to `self.accounts` and persists when it reports a change, restoring the
    /// previous list if writing fails so memory and disk never diverge.
    fn commit(
        &mut self,
        mutate: impl FnOnce(&mut Vec<Account>) -> Result<bool, Error>,
    ) -> Result<bool, Error> {
        let previous = self.accounts.clone();
        let changed = match mutate(&mut self.accounts) {
            Ok(changed) => changed,
            Err(error) => {
                self.accounts = previous;
                return Err(error);
            }
        };
        if changed && let Err(error) = self.save() {
            self.accounts = previous;
            return Err(error);
        }
        Ok(changed)
    }

    /// Writes the list through a temporary file, so a crash never leaves half of it.
    fn save(&self) -> Result<(), Error> {
        let failed = |source| Error::Accounts {
            path: self.path.clone(),
            source,
        };
        let json = serde_json::to_vec_pretty(&self.accounts).expect("accounts always serialize");
        let partial = self.path.with_extension("json.partial");
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(failed)?;
        }
        std::fs::write(&partial, json).map_err(failed)?;
        std::fs::rename(&partial, &self.path).map_err(failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(email: &str) -> Account {
        Account {
            email: email.into(),
            ..Account::default()
        }
    }

    fn emails(registry: &Registry) -> Vec<&str> {
        registry
            .accounts()
            .iter()
            .map(|account| account.email.as_str())
            .collect()
    }

    #[test]
    fn keeps_order_across_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::load(dir.path()).unwrap();
        assert!(registry.accounts().is_empty());
        for email in ["a@x.com", "b@x.com", "c@x.com"] {
            registry.sign_in(account(email)).unwrap();
        }
        registry
            .sign_in(Account {
                name: Some("Bea".into()),
                ..account("b@x.com")
            })
            .unwrap();
        registry.move_to("c@x.com", 0).unwrap();
        assert!(registry.remove("a@x.com").unwrap());
        assert!(!registry.remove("a@x.com").unwrap());

        let reloaded = Registry::load(dir.path()).unwrap();
        assert_eq!(emails(&reloaded), ["c@x.com", "b@x.com"]);
        assert_eq!(reloaded.accounts()[1].name.as_deref(), Some("Bea"));
        assert!(!dir.path().join("accounts.json.partial").exists());
    }

    #[test]
    fn moves_clamp_to_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::load(&dir.path().join("new")).unwrap();
        for email in ["a@x.com", "b@x.com", "c@x.com"] {
            registry.sign_in(account(email)).unwrap();
        }
        registry.move_to("a@x.com", 99).unwrap();
        assert_eq!(emails(&registry), ["b@x.com", "c@x.com", "a@x.com"]);
        registry.move_to("a@x.com", 1).unwrap();
        assert_eq!(emails(&registry), ["b@x.com", "a@x.com", "c@x.com"]);
        assert!(matches!(
            registry.move_to("z@x.com", 0),
            Err(Error::UnknownAccount(email)) if email == "z@x.com"
        ));
    }

    #[test]
    fn launch_opens_the_last_used_account_unless_google_signed_it_out() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::load(dir.path()).unwrap();
        assert_eq!(registry.default_account(), None);
        for email in ["a@x.com", "b@x.com", "c@x.com"] {
            registry.sign_in(account(email)).unwrap();
        }
        let default = |registry: &Registry| {
            registry
                .default_account()
                .map(|account| account.email.clone())
        };
        assert_eq!(
            default(&registry).as_deref(),
            Some("c@x.com"),
            "the last to sign in"
        );
        registry.use_account("b@x.com").unwrap();
        assert_eq!(
            default(&Registry::load(dir.path()).unwrap()).as_deref(),
            Some("b@x.com"),
            "kept across launches"
        );

        registry.sign_out("b@x.com").unwrap();
        registry.sign_out("a@x.com").unwrap();
        assert_eq!(
            default(&registry).as_deref(),
            Some("c@x.com"),
            "the first still signed in"
        );
        registry.sign_out("c@x.com").unwrap();
        assert_eq!(default(&registry), None);

        registry.set_color("a@x.com", Some("sea".into())).unwrap();
        let joined = registry.accounts()[0].joined;
        assert!(joined.is_some(), "a new account joins as it first signs in");
        registry
            .sign_in(Account {
                name: Some("Ann".into()),
                ..account("a@x.com")
            })
            .unwrap();
        let reloaded = Registry::load(dir.path()).unwrap();
        let a = &reloaded.accounts()[0];
        assert_eq!(
            (
                a.email.as_str(),
                a.name.as_deref(),
                a.color.as_deref(),
                a.signed_out
            ),
            ("a@x.com", Some("Ann"), Some("sea"), false),
            "signing in again keeps its place and color"
        );
        assert_eq!(a.joined, joined, "and its join date, across launches");
        assert_eq!(default(&reloaded).as_deref(), Some("a@x.com"));
    }

    #[test]
    fn fields_left_out_take_their_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(FILE_NAME),
            r#"[{"email": "a@x.com", "name": "Ann"}]"#,
        )
        .unwrap();
        let registry = Registry::load(dir.path()).unwrap();
        assert_eq!(
            registry.accounts(),
            [Account {
                name: Some("Ann".into()),
                ..account("a@x.com")
            }]
        );
        assert_eq!(
            registry
                .default_account()
                .map(|account| account.email.as_str()),
            Some("a@x.com")
        );
    }

    #[test]
    fn google_entries_load_and_save_as_they_always_have() {
        let dir = tempfile::tempdir().unwrap();
        let before = r#"[
  {
    "email": "a@x.com",
    "name": "Ann",
    "last_used": true,
    "joined": "2026-11-01T09:00:00Z"
  }
]"#;
        std::fs::write(dir.path().join(FILE_NAME), before).unwrap();
        let mut registry = Registry::load(dir.path()).unwrap();
        let account = &registry.accounts()[0];
        assert_eq!(account.provider, Provider::Google);
        assert_eq!(account.key(), "a@x.com");
        registry.set_color("a@x.com", None).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap(),
            before,
            "the entry is written back byte for byte"
        );
    }

    #[test]
    fn one_address_can_be_a_google_and_a_microsoft_account() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::load(dir.path()).unwrap();
        let microsoft = Account {
            provider: Provider::Microsoft,
            name: Some("Ada at work".into()),
            ..account("a@x.com")
        };
        registry.sign_in(account("a@x.com")).unwrap();
        registry.sign_in(microsoft.clone()).unwrap();
        let keys: Vec<String> = registry.accounts().iter().map(Account::key).collect();
        assert_eq!(keys, ["a@x.com", "microsoft a@x.com"]);
        registry
            .set_color("microsoft a@x.com", Some("sea".into()))
            .unwrap();
        assert!(registry.remove("a@x.com").unwrap());

        let reloaded = Registry::load(dir.path()).unwrap();
        let [left] = reloaded.accounts() else {
            panic!("{:?}", reloaded.accounts());
        };
        assert_eq!(left.provider, Provider::Microsoft);
        assert_eq!(left.color.as_deref(), Some("sea"));
        assert!(
            std::fs::read_to_string(dir.path().join(FILE_NAME))
                .unwrap()
                .contains(r#""provider": "microsoft""#)
        );
        assert_eq!(
            parse_key("microsoft a@x.com"),
            (Provider::Microsoft, "a@x.com")
        );
        assert_eq!(parse_key("a@x.com"), (Provider::Google, "a@x.com"));
        assert_eq!(
            Account::key_for(Provider::Google, "  Ada@Example.COM "),
            "ada@example.com"
        );
        assert_eq!(
            Account::key_for(Provider::Microsoft, "  Ada@Example.COM "),
            "microsoft ada@example.com"
        );
        assert_eq!(
            Account::parse_key("  MICROSOFT   Ada@Example.COM "),
            (Provider::Microsoft, "Ada@Example.COM")
        );
        assert_eq!(
            normalize_key("  MICROSOFT   Ada@Example.COM "),
            "microsoft ada@example.com"
        );
        assert_eq!(
            Account::parse_key("ü@example.com"),
            (Provider::Google, "ü@example.com")
        );
    }

    #[test]
    fn calendars_list_keeps_google_and_microsoft_same_email_distinct_across_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::load(dir.path()).unwrap();
        registry.sign_in(account("owner@x.com")).unwrap();
        registry.add_calendar("owner@x.com", "cal@x.com").unwrap();
        registry
            .add_calendar("owner@x.com", "microsoft cal@x.com")
            .unwrap();
        // Adding either again is a no-op.
        registry
            .add_calendar("owner@x.com", "microsoft cal@x.com")
            .unwrap();

        let reloaded = Registry::load(dir.path()).unwrap();
        assert_eq!(
            reloaded.accounts()[0].calendars,
            ["cal@x.com", "microsoft cal@x.com"]
        );

        // Mixed-case Microsoft entries in accounts.json normalize on load.
        std::fs::write(
            dir.path().join(FILE_NAME),
            r#"[{"email":"Owner@X.com","calendars":["Cal@X.com","  MICROSOFT  Cal@X.com "]}]"#,
        )
        .unwrap();
        let normalized = Registry::load(dir.path()).unwrap();
        assert_eq!(
            normalized.accounts()[0].calendars,
            ["cal@x.com", "microsoft cal@x.com"]
        );
    }

    #[test]
    fn a_corrupt_file_is_an_error_not_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), "[{").unwrap();
        assert!(matches!(
            Registry::load(dir.path()),
            Err(Error::Accounts { .. })
        ));
    }

    #[test]
    fn resigning_in_last_used_account_persists_state_and_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::load(dir.path()).unwrap();
        registry
            .sign_in(Account {
                name: Some("Ada".into()),
                ..account("a@x.com")
            })
            .unwrap();
        registry.sign_out("a@x.com").unwrap();
        assert!(registry.accounts()[0].last_used);
        assert!(registry.accounts()[0].signed_out);

        registry
            .sign_in(Account {
                name: Some("Ada Lovelace".into()),
                ..account("a@x.com")
            })
            .unwrap();

        let reloaded = Registry::load(dir.path()).unwrap();
        assert_eq!(reloaded.accounts().len(), 1);
        let a = &reloaded.accounts()[0];
        assert!(!a.signed_out);
        assert!(a.last_used);
        assert_eq!(a.name.as_deref(), Some("Ada Lovelace"));
    }

    #[test]
    fn sign_in_rolls_back_in_memory_state_when_save_fails() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::load(dir.path()).unwrap();
        registry
            .sign_in(Account {
                name: Some("Ada".into()),
                ..account("a@x.com")
            })
            .unwrap();
        registry.sign_out("a@x.com").unwrap();
        let before = registry.accounts().to_vec();

        // Block writes to the partial file so `save()` fails.
        std::fs::create_dir(dir.path().join("accounts.json.partial")).unwrap();
        let result = registry.sign_in(Account {
            name: Some("Ada Lovelace".into()),
            ..account("a@x.com")
        });
        assert!(matches!(result, Err(Error::Accounts { .. })));
        assert_eq!(registry.accounts(), before.as_slice());
    }

    #[test]
    fn all_mutators_roll_back_in_memory_state_when_save_fails() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::load(dir.path()).unwrap();
        registry.sign_in(account("a@x.com")).unwrap();
        registry.sign_in(account("b@x.com")).unwrap();
        registry.add_calendar("a@x.com", "cal@x.com").unwrap();
        let before = registry.accounts().to_vec();

        // Block writes to the partial file so every save fails.
        std::fs::create_dir(dir.path().join("accounts.json.partial")).unwrap();

        assert!(matches!(
            registry.use_account("a@x.com"),
            Err(Error::Accounts { .. })
        ));
        assert_eq!(registry.accounts(), before.as_slice());

        assert!(matches!(
            registry.sign_out("a@x.com"),
            Err(Error::Accounts { .. })
        ));
        assert_eq!(registry.accounts(), before.as_slice());

        assert!(matches!(
            registry.set_color("a@x.com", Some("sea".into())),
            Err(Error::Accounts { .. })
        ));
        assert_eq!(registry.accounts(), before.as_slice());

        assert!(matches!(
            registry.add_calendar("a@x.com", "other@x.com"),
            Err(Error::Accounts { .. })
        ));
        assert_eq!(registry.accounts(), before.as_slice());

        assert!(matches!(
            registry.remove_calendar("a@x.com", "cal@x.com"),
            Err(Error::Accounts { .. })
        ));
        assert_eq!(registry.accounts(), before.as_slice());

        assert!(matches!(
            registry.remove("a@x.com"),
            Err(Error::Accounts { .. })
        ));
        assert_eq!(registry.accounts(), before.as_slice());

        assert!(matches!(
            registry.move_to("b@x.com", 0),
            Err(Error::Accounts { .. })
        ));
        assert_eq!(registry.accounts(), before.as_slice());
    }
}
