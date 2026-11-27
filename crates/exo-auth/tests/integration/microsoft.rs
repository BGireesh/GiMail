//! Microsoft sign-in end to end against mock-servers' Microsoft identity platform and Graph.

use crate::support;

use exo_auth::{Error, MICROSOFT_SCOPES, Provider, SecretStore};
use outlook::TokenProvider;
use serde_json::json;
use support::{EMAIL, Harness, MICROSOFT_CLIENT_ID, browser, param};

/// The mock's account, as a Microsoft account.
const KEY: &str = "microsoft maya@brightmoor.co";

/// The sign-in error inside a Graph client's failure, if that's what it is.
fn sign_in_again(error: &outlook::Error) -> Option<&str> {
    let outlook::Error::Token(inner) = error else {
        return None;
    };
    match inner.downcast_ref::<Error>()? {
        Error::SignInAgain { account } => Some(account),
        _ => None,
    }
}

#[tokio::test]
async fn signs_in_through_the_loopback_redirect() {
    let harness = Harness::new().await;
    let (open, page) = browser(Clone::clone);
    let account = harness.auth.sign_in_microsoft(None, open).await.unwrap();
    assert_eq!(account.email, EMAIL);
    assert_eq!(account.provider, Provider::Microsoft);
    assert_eq!(account.key(), KEY);
    assert_eq!(account.name.as_deref(), Some("Maya Okafor"));

    let page = page.await.unwrap();
    assert_eq!(page.status, 200);
    assert!(
        page.text.contains("Thanks for allowing Exo!"),
        "{}",
        page.text
    );
    let url = &page.opened;
    assert_eq!(param(url, "client_id"), MICROSOFT_CLIENT_ID);
    assert!(param(url, "redirect_uri").starts_with("http://localhost:"));
    assert_eq!(param(url, "scope"), MICROSOFT_SCOPES.join(" "));
    assert_eq!(param(url, "prompt"), "select_account");
    assert_eq!(param(url, "code_challenge_method"), "S256");
    assert!(url.query_pairs().all(|(key, _)| key != "login_hint"));

    assert_eq!(harness.auth.accounts(), [account]);
    assert_eq!(
        harness.secrets.get(EMAIL).unwrap(),
        None,
        "not under Google's key"
    );
    assert!(
        harness
            .secrets
            .get(KEY)
            .unwrap()
            .is_some_and(|grant| grant.starts_with("M.C"))
    );
    let entry = std::fs::read_to_string(harness.dir.path().join("accounts.json")).unwrap();
    assert!(entry.contains(r#""provider": "microsoft""#), "{entry}");

    let me = harness.graph(KEY).people().me().await.unwrap();
    assert_eq!(me.address(), Some(EMAIL));
}

#[tokio::test]
async fn a_hint_preselects_the_account() {
    let harness = Harness::new().await;
    let (open, page) = browser(Clone::clone);
    let account = harness
        .auth
        .sign_in_microsoft(Some("maya.okafor@outlook.com"), open)
        .await
        .unwrap();
    assert_eq!(
        param(&page.await.unwrap().opened, "login_hint"),
        "maya.okafor@outlook.com"
    );
    // The mock signs a personal account in for an outlook.com address.
    assert_eq!(account.email, "maya.okafor@outlook.com");
    assert_eq!(account.key(), "microsoft maya.okafor@outlook.com");
}

#[tokio::test]
async fn each_refresh_saves_the_rotated_refresh_token() {
    let harness = Harness::new().await;
    harness.sign_in_microsoft().await;
    let signed_in = harness.secrets.get(KEY).unwrap().unwrap();
    let tokens = harness.auth.tokens(KEY).unwrap();
    let first = tokens.access_token().await.unwrap();
    tokens.invalidate(&first);
    let second = tokens.access_token().await.unwrap();
    assert_ne!(second, first);
    let rotated = harness.secrets.get(KEY).unwrap().unwrap();
    assert_ne!(rotated, signed_in, "Microsoft's new refresh token is kept");

    tokens.invalidate(&second);
    tokens.access_token().await.unwrap();
    assert_ne!(harness.secrets.get(KEY).unwrap().unwrap(), rotated);

    // The next launch refreshes from the latest.
    let auth = harness.relaunch();
    let client = outlook::Client::new(
        auth.microsoft_endpoints().clone(),
        auth.tokens(KEY).unwrap(),
    );
    assert!(client.people().me().await.is_ok());
}

#[tokio::test]
async fn a_google_and_a_microsoft_account_can_share_an_address() {
    let harness = Harness::new().await;
    let google = harness.sign_in().await;
    let microsoft = harness.sign_in_microsoft().await;
    assert_eq!(google.email, microsoft.email);
    let keys: Vec<String> = harness
        .auth
        .accounts()
        .iter()
        .map(exo_auth::Account::key)
        .collect();
    assert_eq!(keys, [EMAIL, KEY]);
    let google_grant = harness.secrets.get(EMAIL).unwrap().unwrap();
    let microsoft_grant = harness.secrets.get(KEY).unwrap().unwrap();
    assert!(google_grant.starts_with("1//") && microsoft_grant.starts_with("M.C"));

    // Both refresh, and neither's grant touches the other's.
    let google_tokens = harness.auth.tokens(EMAIL).unwrap();
    let microsoft_tokens = harness.auth.tokens(KEY).unwrap();
    let google_first = gmail::TokenProvider::access_token(&google_tokens)
        .await
        .unwrap();
    gmail::TokenProvider::invalidate(&google_tokens, &google_first);
    let microsoft_first = microsoft_tokens.access_token().await.unwrap();
    microsoft_tokens.invalidate(&microsoft_first);
    let gmail = gmail::Client::new(harness.auth.endpoints().clone(), google_tokens.clone());
    let profile = gmail.gmail().profile().await.unwrap();
    assert_eq!(profile.email_address.as_deref(), Some(EMAIL));
    assert!(harness.graph(KEY).people().me().await.is_ok());
    assert_eq!(
        harness.secrets.get(EMAIL).unwrap().as_deref(),
        Some(google_grant.as_str()),
        "Microsoft's rotation left Google's grant alone"
    );
    let rotated = harness.secrets.get(KEY).unwrap().unwrap();
    assert_ne!(rotated, microsoft_grant);
    assert_ne!(rotated, google_grant);

    // Each is its own account in the list.
    assert_eq!(harness.auth.default_account().unwrap().key(), KEY);
    harness.auth.use_account(EMAIL).unwrap();
    assert_eq!(
        harness.relaunch().default_account().unwrap().provider,
        Provider::Google
    );
    harness
        .auth
        .set_profile_picture(KEY, Some(b"microsoft"))
        .unwrap();
    harness
        .auth
        .set_profile_picture(EMAIL, Some(b"google"))
        .unwrap();
    harness.auth.forget(KEY).unwrap();
    assert_eq!(harness.auth.accounts(), [google]);
    assert_eq!(harness.secrets.get(KEY).unwrap(), None);
    assert_eq!(
        harness.secrets.get(EMAIL).unwrap().as_deref(),
        Some(google_grant.as_str())
    );
    assert_eq!(
        harness.auth.profile_picture(EMAIL).unwrap().as_deref(),
        Some(&b"google"[..])
    );
    assert!(microsoft_tokens.needs_sign_in());
    assert!(!google_tokens.needs_sign_in());
    assert!(gmail.gmail().profile().await.is_ok());
}

#[tokio::test]
async fn consent_errors_say_an_admin_must_approve() {
    let harness = Harness::new().await;
    // On the redirect: the administrator hasn't approved Exo, approval is required, the user
    // declined.
    for code in [65001, 90094, 65004] {
        harness
            .fault(json!({
                "path": "/common/oauth2/v2.0/authorize",
                "status": 400,
                "reason": format!("AADSTS{code}"),
            }))
            .await;
        let (open, page) = browser(Clone::clone);
        let error = harness
            .auth
            .sign_in_microsoft(None, open)
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::ConsentRequired { code: got } if got == code),
            "{code}: {error}"
        );
        let page = page.await.unwrap();
        assert_eq!(page.status, 400);
        assert!(
            page.text
                .contains("Sorry, something went wrong! Please try again later."),
            "{}",
            page.text
        );
    }
    // At the token endpoint: the user may not consent.
    harness
        .fault(
            json!({ "path": "/common/oauth2/v2.0/token", "status": 400, "reason": "AADSTS90093" }),
        )
        .await;
    let (open, _page) = browser(Clone::clone);
    let error = harness
        .auth
        .sign_in_microsoft(None, open)
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::ConsentRequired { code: 90093 }),
        "{error}"
    );
    assert!(harness.auth.accounts().is_empty(), "nothing was saved");
}

