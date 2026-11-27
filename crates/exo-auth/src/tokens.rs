//! Access tokens for one account, minted from its refresh token.

use std::sync::{Arc, Mutex, MutexGuard};

use gmail::oauth::OAuthClient;
use jiff::{SignedDuration, Timestamp};
use outlook::identity::IdentityClient;

use crate::config::MissingGoogle;
use crate::{Error, SecretStore};

/// How long before expiry a token is refreshed.
const MARGIN: SignedDuration = SignedDuration::from_mins(5);

/// One account's access tokens: a [`gmail::TokenProvider`] for a Google account's Gmail, Calendar
/// and People clients, and an [`outlook::TokenProvider`] for a Microsoft account's Graph client.
/// Cheap to clone, and clones share the cached token.
///
/// The token is cached until five minutes before it expires, then refreshed on the next request.
/// Concurrent requests wait for one refresh. A refresh that comes back with a new refresh token
/// (Microsoft's always do) keeps it, in memory and in the secret store. A 401 for the token
/// ([`invalidate`](gmail::TokenProvider::invalidate)) drops it, but a 401 for a token it already
/// replaced changes nothing. A refresh that finishes after the account was removed hands out
/// nothing.
/// When the provider says the grant was revoked or expired, every request fails with
/// [`Error::SignInAgain`] (inside the client's `Error::Token`) without asking it again, and
/// [`needs_sign_in`](Self::needs_sign_in) turns true, until the account signs in again.
#[derive(Clone)]
pub struct Tokens(Arc<Inner>);

struct Inner {
    /// The grant's key in the secret store: the account's, or a calendar grant's.
    key: String,
    refresher: Refresher,
    secrets: Arc<dyn SecretStore>,
    /// The account list's lock ([`crate::Auth`]'s), held while a rotated refresh token is saved
    /// so that a sign-out or a new sign-in can't come between the check and the write.
    ops: Arc<Mutex<()>>,
    state: Mutex<State>,
    /// Held while refreshing, so concurrent requests share one refresh.
    refreshing: tokio::sync::Mutex<()>,
}

/// Who mints the tokens.
#[derive(Clone)]
pub(crate) enum Refresher {
    Google(Result<OAuthClient, MissingGoogle>),
    Microsoft {
        identity: IdentityClient,
        scopes: &'static [&'static str],
    },
}

/// A refresh's answer.
struct Minted {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
}

/// Why a refresh failed.
enum Refused {
    /// The grant is no good any more: the account has to sign in again.
    Grant(String),
    /// Anything else (the network, the provider's outage), which a later request may get past.
    Other(Error),
}

impl Refresher {
    async fn refresh(&self, grant: &str) -> Result<Minted, Refused> {
        match self {
            Self::Google(Err(missing)) => Err(Refused::Other(missing.to_error())),
            Self::Google(Ok(oauth)) => match oauth.refresh(grant).await {
                Ok(response) => Ok(Minted {
                    access_token: response.access_token,
                    expires_in: response.expires_in,
                    refresh_token: response.refresh_token,
                }),
                Err(gmail::Error::OAuth(error)) if error.error == "invalid_grant" => {
                    Err(Refused::Grant(error.to_string()))
                }
                Err(error) => Err(Refused::Other(error.into())),
            },
            Self::Microsoft { identity, scopes } => match identity.refresh(grant, scopes).await {
                Ok(response) => Ok(Minted {
                    access_token: response.access_token,
                    expires_in: response.expires_in,
                    refresh_token: response.refresh_token,
                }),
                // Each says only a new sign-in gets a grant again
                // (learn.microsoft.com/entra/identity-platform/v2-oauth2-auth-code-flow).
                Err(outlook::Error::Identity(error))
                    if matches!(
                        error.error.as_str(),
                        "invalid_grant" | "interaction_required" | "consent_required"
                    ) =>
                {
                    Err(Refused::Grant(error.to_string()))
                }
                Err(error) => Err(Refused::Other(error.into())),
            },
        }
    }
}

struct State {
    /// The refresh token, or `None` once the provider rejected it (or when there never was one).
    grant: Option<String>,
    access: Option<Access>,
}

/// An access token and when to stop using it.
pub(crate) struct Access {
    token: String,
    refresh_at: Timestamp,
}

impl Access {
    /// A token issued at (or after) `issued` that lives `expires_in` seconds.
    pub(crate) fn new(token: String, issued: Timestamp, expires_in: u64) -> Self {
        let lifetime = SignedDuration::from_secs(i64::try_from(expires_in).unwrap_or(i64::MAX));
        let refresh_at = issued
            .checked_add(lifetime.saturating_sub(MARGIN))
            .unwrap_or(Timestamp::MAX);
        Self { token, refresh_at }
    }

    fn usable_at(&self, now: Timestamp) -> bool {
        now < self.refresh_at
    }
}

/// What a request does next.
enum Next {
    Use(String),
    Refresh(String),
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("key", &self.0.key)
            .finish_non_exhaustive()
    }
}

impl Tokens {
    /// Tokens for the grant kept under `key`, from `grant`, its refresh token. `None` means it
    /// has to sign in again.
    pub(crate) fn new(
        key: &str,
        refresher: Refresher,
        secrets: Arc<dyn SecretStore>,
        ops: Arc<Mutex<()>>,
        grant: Option<String>,
    ) -> Self {
        Self(Arc::new(Inner {
            key: key.to_owned(),
            refresher,
            secrets,
            ops,
            state: Mutex::new(State {
                grant,
                access: None,
            }),
            refreshing: tokio::sync::Mutex::new(()),
        }))
    }

