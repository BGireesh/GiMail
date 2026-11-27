//! Microsoft sign-in: Exo's client and the file that overrides it, and the authorization code
//! flow's exchange and checks.

use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use outlook::identity::{
    CONSENT_ERRORS, IdTokenClaims, IdentityClient, IdentityError, Pkce, RedirectError,
};
use outlook::{Endpoints, StaticToken};
use serde::Deserialize;
use url::Url;

use crate::accounts::Provider;
use crate::sign_in::{Grant, Loopback, Purpose, random_state};
use crate::tokens::Access;
use crate::{Account, Error, loopback};

/// Exo's own registration with Microsoft: a public client, so it has no secret, and its ID
/// travels in every sign-in address.
pub const EXO_CLIENT_ID: &str = "7d0f2357-7b6e-4cf2-97b6-1d3f61b9888b";

/// The file in Exo's config folder that names another registration, for an organization that
/// approves its own.
pub(crate) const FILE_NAME: &str = "microsoft-oauth-client.json";

/// Who Exo is to Microsoft, and where Microsoft is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrosoftClient {
    client_id: String,
    endpoints: Endpoints,
}

/// `microsoft-oauth-client.json`: the client ID alone.
#[derive(Deserialize)]
struct File {
    client_id: String,
}

impl Default for MicrosoftClient {
    /// Exo's own registration, at Microsoft.
    fn default() -> Self {
        Self::new(EXO_CLIENT_ID, Endpoints::microsoft())
    }
}

impl MicrosoftClient {
    /// The registration `client_id` at `endpoints`, e.g. the mock servers'.
    pub fn new(client_id: impl Into<String>, endpoints: Endpoints) -> Self {
        Self {
            client_id: client_id.into(),
            endpoints,
        }
    }

    /// The client `microsoft-oauth-client.json` in `config_dir` names (`{ "client_id": "…" }`),
    /// or else Exo's own. It talks to Microsoft's production endpoints.
    ///
    /// # Errors
    /// [`Error::InvalidClient`] when the file is there but can't be read or names no client ID.
    pub fn load(config_dir: &Path) -> Result<Self, Error> {
        let path = config_dir.join(FILE_NAME);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => {
                return Err(Error::InvalidClient {
                    provider: Provider::Microsoft,
                    path,
                    reason: format!("can't be read: {error}"),
                });
            }
        };
        Self::parse(&text, path)
    }

    fn parse(text: &str, path: PathBuf) -> Result<Self, Error> {
        let invalid = |reason: String| Error::InvalidClient {
            provider: Provider::Microsoft,
            path: path.clone(),
            reason,
        };
        let file: File = serde_json::from_str(text)
            .map_err(|error| invalid(format!("isn't {{ \"client_id\": \"…\" }}: {error}")))?;
        let client_id = file.client_id.trim();
        if client_id.is_empty() {
            return Err(invalid("has an empty client_id".into()));
        }
        Ok(Self::new(client_id, Endpoints::microsoft()))
    }

    /// The client ID, which the consent screen names the app by.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Graph and sign-in, for Microsoft accounts' clients.
    #[must_use]
    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    pub(crate) fn identity(&self) -> IdentityClient {
        IdentityClient::new(self.endpoints.identity.clone(), &self.client_id)
    }
}

