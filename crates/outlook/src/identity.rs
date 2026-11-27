//! The Microsoft identity platform's authorization code flow for a public client, a desktop app
//! with no secret (learn.microsoft.com/entra/identity-platform/v2-oauth2-auth-code-flow): the
//! sign-in URL with PKCE, the code exchange, refresh, the ID token's claims, and the `AADSTS`
//! errors sign-in fails with.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use sha2::{Digest, Sha256};
use url::Url;

use crate::endpoints::IdentityEndpoints;
use crate::error::Error;

/// The tenant personal Microsoft accounts (Outlook.com) sign in to, as an ID token's `tid`
/// says (learn.microsoft.com/entra/identity-platform/id-token-claims-reference).
pub const CONSUMER_TENANT: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

/// How long a sign-in request may take, so one that hangs on a dead pooled connection doesn't
/// hold up every request waiting on its refresh.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Scopes: Graph's delegated permissions (learn.microsoft.com/graph/permissions-reference) and
/// OpenID Connect's.
pub mod scopes {
    /// A refresh token.
    pub const OFFLINE_ACCESS: &str = "offline_access";
    /// An ID token.
    pub const OPENID: &str = "openid";
    /// The ID token's `name`, `oid` and `preferred_username`.
    pub const PROFILE: &str = "profile";
    /// The ID token's `email`.
    pub const EMAIL: &str = "email";
    /// The signed-in user's profile.
    pub const USER_READ: &str = "User.Read";
    /// The organization's directory.
    pub const USER_READ_BASIC_ALL: &str = "User.ReadBasic.All";
    /// Read and change mail.
    pub const MAIL_READ_WRITE: &str = "Mail.ReadWrite";
    /// Send mail.
    pub const MAIL_SEND: &str = "Mail.Send";
    /// Mailbox settings: automatic replies, the time zone.
    pub const MAILBOX_SETTINGS_READ_WRITE: &str = "MailboxSettings.ReadWrite";
    /// Read and change calendars.
    pub const CALENDARS_READ_WRITE: &str = "Calendars.ReadWrite";
    /// Read and change contacts.
    pub const CONTACTS_READ_WRITE: &str = "Contacts.ReadWrite";
    /// The people the user works with most.
    pub const PEOPLE_READ: &str = "People.Read";

    /// OpenID Connect's scopes, which a token response's `scope` needn't list.
    pub(crate) const OPENID_CONNECT: &[&str] = &[OFFLINE_ACCESS, OPENID, PROFILE, EMAIL];
}

/// The `AADSTS` codes that mean Exo lacks consent the user can't give: the organization needs
/// an admin to approve the app (learn.microsoft.com/entra/identity-platform/
/// reference-error-codes). 65004 is also a user declining on the consent screen.
pub const CONSENT_ERRORS: &[u32] = &[65001, 65004, 90093, 90094];

/// A PKCE verifier and its S256 challenge (RFC 7636).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    /// Kept secret until the code exchange.
    pub verifier: String,
    /// Sent in the sign-in URL.
    pub challenge: String,
}