    /// The provider rejected the refresh token, so the account has to sign in again.
    #[must_use]
    pub fn needs_sign_in(&self) -> bool {
        self.state().grant.is_none()
    }

    /// The account signed in again (or just signed in), so use its new grant from now on.
    pub(crate) fn replace(&self, grant: String, access: Access) {
        let mut state = self.state();
        state.grant = Some(grant);
        state.access = Some(access);
    }

    /// The account was removed, so stop handing out tokens.
    pub(crate) fn revoke(&self) {
        let mut state = self.state();
        state.grant = None;
        state.access = None;
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().expect("token state is never poisoned")
    }

    fn sign_in_again(&self) -> Error {
        Error::SignInAgain {
            account: self.0.key.clone(),
        }
    }

    fn next(&self) -> Result<Next, Error> {
        let state = self.state();
        let Some(grant) = &state.grant else {
            return Err(self.sign_in_again());
        };
        Ok(match &state.access {
            Some(access) if access.usable_at(Timestamp::now()) => Next::Use(access.token.clone()),
            _ => Next::Refresh(grant.clone()),
        })
    }

    async fn access(&self) -> Result<String, Error> {
        if let Next::Use(token) = self.next()? {
            return Ok(token);
        }
        let _refreshing = self.0.refreshing.lock().await;
        loop {
            // Whoever held the lock may have refreshed already.
            let grant = match self.next()? {
                Next::Use(token) => return Ok(token),
                Next::Refresh(grant) => grant,
            };
            let issued = Timestamp::now();
            let result = self.0.refresher.refresh(&grant).await;
            match result {
                Ok(mut minted) => {
                    let access =
                        Access::new(minted.access_token.clone(), issued, minted.expires_in);
                    if let Some(new_grant) = minted.refresh_token.take().filter(|new| *new != grant)
                    {
                        // Persist rotated refresh tokens on the blocking pool under `ops` without
                        // holding `state` across Keychain I/O, so `needs_sign_in` never stalls and
                        // tokio workers never block on the Keychain.
                        let inner = self.0.clone();
                        let expected = grant.clone();
                        let applied = tokio::task::spawn_blocking(move || {
                            let _op = inner
                                .ops
                                .lock()
                                .expect("account ops lock is never poisoned");
                            let still_current = inner
                                .state
                                .lock()
                                .expect("token state is never poisoned")
                                .grant
                                .as_deref()
                                == Some(expected.as_str());
                            if !still_current {
                                return false;
                            }
                            // Using a refresh token doesn't revoke it, so the one a failed write
                            // leaves in the store still works for a while.
                            if let Err(error) = inner.secrets.set(&inner.key, &new_grant) {
                                log::error!("saving an account's new refresh token: {error}");
                            }
                            let mut state =
                                inner.state.lock().expect("token state is never poisoned");
                            if state.grant.as_deref() != Some(expected.as_str()) {
                                return false;
                            }
                            state.grant = Some(new_grant);
                            state.access = Some(access);
                            true
                        })
                        .await
                        .map_err(|error| {
                            Error::Secrets(format!("saving refresh token: {error}").into())
                        })?;
                        if !applied {
                            continue;
                        }
                        return Ok(minted.access_token);
                    }
                    let mut state = self.state();
                    // The account was removed or signed in again meanwhile, so the answer is about
                    // a grant it no longer has. Start over from what it has now.
                    if state.grant.as_deref() != Some(grant.as_str()) {
                        continue;
                    }
                    state.access = Some(access);
                    return Ok(minted.access_token);
                }
                Err(Refused::Grant(error)) => {
                    let mut state = self.state();
                    if state.grant.as_deref() != Some(grant.as_str()) {
                        continue;
                    }
                    log::warn!("the provider rejected an account's refresh token: {error}");
                    state.grant = None;
                    state.access = None;
                    return Err(self.sign_in_again());
                }
                Err(Refused::Other(error)) => {
                    if self.state().grant.as_deref() != Some(grant.as_str()) {
                        continue;
                    }
                    return Err(error);
                }
            }
        }
    }

    fn invalidate_access(&self, token: &str) {
        let mut state = self.state();
        if state
            .access
            .as_ref()
            .is_some_and(|access| access.token == token)
        {
            state.access = None;
        }
    }
}

impl gmail::TokenProvider for Tokens {
    fn access_token(&self) -> gmail::TokenFuture<'_> {
        Box::pin(async move {
            self.access().await.map_err(|error| match error {
                Error::Google(error) => error,
                other => gmail::Error::Token(Box::new(other)),
            })
        })
    }

    fn invalidate(&self, token: &str) {
        self.invalidate_access(token);
    }
}

impl outlook::TokenProvider for Tokens {
    fn access_token(&self) -> outlook::TokenFuture<'_> {
        Box::pin(async move {
            self.access().await.map_err(|error| match error {
                Error::Microsoft(error) => error,
                other => outlook::Error::Token(Box::new(other)),
            })
        })
    }

    fn invalidate(&self, token: &str) {
        self.invalidate_access(token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_refreshed_five_minutes_before_they_expire() {
        let issued: Timestamp = "2026-11-17T14:00:00Z".parse().unwrap();
        let access = Access::new("ya29.x".into(), issued, 3599);
        let at = |offset: i64| {
            issued
                .checked_add(SignedDuration::from_secs(offset))
                .unwrap()
        };
        assert!(access.usable_at(issued));
        assert!(access.usable_at(at(3599 - 301)));
        assert!(!access.usable_at(at(3599 - 300)));
        assert!(!access.usable_at(at(4000)));
        // A token shorter-lived than the margin is never reused.
        assert!(!Access::new("ya29.y".into(), issued, 60).usable_at(issued));
    }
}