/// Runs Microsoft's flow for `purpose`: opens the browser at the sign-in page, waits for the
/// redirect, exchanges the code, hands the grant to `commit`, and tells the browser how it went.
pub(crate) async fn authorize<T>(
    client: &MicrosoftClient,
    purpose: Purpose,
    login_hint: Option<&str>,
    open: impl FnOnce(&Url) -> io::Result<()>,
    commit: impl FnOnce(Grant) -> Result<T, Error>,
) -> Result<T, Error> {
    // Microsoft matches the registration's `http://localhost` whatever the port.
    let loopback = Loopback::bind("localhost").await?;
    let identity = client.identity();
    let (pkce, nonce) = (Pkce::generate(), random_state());
    let mut url = identity.authorization_url(
        loopback.redirect_uri(),
        purpose.scopes(Provider::Microsoft),
        loopback.state(),
        &nonce,
        &pkce,
    );
    if let Some(hint) = login_hint {
        url.query_pairs_mut().append_pair("login_hint", hint);
    }
    open(&url).map_err(Error::Browser)?;
    let (redirect, stream) = loopback.redirect().await?;
    let sign_in = SignIn {
        identity: &identity,
        endpoints: client.endpoints(),
        purpose,
        pkce: &pkce,
        nonce: &nonce,
        redirect_uri: loopback.redirect_uri(),
    };
    let result = match sign_in.exchange(&redirect).await {
        Ok(grant) => commit(grant),
        Err(error) => Err(error),
    };
    loopback::respond(stream, Provider::Microsoft, &result).await;
    result
}

/// What the code exchange checks the redirect against.
struct SignIn<'a> {
    identity: &'a IdentityClient,
    endpoints: &'a Endpoints,
    purpose: Purpose,
    pkce: &'a Pkce,
    nonce: &'a str,
    redirect_uri: &'a str,
}