#[tokio::test]
async fn other_refusals_are_not_consent_errors() {
    let harness = Harness::new().await;
    harness
        .fault(json!({
            "path": "/common/oauth2/v2.0/authorize",
            "status": 400,
            "reason": "AADSTS50076",
        }))
        .await;
    let (open, _page) = browser(Clone::clone);
    let error = harness
        .auth
        .sign_in_microsoft(None, open)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, Error::Authorization { error } if error == "interaction_required"),
        "{error}"
    );
}

#[tokio::test]
async fn an_expired_grant_asks_to_sign_in_again() {
    let harness = Harness::new().await;
    harness.sign_in_microsoft().await;
    let tokens = harness.auth.tokens(KEY).unwrap();
    let rejected = tokens.access_token().await.unwrap();
    harness
        .fault(
            json!({ "path": "/common/oauth2/v2.0/token", "status": 400, "reason": "AADSTS70008" }),
        )
        .await;
    tokens.invalidate(&rejected);
    let error = tokens.access_token().await.unwrap_err();
    assert_eq!(sign_in_again(&error), Some(KEY), "{error}");
    assert!(tokens.needs_sign_in());
    assert!(
        harness
            .auth
            .accounts()
            .iter()
            .any(|account| account.key() == KEY),
        "it stays listed"
    );
}

