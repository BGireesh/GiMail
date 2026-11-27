//! Signing in from the UI thread: exo-auth's installed-app flow, run on a runtime of its own,
//! its outcome picked up on a later frame, and the signed-in account's mail opened from it.

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use exo_auth::{Auth, Tokens};
use tokio::{runtime::Runtime, sync::oneshot, task::JoinHandle};
use url::Url;

use crate::account::{Account, Credentials, OpenError, PaceIntervals};
use crate::power::PowerSource;

/// Shows Google's consent page, in the system browser in the app and a scripted one in tests.
pub type Browser = Arc<dyn Fn(&Url) -> io::Result<()> + Send + Sync>;

/// Opens `url` in the system browser.
///
/// # Errors
/// When `open` can't run or fails.
pub fn system_browser(url: &Url) -> io::Result<()> {
    let status = std::process::Command::new("/usr/bin/open")
        .arg(url.as_str())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("open exited with {status}")))
    }
}

/// What one sign-in came to.
pub type Outcome = Result<exo_auth::Account, exo_auth::Error>;

/// The device's accounts: signing them in, and opening their mail.
pub struct SignIn {
    /// Opens the account list with its OAuth client. It's tried again on each sign-in until it
    /// works, so a client file saved while Exo runs is picked up.
    open: Box<dyn Fn() -> Result<Auth, exo_auth::Error>>,
    auth: Option<Auth>,
    /// Where accounts' mail stores live.
    stores: PathBuf,
    browser: Browser,
    runtime: Arc<Runtime>,
    attempt: Option<Attempt>,
    /// The power source the accounts it opens sync by off screen.
    power: PowerSource,
    /// Background sync cadence for accounts opened by this session.
    pace: PaceIntervals,
}

/// A sign-in in flight. Dropping it abandons it, closing its loopback listener.
struct Attempt {
    task: JoinHandle<()>,
    outcome: oneshot::Receiver<Outcome>,
}

impl Drop for Attempt {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl SignIn {
    /// Accounts opened by `open`, their mail stored in `stores`, consent shown by `browser`.
    ///
    /// # Errors
    /// When the sign-in runtime can't start.
    pub fn new(
        open: impl Fn() -> Result<Auth, exo_auth::Error> + 'static,
        stores: &Path,
        browser: Browser,
    ) -> io::Result<Self> {
        Ok(Self {
            open: Box::new(open),
            auth: None,
            stores: stores.to_owned(),
            browser,
            runtime: crate::account::new_runtime()?,
            attempt: None,
            power: PowerSource::default(),
            pace: PaceIntervals::default(),
        })
    }

    /// The Tokio runtime shared across sign-in and all accounts opened in this app session.
    pub(crate) fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }

    /// Has the accounts it opens from now on sync off screen by `power`, the Mac's
    /// ([`PowerSource::watch`]) in the app. Without this, they count as plugged in.
    pub fn set_power(&mut self, power: PowerSource) {
        self.power = power;
    }

    /// Overrides the background sync intervals (`focused`, `fast`, `medium`) for accounts opened
    /// from now on.
    pub fn set_sync_pace(&mut self, focused: Duration, fast: Duration, medium: Duration) {
        self.pace = PaceIntervals {
            focused,
            fast,
            medium,
        };
    }

    /// The account list, opened the first time that works.
    ///
    /// # Errors
    /// [`exo_auth::Error::MissingClient`] or [`exo_auth::Error::InvalidClient`] while there's no
    /// usable OAuth client, or [`exo_auth::Error::Accounts`].
    pub fn auth(&mut self) -> Result<&Auth, exo_auth::Error> {
        let auth = match self.auth.take() {
            Some(auth) => auth,
            None => (self.open)()?,
        };
        Ok(self.auth.insert(auth))
    }

    /// Starts signing a Google account in, abandoning the attempt in flight if there is one.
    /// `ctx` repaints when it's over. Google offers `hint` first (`login_hint`), when given.
    ///
    /// # Errors
    /// When the account list can't be opened or no Google OAuth client is configured.
    pub fn start(
        &mut self,
        ctx: &egui::Context,
        hint: Option<&str>,
    ) -> Result<(), exo_auth::Error> {
        self.start_provider(exo_auth::Provider::Google, ctx, hint)
    }

    /// Starts signing a Microsoft account in, abandoning the attempt in flight if there is one.
    /// `ctx` repaints when it's over. Microsoft offers `hint` first (`login_hint`), when given.
    ///
    /// # Errors
    /// When the account list can't be opened (see [`SignIn::auth`]).
    pub fn start_microsoft(
        &mut self,
        ctx: &egui::Context,
        hint: Option<&str>,
    ) -> Result<(), exo_auth::Error> {
        self.start_provider(exo_auth::Provider::Microsoft, ctx, hint)
    }

    /// Starts signing an account in with `provider`, abandoning any attempt in flight.
    ///
    /// # Errors
    /// When the account list can't be opened, or when `provider` is Google and no Google client is
    /// configured.
    pub fn start_provider(
        &mut self,
        provider: exo_auth::Provider,
        ctx: &egui::Context,
        hint: Option<&str>,
    ) -> Result<(), exo_auth::Error> {
        // Retry loading config when Google sign-in is attempted after opening without a Google
        // client file, so saving `google-oauth-client.json` while Exo runs is picked up at once.
        if provider == exo_auth::Provider::Google
            && self
                .auth
                .as_ref()
                .is_some_and(|auth| !auth.has_google_client())
        {
            self.auth = None;
        }
        let auth = self.auth()?.clone();
        if provider == exo_auth::Provider::Google {
            auth.check_google_client()?;
        }
        let (browser, ctx, hint) = (self.browser.clone(), ctx.clone(), hint.map(str::to_owned));
        let (sender, outcome) = oneshot::channel();
        let task = self.runtime.spawn(async move {
            let result = match provider {
                exo_auth::Provider::Google => {
                    auth.sign_in(hint.as_deref(), |url| browser(url)).await
                }
                exo_auth::Provider::Microsoft => {
                    auth.sign_in_microsoft(hint.as_deref(), |url| browser(url))
                        .await
                }
            };
            let _ = sender.send(result);
            ctx.request_repaint();
        });
        self.attempt = Some(Attempt { task, outcome });
        Ok(())
    }