impl SignIn<'_> {
    /// Exchanges the code in the redirect, whose `state` is this attempt's, and checks the
    /// account it signed in: its ID token is this sign-in's, it granted every Graph permission
    /// `purpose` asked for, and it has a mailbox (for mail) or calendar access (for calendar).
    async fn exchange(&self, redirect: &Url) -> Result<Grant, Error> {
        if let Some(error) = RedirectError::from_redirect(redirect) {
            return Err(match error.code() {
                Some(code) if CONSENT_ERRORS.contains(&code) => Error::ConsentRequired { code },
                _ if error.error == "access_denied" => Error::AccessDenied,
                _ => Error::Authorization { error: error.error },
            });
        }
        let Some(code) = loopback::param(redirect, "code") else {
            return Err(Error::Authorization {
                error: "no authorization code".into(),
            });
        };

        let scopes = self.purpose.scopes(Provider::Microsoft);
        let issued = Timestamp::now();
        let tokens = self
            .identity
            .exchange_code(&code, &self.pkce.verifier, self.redirect_uri, scopes)
            .await
            .map_err(|error| match error {
                outlook::Error::Identity(error) => consent(*error),
                error => error.into(),
            })?;
        let missing = tokens.missing_scopes(scopes);
        if !missing.is_empty() {
            return Err(Error::MissingScopes(missing));
        }
        let refresh_token = tokens.refresh_token.ok_or(Error::NoRefreshToken)?;
        let claims = tokens
            .id_token
            .as_deref()
            .map(IdTokenClaims::decode)
            .transpose()?
            .unwrap_or_default();
        if claims.aud.as_deref() != Some(self.identity.client_id())
            || claims.nonce.as_deref() != Some(self.nonce)
        {
            return Err(Error::Authorization {
                error: "the ID token isn't this sign-in's".into(),
            });
        }

        let graph = outlook::Client::new(
            self.endpoints.clone(),
            StaticToken(tokens.access_token.clone()),
        );
        let people = graph.people();
        let access_check = async {
            let check = match self.purpose {
                Purpose::Mail => graph
                    .mail()
                    .folder(outlook::well_known::INBOX)
                    .select("id")
                    .into_future()
                    .await
                    .map(drop),
                Purpose::Calendar => graph
                    .calendar()
                    .default_calendar()
                    .select("id")
                    .into_future()
                    .await
                    .map(drop),
            };
            // An account without an Exchange Online mailbox (Microsoft's error for one hosted
            // on-premises, inactive or deleted).
            check.map_err(|error| match error.code() {
                Some("MailboxNotEnabledForRESTAPI") => Error::NoMailbox,
                _ => error.into(),
            })
        };
        let (me, access_check) = tokio::join!(
            people
                .me()
                .select("displayName,mail,userPrincipalName")
                .into_future(),
            access_check,
        );
        access_check?;
        let me = me?;
        let (email, name) = resolve_identity(&me, &claims)?;
        Ok(Grant {
            account: Account {
                email,
                provider: Provider::Microsoft,
                name,
                ..Account::default()
            },
            refresh_token,
            access: Access::new(tokens.access_token, issued, tokens.expires_in),
        })
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// Resolves the account's trimmed non-empty address and display name from Graph `/me` and ID
/// token claims, skipping empty or whitespace-only fields before falling back.
fn resolve_identity(
    me: &outlook::people::User,
    claims: &IdTokenClaims,
) -> Result<(String, Option<String>), Error> {
    let email = non_empty(me.address())
        .or_else(|| non_empty(me.mail.as_deref()))
        .or_else(|| non_empty(me.user_principal_name.as_deref()))
        .or_else(|| non_empty(claims.preferred_username.as_deref()))
        .ok_or_else(|| Error::MissingScopes(vec![outlook::identity::scopes::USER_READ.into()]))?
        .to_owned();
    let name = non_empty(me.display_name.as_deref())
        .or_else(|| non_empty(claims.name.as_deref()))
        .map(str::to_owned);
    Ok((email, name))
}

/// The error a refused code exchange is: [`Error::ConsentRequired`] for the consent codes.
fn consent(error: IdentityError) -> Error {
    match error.code() {
        Some(code) if CONSENT_ERRORS.contains(&code) => Error::ConsentRequired { code },
        _ => Error::Microsoft(outlook::Error::Identity(Box::new(error))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exo_signs_in_as_itself_unless_a_file_says_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let builtin = MicrosoftClient::load(dir.path()).unwrap();
        assert_eq!(builtin.client_id(), EXO_CLIENT_ID);
        assert_eq!(builtin.endpoints(), &Endpoints::microsoft());

        let file = dir.path().join(FILE_NAME);
        std::fs::write(
            &file,
            r#"{ "client_id": " 00001111-aaaa-2222-bbbb-3333cccc4444 " }"#,
        )
        .unwrap();
        let theirs = MicrosoftClient::load(dir.path()).unwrap();
        assert_eq!(theirs.client_id(), "00001111-aaaa-2222-bbbb-3333cccc4444");
        assert_eq!(theirs.endpoints(), &Endpoints::microsoft());

        for broken in [r#"{ "client_id": "" }"#, "{}", "not json"] {
            std::fs::write(&file, broken).unwrap();
            let error = MicrosoftClient::load(dir.path()).unwrap_err();
            assert!(
                matches!(
                    &error,
                    Error::InvalidClient {
                        provider: Provider::Microsoft,
                        path,
                        ..
                    } if *path == file
                ),
                "{broken}: {error}"
            );
        }
    }

    #[test]
    fn resolve_identity_skips_empty_or_whitespace_fields_before_falling_back() {
        let user = outlook::people::User {
            display_name: Some("   ".into()),
            mail: Some("  ".into()),
            user_principal_name: Some(String::new()),
            ..outlook::people::User::default()
        };
        let claims = IdTokenClaims {
            preferred_username: Some("  ada@contoso.com ".into()),
            name: Some(" Ada Lovelace ".into()),
            ..IdTokenClaims::default()
        };
        let (email, name) = resolve_identity(&user, &claims).unwrap();
        assert_eq!(email, "ada@contoso.com");
        assert_eq!(name.as_deref(), Some("Ada Lovelace"));

        let user_with_upn = outlook::people::User {
            user_principal_name: Some("  upn@contoso.com ".into()),
            ..user.clone()
        };
        assert_eq!(
            resolve_identity(&user_with_upn, &claims).unwrap().0,
            "upn@contoso.com"
        );

        let empty_claims = IdTokenClaims {
            preferred_username: Some("   ".into()),
            ..IdTokenClaims::default()
        };
        assert!(matches!(
            resolve_identity(&user, &empty_claims),
            Err(Error::MissingScopes(scopes)) if scopes == [outlook::identity::scopes::USER_READ]
        ));
    }
}