#[tokio::test]
async fn microsoft_sign_in_works_without_a_google_client_file() {
    let harness = Harness::new().await;
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("google-oauth-client.json");
    let config = exo_auth::ClientConfig::without_google(missing.clone(), false).with_microsoft(
        exo_auth::MicrosoftClient::new(
            MICROSOFT_CLIENT_ID,
            harness.auth.microsoft_endpoints().clone(),
        ),
    );
    let auth =
        exo_auth::Auth::open(config, dir.path(), exo_auth::MemorySecrets::default()).unwrap();

    let (open_google, _page) = browser(Clone::clone);
    let err = auth.sign_in(None, open_google).await.unwrap_err();
    assert!(
        matches!(err, Error::MissingClient { ref path, from_env: false } if *path == missing),
        "{err}"
    );

    let (open_ms, _page) = browser(Clone::clone);
    let account = auth.sign_in_microsoft(None, open_ms).await.unwrap();
    assert_eq!(account.email, EMAIL);
    assert_eq!(account.provider, Provider::Microsoft);
    assert_eq!(account.key(), KEY);
}

#[tokio::test]
async fn rotated_refresh_does_not_block_needs_sign_in_during_secret_write() {
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    };

    #[derive(Clone)]
    struct PausingSecrets {
        inner: Arc<exo_auth::MemorySecrets>,
        arm: Arc<AtomicBool>,
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    }

    impl SecretStore for PausingSecrets {
        fn get(&self, account: &str) -> Result<Option<String>, Error> {
            self.inner.get(account)
        }

        fn set(&self, account: &str, secret: &str) -> Result<(), Error> {
            let pause = self.arm.swap(false, Ordering::SeqCst);
            if pause {
                self.entered.wait();
                self.release.wait();
            }
            self.inner.set(account, secret)
        }

        fn delete(&self, account: &str) -> Result<(), Error> {
            self.inner.delete(account)
        }
    }

    let harness = Harness::new().await;
    let dir = tempfile::tempdir().unwrap();
    let arm = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let secrets = PausingSecrets {
        inner: Arc::new(exo_auth::MemorySecrets::default()),
        arm: arm.clone(),
        entered: entered.clone(),
        release: release.clone(),
    };
    let config = exo_auth::ClientConfig::without_google(dir.path().join("missing.json"), false)
        .with_microsoft(exo_auth::MicrosoftClient::new(
            MICROSOFT_CLIENT_ID,
            harness.auth.microsoft_endpoints().clone(),
        ));
    let auth = exo_auth::Auth::open(config, dir.path(), secrets.clone()).unwrap();
    let (open_ms, _page) = browser(Clone::clone);
    auth.sign_in_microsoft(None, open_ms).await.unwrap();

    let tokens = auth.tokens(KEY).unwrap();
    let first = tokens.access_token().await.unwrap();
    tokens.invalidate(&first);
    arm.store(true, Ordering::SeqCst);

    let refreshing = tokio::spawn({
        let tokens = tokens.clone();
        async move { tokens.access_token().await }
    });

    // Wait until `secrets.set` is actively executing on the blocking pool.
    tokio::task::spawn_blocking(move || entered.wait())
        .await
        .unwrap();

    // `needs_sign_in()` must not stall while Keychain I/O is in flight.
    assert!(!tokens.needs_sign_in());
    tokio::task::spawn_blocking(move || release.wait())
        .await
        .unwrap();

    let second = refreshing.await.unwrap().unwrap();
    assert_ne!(second, first);
    assert!(secrets.inner.get(KEY).unwrap().is_some());
}