impl Pkce {
    /// A fresh random verifier (43 characters, the RFC's minimum entropy).
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("the OS random source is available");
        Self::from_verifier(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// The pair for a known verifier.
    #[must_use]
    pub fn from_verifier(verifier: String) -> Self {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

/// A public client's registration and the endpoints to use it against.
#[derive(Debug, Clone)]
pub struct IdentityClient {
    http: reqwest::Client,
    endpoints: IdentityEndpoints,
    client_id: String,
}

/// A successful token response.
#[skip_serializing_none]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenResponse {
    /// Always `Bearer`.
    pub token_type: String,
    /// The scopes granted, space-separated; Graph's may carry their resource
    /// (`https://graph.microsoft.com/Mail.ReadWrite`).
    pub scope: Option<String>,
    /// Lifetime of `access_token` in seconds.
    pub expires_in: u64,
    /// Bearer token for Graph.
    pub access_token: String,
    /// The refresh token. Each refresh returns a new one, which replaces the old.
    pub refresh_token: Option<String>,
    /// The ID token, with `openid`.
    pub id_token: Option<String>,
}

/// Leaves the tokens out, so logging a response never prints them.
impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("token_type", &self.token_type)
            .field("scope", &self.scope)
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

impl TokenResponse {
    /// Those of Graph's `scopes` the response didn't grant. Scopes compare without their
    /// resource and case, and OpenID Connect's aren't checked. Without `scope`, everything asked
    /// was granted (RFC 6749, section 5.1).
    #[must_use]
    pub fn missing_scopes(&self, scopes: &[&str]) -> Vec<String> {
        let Some(granted) = &self.scope else {
            return Vec::new();
        };
        let granted: Vec<&str> = granted
            .split_whitespace()
            .map(|scope| scope.rsplit('/').next().unwrap_or(scope))
            .collect();
        scopes
            .iter()
            .filter(|scope| !scopes::OPENID_CONNECT.contains(scope))
            .filter(|scope| !granted.iter().any(|g| g.eq_ignore_ascii_case(scope)))
            .map(|scope| (*scope).to_owned())
            .collect()
    }
}

/// The ID token's claims Exo reads (learn.microsoft.com/entra/identity-platform/
/// id-token-claims-reference).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdTokenClaims {
    /// The client ID it was issued to.
    pub aud: Option<String>,
    /// Who issued it: `https://login.microsoftonline.com/{tid}/v2.0`.
    pub iss: Option<String>,
    /// The user, as this app sees them.
    pub sub: Option<String>,
    /// The tenant the user signed in to: [`CONSUMER_TENANT`] for a personal account.
    pub tid: Option<String>,
    /// The user's object ID, with `profile`. For work accounts, Graph's user `id`.
    pub oid: Option<String>,
    /// The user's name, with `profile`.
    pub name: Option<String>,
    /// The name the user signed in with, with `profile`; usually their address.
    pub preferred_username: Option<String>,
    /// The user's address, with `email`. Microsoft doesn't vouch for it.
    pub email: Option<String>,
    /// The `nonce` the sign-in URL carried.
    pub nonce: Option<String>,
}

impl IdTokenClaims {
    /// The claims of `id_token`, a JWT. The signature isn't checked: the token comes straight
    /// from the token endpoint over TLS, which OpenID Connect accepts in place of a signature
    /// check (Core 1.0, section 3.1.3.7).
    ///
    /// # Errors
    /// [`Error::Decode`] when it isn't a JWT.
    pub fn decode(id_token: &str) -> Result<Self, Error> {
        let failed = |reason: String| Error::Decode {
            status: 200,
            reason: format!("ID token: {reason}"),
        };
        let mut parts = id_token.split('.');
        let (Some(_header), Some(payload), Some(_signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(failed("not a JWT".into()));
        };
        let json =
            crate::wire::decode_base64(payload).map_err(|error| failed(error.to_string()))?;
        serde_json::from_slice(&json).map_err(|error| failed(error.to_string()))
    }

    /// Whether a personal Microsoft account signed in.
    #[must_use]
    pub fn is_personal(&self) -> bool {
        self.tid.as_deref() == Some(CONSUMER_TENANT)
    }
}

/// The token endpoint's error body (learn.microsoft.com/entra/identity-platform/
/// reference-error-codes). React to `error` and the `AADSTS` code, never the description.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{error}: {}", summary(error_description.as_deref()))]
pub struct IdentityError {
    /// The error's class, e.g. `invalid_grant`, `interaction_required`, `invalid_client`.
    pub error: String,
    /// `AADSTS…: what happened`, then trace lines.
    pub error_description: Option<String>,
    /// The `AADSTS` codes, as numbers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub error_codes: Vec<u32>,
    /// When it happened.
    pub timestamp: Option<String>,
    /// For Microsoft's support.
    pub trace_id: Option<String>,
    /// For Microsoft's support.
    pub correlation_id: Option<String>,
    /// Microsoft's page about the error, for developers.
    pub error_uri: Option<String>,
    /// A finer class, e.g. `bad_token`.
    pub suberror: Option<String>,
}

impl IdentityError {
    /// The `AADSTS` code.
    #[must_use]
    pub fn code(&self) -> Option<u32> {
        self.error_codes
            .first()
            .copied()
            .or_else(|| aadsts(self.error_description.as_deref()?))
    }
}

/// The error a sign-in redirect came back with (`?error=…&error_description=AADSTS…`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectError {
    /// The error's class, e.g. `access_denied`, `consent_required`, `invalid_request`.
    pub error: String,
    /// `AADSTS…: what happened`, then trace lines.
    pub description: Option<String>,
    /// `cancel` when the user turned the request down.
    pub subcode: Option<String>,
}

impl RedirectError {
    /// The error `redirect` carries, if it carries one.
    #[must_use]
    pub fn from_redirect(redirect: &Url) -> Option<Self> {
        let param = |name: &str| {
            redirect
                .query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        Some(Self {
            error: param("error")?,
            description: param("error_description"),
            subcode: param("error_subcode"),
        })
    }

    /// The `AADSTS` code, from the description: the redirect carries no other.
    #[must_use]
    pub fn code(&self) -> Option<u32> {
        aadsts(self.description.as_deref()?)
    }
}

/// The code an `AADSTS…:` description starts with.
fn aadsts(description: &str) -> Option<u32> {
    let rest = description.trim_start().strip_prefix("AADSTS")?;
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..digits].parse().ok()
}

/// A description's first line, without the trace lines after it.
fn summary(description: Option<&str>) -> &str {
    description
        .and_then(|description| description.lines().next())
        .unwrap_or("no description")
}

impl IdentityClient {
    /// The registration `client_id` (a public client: no secret), signing in at `endpoints`.
    pub fn new(endpoints: IdentityEndpoints, client_id: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .expect("the TLS backend initializes, as reqwest::Client::new assumes"),
            endpoints,
            client_id: client_id.into(),
        }
    }