    /// The outcome of the attempt in flight, once it has one.
    pub fn poll(&mut self) -> Option<Outcome> {
        let outcome = self.attempt.as_mut()?.outcome.try_recv().ok()?;
        self.attempt = None;
        Some(outcome)
    }

    /// Forgets `key`'s grant, its Keychain item and its place in the list, as signing out
    /// does.
    ///
    /// # Errors
    /// When the account list can't be opened or written, or the Keychain item deleted.
    pub(crate) fn forget(&mut self, key: &str) -> Result<(), exo_auth::Error> {
        self.auth()?.forget(key)
    }

    /// What `key`'s store holds that the provider doesn't have yet, read from the store whether or
    /// not its mail is open (a held send can outlive a quit that couldn't reach the server).
    ///
    /// # Errors
    /// When the account list can't be opened, or the store read.
    pub(crate) fn unsent(&mut self, key: &str) -> Result<exo_store::Unsent, MailError> {
        let provider = {
            self.auth()?;
            self.listed(key).provider
        };
        let auth = self.auth()?;
        // Counting makes no call to the provider, so the store's client is never used.
        let remote = match provider {
            exo_auth::Provider::Google => exo_store::Remote::gmail(
                auth.endpoints().clone(),
                gmail::StaticToken(String::new()),
            ),
            exo_auth::Provider::Microsoft => exo_store::Remote::outlook(
                auth.microsoft_endpoints().clone(),
                outlook::StaticToken(String::new()),
            ),
        };
        let stores = self.stores.clone();
        let counted = crate::account::block_on_runtime(&self.runtime, async move {
            exo_store::Store::open(&stores, key, remote)
                .await?
                .unsent()
                .await
        });
        Ok(counted.map_err(OpenError::from)?)
    }

    /// Whether `key` has a store, which it does when it has opened on this Mac since it last
    /// signed out.
    pub(crate) fn has_store(&self, key: &str) -> bool {
        exo_store::Store::exists(&self.stores, key)
    }

    /// Deletes `key`'s store, once its mail is closed.
    pub(crate) fn delete_store(&self, key: &str) {
        if let Err(error) = exo_store::Store::delete(&self.stores, key) {
            log::error!("deleting a signed-out account's store: {error}");
        }
    }

    /// Clears `key`'s cache, once its mail is closed. Its store is made again, keeping what
    /// the provider doesn't have yet ([`exo_store::Store::clear_cache`]), and fills when it
    /// next opens.
    ///
    /// # Errors
    /// When the old store can't be moved aside or the new one made.
    pub(crate) fn clear_cache(&self, key: &str) -> exo_store::Result<()> {
        crate::account::block_on_runtime(
            &self.runtime,
            exo_store::Store::clear_cache(&self.stores, key),
        )
    }

    /// Opens `key`'s mail, repainting `ctx` as it changes. Also returns its tokens, which say
    /// when the provider stops honoring the grant. Reads the Keychain the first time.
    ///
    /// # Errors
    /// When the account isn't listed, its grant can't be read, or its store can't be opened.
    pub fn open_mail(
        &mut self,
        key: &str,
        ctx: &egui::Context,
    ) -> Result<(Account, Tokens), MailError> {
        let tokens = self.auth()?.tokens(key)?;
        let account = self.open_mail_with_tokens(key, tokens.clone(), ctx)?;
        Ok((account, tokens))
    }

    /// [`SignIn::open_mail`] when `tokens` were already read from the Keychain (as launch does).
    pub(crate) fn open_mail_with_tokens(
        &mut self,
        key: &str,
        tokens: Tokens,
        ctx: &egui::Context,
    ) -> Result<Account, MailError> {
        self.auth()?;
        let account = self.listed(key);
        let auth = self.auth()?;
        let credentials = Credentials::signed_in(auth, &account, tokens);
        let calendars = crate::account::CalendarAuth {
            auth: auth.clone(),
            browser: self.browser.clone(),
        };
        let mut account = Account::open_on_with_pace(
            &self.stores,
            credentials,
            ctx.clone(),
            self.runtime.clone(),
            self.pace,
        )?;
        account.follow_power(&self.power);
        account.sign_in_calendars_with(calendars);
        Ok(account)
    }
}

impl SignIn {
    /// The listed account keyed `key`, falling back to parsing `key` into `(provider, email)`.
    fn listed(&self, key: &str) -> exo_auth::Account {
        self.auth
            .as_ref()
            .and_then(|auth| {
                auth.accounts()
                    .into_iter()
                    .find(|account| account.key() == key)
            })
            .unwrap_or_else(|| {
                let (provider, email) = exo_auth::Account::parse_key(key);
                exo_auth::Account {
                    email: email.to_owned(),
                    provider,
                    ..exo_auth::Account::default()
                }
            })
    }
}

/// Why an account's mail couldn't be opened.
#[derive(Debug, thiserror::Error)]
pub enum MailError {
    /// The account or its grant couldn't be read.
    #[error(transparent)]
    Auth(#[from] exo_auth::Error),
    /// Its store couldn't be opened.
    #[error(transparent)]
    Open(#[from] OpenError),
}
