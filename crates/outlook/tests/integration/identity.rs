//! The authorization code flow against mock-servers' Microsoft sign-in.

use crate::support;

use mock_servers::{CONSUMER_TENANT, TENANT};
use outlook::identity::{
    CONSENT_ERRORS, IdTokenClaims, IdentityClient, Pkce, RedirectError, TokenResponse, scopes,
};
use outlook::{Client, Endpoints, StaticToken};
use serde_json::json;
use support::{EMAIL, Mock};
use url::Url;

const CLIENT: &str = "7d0f2357-0000-4000-8000-000000000000";
const REDIRECT: &str = "http://localhost:53682";
const SCOPES: &[&str] = &[
    scopes::OFFLINE_ACCESS,
    scopes::OPENID,
    scopes::PROFILE,
    scopes::EMAIL,
    scopes::USER_READ,
    scopes::MAIL_READ_WRITE,
];

fn identity(mock: &Mock) -> IdentityClient {
    IdentityClient::new(Endpoints::mock(&mock.server.url()).identity, CLIENT)
}

/// Opens the sign-in URL as a browser would, returning where Microsoft redirects it.
async fn authorize(mock: &Mock, pkce: &Pkce, hint: Option<&str>) -> Url {
    let mut url = identity(mock).authorization_url(REDIRECT, SCOPES, "s-1", "n-1", pkce);
    if let Some(hint) = hint {
        url.query_pairs_mut().append_pair("login_hint", hint);
    }
    let param = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    assert_eq!(param("prompt").as_deref(), Some("select_account"));
    assert_eq!(param("code_challenge_method").as_deref(), Some("S256"));
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = http.get(url).send().await.unwrap();
    assert_eq!(response.status(), 302);
    let location = response.headers()["location"].to_str().unwrap();
    let redirect = Url::parse(location).unwrap();
    assert!(location.starts_with(REDIRECT), "{location}");
    assert_eq!(
        redirect
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .as_deref(),
        Some("s-1")
    );
    redirect
}

/// Signs in, returning the tokens.
async fn sign_in(mock: &Mock, hint: Option<&str>) -> TokenResponse {
    let pkce = Pkce::generate();
    let redirect = authorize(mock, &pkce, hint).await;
    let code = redirect
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
        .expect("a code");
    identity(mock)
        .exchange_code(&code, &pkce.verifier, REDIRECT, SCOPES)
        .await
        .unwrap()
}

fn graph(mock: &Mock, tokens: &TokenResponse) -> Client {
    Client::new(
        Endpoints::mock(&mock.server.url()),
        StaticToken(tokens.access_token.clone()),
    )
}

#[tokio::test]
async fn a_work_account_signs_in() {
    let mock = Mock::start().await;
    let tokens = sign_in(&mock, None).await;
    assert!(tokens.refresh_token.is_some());
    assert!(
        tokens.missing_scopes(SCOPES).is_empty(),
        "{:?}",
        tokens.scope
    );
    let claims = IdTokenClaims::decode(tokens.id_token.as_deref().unwrap()).unwrap();
    assert_eq!(claims.aud.as_deref(), Some(CLIENT));
    assert_eq!(claims.nonce.as_deref(), Some("n-1"));
    assert_eq!(claims.tid.as_deref(), Some(TENANT));
    assert!(!claims.is_personal());
    assert_eq!(claims.name.as_deref(), Some("Maya Okafor"));
    assert_eq!(claims.preferred_username.as_deref(), Some(EMAIL));

    let me = graph(&mock, &tokens).people().me().await.unwrap();
    assert_eq!(me.address(), Some(EMAIL));
    assert_eq!(me.id, claims.oid, "a work account's oid is its Graph id");
}

#[tokio::test]
async fn a_personal_account_signs_in_to_the_consumer_tenant() {
    let mock = Mock::start().await;
    let tokens = sign_in(&mock, Some("maya.okafor@outlook.com")).await;
    let claims = IdTokenClaims::decode(tokens.id_token.as_deref().unwrap()).unwrap();
    assert_eq!(claims.tid.as_deref(), Some(CONSUMER_TENANT));
    assert!(claims.is_personal());
    let client = graph(&mock, &tokens);
    assert!(
        client
            .mail()
            .folder(outlook::well_known::INBOX)
            .await
            .is_ok()
    );
    let error = client.people().users().await.unwrap_err();
    assert_eq!(error.status(), Some(403), "no directory: {error}");
}

#[tokio::test]
async fn each_refresh_replaces_the_refresh_token() {
    let mock = Mock::start().await;
    let first = sign_in(&mock, None).await;
    let identity = identity(&mock);
    let second = identity
        .refresh(first.refresh_token.as_deref().unwrap(), SCOPES)
        .await
        .unwrap();
    let rotated = second.refresh_token.as_deref().unwrap();
    assert_ne!(Some(rotated), first.refresh_token.as_deref());
    let third = identity.refresh(rotated, SCOPES).await.unwrap();
    assert!(graph(&mock, &third).people().me().await.is_ok());

    let error = identity.refresh("M.C-unknown", SCOPES).await.unwrap_err();
    let outlook::Error::Identity(error) = error else {
        panic!("{error:?}");
    };
    assert_eq!(error.error, "invalid_grant");
    assert_eq!(error.code(), Some(9_002_313));
}

#[tokio::test]
async fn consent_errors_come_back_with_their_codes() {
    let mock = Mock::start().await;
    // On the redirect, when the organization's admin hasn't approved the app.
    mock.fault(
        json!({ "path": "/common/oauth2/v2.0/authorize", "status": 400, "reason": "AADSTS65001" }),
    )
    .await;
    let redirect = authorize(&mock, &Pkce::generate(), None).await;
    let error = RedirectError::from_redirect(&redirect).expect("an error");
    assert_eq!(error.code(), Some(65001));
    assert!(CONSENT_ERRORS.contains(&65001));

    // And at the token endpoint.
    let pkce = Pkce::generate();
    let redirect = authorize(&mock, &pkce, None).await;
    let code = redirect
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
        .unwrap();
    mock.fault(
        json!({ "path": "/common/oauth2/v2.0/token", "status": 400, "reason": "AADSTS90094" }),
    )
    .await;
    let error = identity(&mock)
        .exchange_code(&code, &pkce.verifier, REDIRECT, SCOPES)
        .await
        .unwrap_err();
    let outlook::Error::Identity(error) = error else {
        panic!("{error:?}");
    };
    assert_eq!(error.code(), Some(90094));
    assert!(
        error.to_string().starts_with("invalid_grant: AADSTS90094:"),
        "{error}"
    );
}

#[tokio::test]
async fn the_verifier_must_match() {
    let mock = Mock::start().await;
    let pkce = Pkce::generate();
    let redirect = authorize(&mock, &pkce, None).await;
    let code = redirect
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
        .unwrap();
    let error = identity(&mock)
        .exchange_code(&code, &Pkce::generate().verifier, REDIRECT, SCOPES)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, outlook::Error::Identity(error) if error.code() == Some(501_481)),
        "{error}"
    );
}