    /// The registration's client ID.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Where to send the user to sign in and grant `scopes`, coming back to `redirect_uri` (a
    /// loopback address: Microsoft ignores a localhost redirect's port). The user picks the
    /// account each time (`prompt=select_account`). Add `login_hint` to suggest one.
    #[must_use]
    pub fn authorization_url(
        &self,
        redirect_uri: &str,
        scopes: &[&str],
        state: &str,
        nonce: &str,
        pkce: &Pkce,
    ) -> Url {
        let mut url = self.endpoints.authorization.clone();
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_mode", "query")
            .append_pair("scope", &scopes.join(" "))
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("prompt", "select_account");
        url
    }

    /// Exchanges an authorization code for tokens.
    ///
    /// # Errors
    /// [`Error::Identity`] when Microsoft refuses the grant, [`Error::Http`] on transport
    /// failure.
    pub async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
        scopes: &[&str],
    ) -> Result<TokenResponse, Error> {
        self.token(
            scopes,
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect_uri),
            ],
        )
        .await
    }

    /// Mints new tokens from a refresh token. The response's refresh token replaces the old
    /// one, which the caller should discard.
    ///
    /// # Errors
    /// [`Error::Identity`] (`invalid_grant` when the grant was revoked or expired), and
    /// [`Error::Http`].
    pub async fn refresh(
        &self,
        refresh_token: &str,
        scopes: &[&str],
    ) -> Result<TokenResponse, Error> {
        self.token(
            scopes,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ],
        )
        .await
    }

    async fn token(&self, scopes: &[&str], grant: &[(&str, &str)]) -> Result<TokenResponse, Error> {
        let scope = scopes.join(" ");
        let mut form = vec![
            ("client_id", self.client_id.as_str()),
            ("scope", scope.as_str()),
        ];
        form.extend_from_slice(grant);
        let response = self
            .http
            .post(self.endpoints.token.clone())
            .form(&form)
            .send()
            .await?;
        let status = response.status().as_u16();
        let body = response.bytes().await?;
        if (200..300).contains(&status) {
            // Unlike other decoding errors, this one doesn't quote the body, which holds the
            // tokens.
            return crate::client::parse(&body).map_err(|error| Error::Decode {
                status,
                reason: error.to_string(),
            });
        }
        match serde_json::from_slice::<IdentityError>(&body) {
            Ok(error) => Err(Error::Identity(Box::new(error))),
            Err(_) => Err(Error::Decode {
                status,
                reason: format!(
                    "not a token error body; body starts with {:?}",
                    String::from_utf8_lossy(&body[..body.len().min(200)])
                ),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_matches_rfc_7636_appendix_b() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(
            pkce.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(Pkce::generate().verifier.len(), 43);
    }

    #[test]
    fn reads_aadsts_codes() {
        let error: IdentityError = serde_json::from_str(
            r#"{"error":"invalid_grant","error_description":"AADSTS70008: The refresh token has expired.\r\nTrace ID: t\r\nCorrelation ID: c","error_codes":[70008],"timestamp":"2026-11-17 14:00:00Z","trace_id":"t","correlation_id":"c","error_uri":"https://login.microsoftonline.com/error?code=70008"}"#,
        )
        .unwrap();
        assert_eq!(error.code(), Some(70008));
        assert_eq!(
            error.to_string(),
            "invalid_grant: AADSTS70008: The refresh token has expired."
        );
        let redirect = Url::parse(
            "http://localhost:5000/?error=access_denied&error_description=AADSTS65004%3A+User+declined+to+consent+to+access+the+app.%0D%0ATrace+ID%3A+x&error_subcode=cancel&state=s",
        )
        .unwrap();
        let error = RedirectError::from_redirect(&redirect).unwrap();
        assert_eq!(error.error, "access_denied");
        assert_eq!(error.code(), Some(65004));
        assert_eq!(error.subcode.as_deref(), Some("cancel"));
        assert!(CONSENT_ERRORS.contains(&65004));
        assert_eq!(
            RedirectError::from_redirect(&Url::parse("http://localhost/?code=c").unwrap()),
            None
        );
        assert_eq!(aadsts("Something else"), None);
    }

    #[test]
    fn graph_scopes_compare_without_their_resource() {
        let response = TokenResponse {
            token_type: "Bearer".into(),
            scope: Some("https://graph.microsoft.com/Mail.ReadWrite User.Read openid".into()),
            expires_in: 3599,
            access_token: "eyJ.secret".into(),
            refresh_token: Some("M.C.secret".into()),
            id_token: None,
        };
        assert_eq!(
            response.missing_scopes(&[
                "offline_access",
                "mail.readwrite",
                "User.Read",
                "Mail.Send"
            ]),
            ["Mail.Send"]
        );
        let debug = format!("{response:?}");
        assert!(!debug.contains("secret"), "{debug}");
    }

    #[test]
    fn id_token_claims_tell_personal_accounts() {
        let payload = URL_SAFE_NO_PAD.encode(format!(
            r#"{{"aud":"c","tid":"{CONSUMER_TENANT}","name":"Ada","preferred_username":"ada@outlook.com","nonce":"n"}}"#
        ));
        let claims = IdTokenClaims::decode(&format!("eyJhbGciOiJub25lIn0.{payload}.sig")).unwrap();
        assert!(claims.is_personal());
        assert_eq!(claims.name.as_deref(), Some("Ada"));
        assert!(IdTokenClaims::decode("garbage").is_err());
        assert!(IdTokenClaims::decode(&format!("eyJhbGciOiJub25lIn0.{payload}")).is_err());
        assert!(
            IdTokenClaims::decode(&format!("eyJhbGciOiJub25lIn0.{payload}.sig.extra")).is_err()
        );
    }
}
