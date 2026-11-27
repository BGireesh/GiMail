//! Calendar accounts, the other Google and Microsoft accounts a mail account signs in for their
//! calendars alone, tested against mock-servers. There, a `login_hint` signs in another address,
//! with calendars of its own.

use crate::support;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use exo_auth::{Account, CALENDAR_SCOPES, Error, MICROSOFT_CALENDAR_SCOPES, Provider, SecretStore};
use outlook::TokenProvider;
use serde_json::json;
use support::{EMAIL, Harness, MICROSOFT_CLIENT_ID, browser, param};
use url::Url;

/// Another Google account of the user's.
const PERSONAL: &str = "maya.lin@example.com";
/// Another work account in the mock directory (works on both Google and Microsoft).
const WORK_CAL: &str = "tom@brightmoor.co";
/// A personal Outlook.com account of the user's.
const OUTLOOK_PERSONAL: &str = "maya.okafor@outlook.com";
/// The mock's owner as a Microsoft account.
const MS_OWNER: &str = "microsoft maya@brightmoor.co";

/// A consent URL with the user picking `email` at the provider.
fn picking(url: &Url, email: &str) -> Url {
    let mut visit = url.clone();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "login_hint")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    visit
        .query_pairs_mut()
        .clear()
        .extend_pairs(pairs)
        .append_pair("login_hint", email);
    visit
}

/// Drops `removed` from the authorization URL's `scope` parameter, simulating a tenant granting
/// fewer permissions than requested.
fn without_scope(url: &Url, removed: &str) -> Url {
    let scopes = param(url, "scope").replace(removed, "");
    let mut visit = url.clone();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| {
            let value = if key == "scope" {
                scopes.clone()
            } else {
                value.into_owned()
            };
            (key.into_owned(), value)
        })
        .collect();
    visit.query_pairs_mut().clear().extend_pairs(pairs);
    visit
}

/// Decodes the `scp` claim from a work-account mock JWT access token.
fn jwt_scopes(access_token: &str) -> Vec<String> {
    let payload = access_token.split('.').nth(1).expect("JWT payload");
    let claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
    claims["scp"]
        .as_str()
        .expect("scp claim")
        .split_whitespace()
        .map(|scope| scope.rsplit('/').next().unwrap_or(scope).to_owned())
        .collect()
}

/// The sign-in error inside an Outlook client's failure, if that's what it is.
fn outlook_sign_in_again(error: &outlook::Error) -> Option<&str> {
    let outlook::Error::Token(inner) = error else {
        return None;
    };
    match inner.downcast_ref::<Error>()? {
        Error::SignInAgain { account } => Some(account),
        _ => None,
    }
}

#[tokio::test]
async fn a_calendar_account_asks_for_calendar_alone_and_its_grant_is_the_mail_accounts() {
    let harness = Harness::new().await;
    harness.sign_in().await;
    // Its Gmail isn't asked about, because a calendar account needs no mailbox.
    harness.fault(serde_json::json!({ "path": "/gmail/v1/users/me/profile", "status": 400, "message": "Mail service not enabled" })).await;
    let (open, page) = browser(|url| picking(url, PERSONAL));
    let email = harness
        .auth
        .sign_in_calendar(EMAIL, None, open)
        .await
        .unwrap();
    assert_eq!(email, PERSONAL);
    let page = page.await.unwrap();
    assert_eq!(param(&page.opened, "scope"), CALENDAR_SCOPES.join(" "));
    assert_eq!(param(&page.opened, "prompt"), "consent");

    let mail = &harness.auth.accounts()[0];
    assert_eq!(
        (mail.email.as_str(), mail.calendars.as_slice()),
        (EMAIL, [PERSONAL.to_owned()].as_slice()),
        "listed under the mail account, which stays the only account"
    );
    assert_eq!(harness.auth.accounts().len(), 1);
    assert!(
        harness.secrets.get(PERSONAL).unwrap().is_none(),
        "kept apart from a mail grant of its own"
    );

    // Its tokens read its own calendars, on the next launch too.
    for auth in [harness.auth.clone(), harness.relaunch()] {
        let tokens = auth.calendar_tokens(EMAIL, PERSONAL).unwrap();
        let calendar = gmail::Client::new(auth.endpoints().clone(), tokens);
        let list = calendar
            .calendar()
            .calendar_list(&gmail::calendar::CalendarListQuery::default())
            .await
            .unwrap();
        assert_eq!(
            list.items
                .iter()
                .find(|entry| entry.primary == Some(true))
                .and_then(|entry| entry.id.as_deref()),
            Some(PERSONAL)
        );
    }
    assert!(matches!(
        harness.auth.calendar_tokens(EMAIL, "someone@example.com"),
        Err(Error::UnknownAccount(_))
    ));
}

#[tokio::test]
async fn signing_in_another_account_than_the_one_asked_for_saves_nothing() {
    let harness = Harness::new().await;
    harness.sign_in().await;
    let (open, _page) = browser(|url| picking(url, "someone@example.com"));
    let error = harness
        .auth
        .sign_in_calendar(EMAIL, Some(PERSONAL), open)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, Error::WrongAccount { expected } if expected == PERSONAL),
        "{error}"
    );
    assert!(harness.auth.accounts()[0].calendars.is_empty());

    // The mail account's own address needs no grant of its own.
    let (open, _page) = browser(Clone::clone);
    assert_eq!(
        harness
            .auth
            .sign_in_calendar(EMAIL, None, open)
            .await
            .unwrap(),
        EMAIL
    );
    assert!(harness.auth.accounts()[0].calendars.is_empty());
}

#[tokio::test]
async fn removing_a_calendar_account_or_signing_its_mail_account_out_forgets_its_grant() {
    let harness = Harness::new().await;
    harness.sign_in().await;
    for email in [PERSONAL, "bea@example.com"] {
        let (open, _page) = browser(move |url| picking(url, email));
        harness
            .auth
            .sign_in_calendar(EMAIL, None, open)
            .await
            .unwrap();
    }
    let tokens = harness.auth.calendar_tokens(EMAIL, PERSONAL).unwrap();
    harness.auth.forget_calendar(EMAIL, PERSONAL).unwrap();
    assert!(tokens.needs_sign_in(), "its tokens stop");
    assert_eq!(harness.auth.accounts()[0].calendars, ["bea@example.com"]);

    let bea = harness
        .auth
        .calendar_tokens(EMAIL, "bea@example.com")
        .unwrap();
    harness.auth.forget(EMAIL).unwrap();
    assert!(
        bea.needs_sign_in(),
        "the mail account's sign-out takes its calendar accounts"
    );
    let relaunched = harness.relaunch();
    assert!(relaunched.accounts().is_empty());
}

#[tokio::test]
async fn google_owner_signs_in_a_microsoft_calendar_account() {
    let harness = Harness::new().await;
    harness.sign_in().await;
    // Mail access isn't requested or probed for a calendar account.
    harness
        .fault(json!({
            "path": "/v1.0/me/mailFolders/inbox",
            "status": 403,
            "reason": "ErrorAccessDenied",
        }))
        .await;

    let (open, page) = browser(|url| picking(url, WORK_CAL));
    let cal_key = harness
        .auth
        .sign_in_calendar_microsoft(EMAIL, None, open)
        .await
        .unwrap();
    let expected_key = Account::key_for(Provider::Microsoft, WORK_CAL);
    assert_eq!(cal_key, expected_key);

    let page = page.await.unwrap();
    assert_eq!(page.status, 200);
    assert!(
        page.text.contains("Thanks for allowing Exo!"),
        "{}",
        page.text
    );
    assert_eq!(param(&page.opened, "client_id"), MICROSOFT_CLIENT_ID);
    assert!(param(&page.opened, "redirect_uri").starts_with("http://localhost:"));
    assert_eq!(
        param(&page.opened, "scope"),
        MICROSOFT_CALENDAR_SCOPES.join(" ")
    );
    assert_eq!(param(&page.opened, "prompt"), "select_account");
    assert_eq!(param(&page.opened, "code_challenge_method"), "S256");

    let mail = &harness.auth.accounts()[0];
    assert_eq!(
        (mail.email.as_str(), mail.calendars.as_slice()),
        (EMAIL, [expected_key.clone()].as_slice()),
        "listed under the Google owner, which stays the only mail account"
    );
    assert_eq!(harness.auth.accounts().len(), 1);
    assert!(
        harness.secrets.get(&expected_key).unwrap().is_none(),
        "kept apart from a Microsoft mail grant of its own"
    );
    assert!(
        harness
            .secrets
            .get(&format!("calendar {expected_key} for {EMAIL}"))
            .unwrap()
            .is_some_and(|grant| grant.starts_with("M.C"))
    );

    // Tokens read its Outlook calendars via both raw-key and provider-keyed lookups, across launch.
    for auth in [harness.auth.clone(), harness.relaunch()] {
        for tokens in [
            auth.calendar_tokens(EMAIL, &expected_key).unwrap(),
            auth.calendar_tokens_for(EMAIL, Provider::Microsoft, WORK_CAL)
                .unwrap(),
        ] {
            let graph = outlook::Client::new(auth.microsoft_endpoints().clone(), tokens);
            let list = graph.calendar().calendars().await.unwrap();
            assert_eq!(
                list.value
                    .iter()
                    .find(|cal| cal.is_default_calendar == Some(true))
                    .and_then(|cal| cal.owner.as_ref())
                    .and_then(|owner| owner.address.as_deref()),
                Some(WORK_CAL)
            );
        }
    }
    assert!(matches!(
        harness
            .auth
            .calendar_tokens_for(EMAIL, Provider::Google, WORK_CAL),
        Err(Error::UnknownAccount(_))
    ));
}

#[tokio::test]
async fn microsoft_owner_signs_in_google_and_microsoft_calendars() {
    let harness = Harness::new().await;
    let owner = harness.sign_in_microsoft().await;
    assert_eq!(owner.key(), MS_OWNER);

    // Cross-provider: Microsoft owner + Google calendar (different address).
    let (open, _page) = browser(|url| picking(url, PERSONAL));
    let google_cal = harness
        .auth
        .sign_in_calendar_with(MS_OWNER, Provider::Google, None, open)
        .await
        .unwrap();
    assert_eq!(google_cal, PERSONAL);
    assert!(
        harness
            .secrets
            .get(&format!("calendar {PERSONAL} for {MS_OWNER}"))
            .unwrap()
            .is_some_and(|grant| grant.starts_with("1//"))
    );

    // Cross-provider same email: Microsoft owner + Google calendar at the SAME address is saved.
    let (open, _page) = browser(Clone::clone);
    let google_same = harness
        .auth
        .sign_in_calendar(MS_OWNER, Some(EMAIL), open)
        .await
        .unwrap();
    assert_eq!(google_same, EMAIL);

    // Same-provider: Microsoft owner + another Microsoft calendar (personal Outlook.com).
    let (open, page) = browser(Clone::clone);
    let ms_cal = harness
        .auth
        .sign_in_calendar_with(MS_OWNER, Provider::Microsoft, Some(OUTLOOK_PERSONAL), open)
        .await
        .unwrap();
    assert_eq!(
        param(&page.await.unwrap().opened, "login_hint"),
        OUTLOOK_PERSONAL
    );
    assert_eq!(ms_cal, format!("microsoft {OUTLOOK_PERSONAL}"));
    assert!(
        harness
            .secrets
            .get(&format!("calendar {ms_cal} for {MS_OWNER}"))
            .unwrap()
            .is_some_and(|grant| grant.starts_with("M.C"))
    );

    // Same-provider own account: Microsoft owner signing in its own Microsoft address saves nothing.
    let (open, _page) = browser(Clone::clone);
    assert_eq!(
        harness
            .auth
            .sign_in_calendar_microsoft(MS_OWNER, None, open)
            .await
            .unwrap(),
        MS_OWNER
    );
    assert_eq!(
        harness.auth.accounts()[0].calendars,
        [PERSONAL, EMAIL, ms_cal.as_str()]
    );

    // All three secondary calendar grants work across relaunch.
    let relaunched = harness.relaunch();
    let g_client = gmail::Client::new(
        relaunched.endpoints().clone(),
        relaunched
            .calendar_tokens_for(MS_OWNER, Provider::Google, PERSONAL)
            .unwrap(),
    );
    let g_list = g_client
        .calendar()
        .calendar_list(&gmail::calendar::CalendarListQuery::default())
        .await
        .unwrap();
    assert_eq!(
        g_list
            .items
            .iter()
            .find(|entry| entry.primary == Some(true))
            .and_then(|entry| entry.id.as_deref()),
        Some(PERSONAL)
    );

    let g_same = gmail::Client::new(
        relaunched.endpoints().clone(),
        relaunched
            .calendar_tokens_for(MS_OWNER, Provider::Google, EMAIL)
            .unwrap(),
    );
    assert!(
        g_same
            .calendar()
            .calendar_list(&gmail::calendar::CalendarListQuery::default())
            .await
            .is_ok()
    );

    let ms_client = outlook::Client::new(
        relaunched.microsoft_endpoints().clone(),
        relaunched
            .calendar_tokens_for(MS_OWNER, Provider::Microsoft, OUTLOOK_PERSONAL)
            .unwrap(),
    );
    let ms_list = ms_client.calendar().calendars().await.unwrap();
    assert_eq!(
        ms_list
            .value
            .iter()
            .find(|cal| cal.is_default_calendar == Some(true))
            .and_then(|cal| cal.owner.as_ref())
            .and_then(|owner| owner.address.as_deref()),
        Some(OUTLOOK_PERSONAL)
    );
}

#[tokio::test]
async fn dual_provider_same_email_calendars_stay_isolated_and_forget_cleans_up_both() {
    let harness = Harness::new().await;
    harness.sign_in().await;
    harness.sign_in_microsoft().await;

    // Google owner signs in BOTH Google and Microsoft calendars at WORK_CAL, plus Microsoft at EMAIL.
    let (open, _page) = browser(|url| picking(url, WORK_CAL));
    assert_eq!(
        harness
            .auth
            .sign_in_calendar(EMAIL, None, open)
            .await
            .unwrap(),
        WORK_CAL
    );
    let (open, _page) = browser(|url| picking(url, WORK_CAL));
    let ms_work = harness
        .auth
        .sign_in_calendar_microsoft(EMAIL, None, open)
        .await
        .unwrap();
    assert_eq!(ms_work, format!("microsoft {WORK_CAL}"));

    let (open, _page) = browser(Clone::clone);
    let ms_same = harness
        .auth
        .sign_in_calendar_microsoft(EMAIL, Some(EMAIL), open)
        .await
        .unwrap();
    assert_eq!(ms_same, MS_OWNER);

    let google_owner = harness
        .auth
        .accounts()
        .into_iter()
        .find(|account| account.key() == EMAIL)
        .unwrap();
    assert_eq!(
        google_owner.calendars,
        [WORK_CAL, ms_work.as_str(), MS_OWNER]
    );

    let g_cal_secret_key = format!("calendar {WORK_CAL} for {EMAIL}");
    let ms_cal_secret_key = format!("calendar {ms_work} for {EMAIL}");
    let ms_same_secret_key = format!("calendar {MS_OWNER} for {EMAIL}");
    let ms_mail_secret = harness.secrets.get(MS_OWNER).unwrap().unwrap();
    let g_cal_secret = harness.secrets.get(&g_cal_secret_key).unwrap().unwrap();
    let ms_cal_secret = harness.secrets.get(&ms_cal_secret_key).unwrap().unwrap();
    assert!(g_cal_secret.starts_with("1//"));
    assert!(ms_cal_secret.starts_with("M.C"));
    assert_ne!(
        harness.secrets.get(&ms_same_secret_key).unwrap().as_deref(),
        Some(ms_mail_secret.as_str()),
        "Microsoft calendar grant is separate from Microsoft mail grant at the same address"
    );

    let g_cal_tokens = harness
        .auth
        .calendar_tokens_for(EMAIL, Provider::Google, WORK_CAL)
        .unwrap();
    let ms_cal_tokens = harness
        .auth
        .calendar_tokens_for(EMAIL, Provider::Microsoft, WORK_CAL)
        .unwrap();
    let ms_same_tokens = harness
        .auth
        .calendar_tokens_for(EMAIL, Provider::Microsoft, EMAIL)
        .unwrap();
    let ms_mail_tokens = harness.auth.tokens(MS_OWNER).unwrap();

    // Forgetting only the Google calendar at WORK_CAL leaves the Microsoft one working.
    harness
        .auth
        .forget_calendar_for(EMAIL, Provider::Google, WORK_CAL)
        .unwrap();
    assert!(g_cal_tokens.needs_sign_in());
    assert!(!ms_cal_tokens.needs_sign_in());
    assert_eq!(harness.secrets.get(&g_cal_secret_key).unwrap(), None);
    assert_eq!(
        harness.secrets.get(&ms_cal_secret_key).unwrap().as_deref(),
        Some(ms_cal_secret.as_str())
    );
    let ms_cal_client = outlook::Client::new(
        harness.auth.microsoft_endpoints().clone(),
        ms_cal_tokens.clone(),
    );
    assert!(ms_cal_client.calendar().calendars().await.is_ok());

    // Forgetting the Google mail owner cleans up all its remaining calendar grants across providers,
    // without touching the Microsoft mail account at the same email address.
    harness.auth.forget(EMAIL).unwrap();
    assert!(ms_cal_tokens.needs_sign_in());
    assert!(ms_same_tokens.needs_sign_in());
    assert_eq!(harness.secrets.get(&ms_cal_secret_key).unwrap(), None);
    assert_eq!(harness.secrets.get(&ms_same_secret_key).unwrap(), None);

    assert!(!ms_mail_tokens.needs_sign_in());
    assert_eq!(
        harness.secrets.get(MS_OWNER).unwrap().as_deref(),
        Some(ms_mail_secret.as_str())
    );
    assert_eq!(harness.relaunch().accounts().len(), 1);
    assert_eq!(harness.relaunch().accounts()[0].key(), MS_OWNER);
    assert!(harness.graph(MS_OWNER).people().me().await.is_ok());
}

#[tokio::test]
async fn microsoft_calendar_refresh_rotates_token_with_calendar_scopes_and_revives_on_resign_in() {
    let harness = Harness::new().await;
    harness.sign_in().await;
    let owner_secret = harness.secrets.get(EMAIL).unwrap().unwrap();

    let (open, _page) = browser(|url| picking(url, WORK_CAL));
    let cal_key = harness
        .auth
        .sign_in_calendar_microsoft(EMAIL, None, open)
        .await
        .unwrap();
    let secret_key = format!("calendar {cal_key} for {EMAIL}");
    let signed_in = harness.secrets.get(&secret_key).unwrap().unwrap();

    let tokens = harness.auth.calendar_tokens(EMAIL, &cal_key).unwrap();
    let first = tokens.access_token().await.unwrap();
    tokens.invalidate(&first);
    let second = tokens.access_token().await.unwrap();
    assert_ne!(second, first);

    // Refresh must request only Microsoft calendar scopes (never mail scopes).
    let refreshed_scopes = jwt_scopes(&second);
    assert!(
        refreshed_scopes.contains(&outlook::identity::scopes::CALENDARS_READ_WRITE.to_owned()),
        "{refreshed_scopes:?}"
    );
    assert!(
        refreshed_scopes.contains(&outlook::identity::scopes::USER_READ.to_owned()),
        "{refreshed_scopes:?}"
    );
    assert!(
        !refreshed_scopes.contains(&outlook::identity::scopes::MAIL_READ_WRITE.to_owned()),
        "calendar refresh must not ask for mail scopes: {refreshed_scopes:?}"
    );
    assert!(
        !refreshed_scopes.contains(&outlook::identity::scopes::MAIL_SEND.to_owned()),
        "calendar refresh must not ask for mail scopes: {refreshed_scopes:?}"
    );

    let rotated = harness.secrets.get(&secret_key).unwrap().unwrap();
    assert_ne!(rotated, signed_in, "rotated refresh token is persisted");
    assert_eq!(
        harness.secrets.get(EMAIL).unwrap().as_deref(),
        Some(owner_secret.as_str()),
        "rotation leaves the owner's mail secret alone"
    );

    // The next launch refreshes from the rotated secret.
    let relaunched = harness.relaunch();
    let graph = outlook::Client::new(
        relaunched.microsoft_endpoints().clone(),
        relaunched.calendar_tokens(EMAIL, &cal_key).unwrap(),
    );
    assert!(graph.calendar().calendars().await.is_ok());

    // An expired grant transitions the calendar's Tokens to needs_sign_in.
    harness
        .fault(
            json!({ "path": "/common/oauth2/v2.0/token", "status": 400, "reason": "AADSTS70008" }),
        )
        .await;
    tokens.invalidate(&second);
    let error = tokens.access_token().await.unwrap_err();
    assert_eq!(outlook_sign_in_again(&error), Some(secret_key.as_str()));
    assert!(tokens.needs_sign_in());

    // Re-signing in revives the same Tokens instance without duplicating the calendars entry.
    let (open, _page) = browser(|url| picking(url, WORK_CAL));
    assert_eq!(
        harness
            .auth
            .sign_in_calendar_microsoft(EMAIL, Some(WORK_CAL), open)
            .await
            .unwrap(),
        cal_key
    );
    assert_eq!(harness.auth.accounts()[0].calendars, [cal_key]);
    assert!(!tokens.needs_sign_in());
    let revived = outlook::Client::new(harness.auth.microsoft_endpoints().clone(), tokens);
    assert!(revived.calendar().calendars().await.is_ok());
}

#[tokio::test]
async fn microsoft_calendar_errors_save_nothing() {
    let harness = Harness::new().await;
    harness.sign_in().await;

    // Wrong account when a login_hint is given.
    let (open, _page) = browser(|url| picking(url, WORK_CAL));
    let error = harness
        .auth
        .sign_in_calendar_microsoft(EMAIL, Some(OUTLOOK_PERSONAL), open)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, Error::WrongAccount { expected } if expected == OUTLOOK_PERSONAL),
        "{error}"
    );
    assert!(harness.auth.accounts()[0].calendars.is_empty());

    // Consent errors on redirect and token exchange.
    for code in [65001, 90094, 65004] {
        harness
            .fault(json!({
                "path": "/common/oauth2/v2.0/authorize",
                "status": 400,
                "reason": format!("AADSTS{code}"),
            }))
            .await;
        let (open, _page) = browser(Clone::clone);
        let error = harness
            .auth
            .sign_in_calendar_microsoft(EMAIL, None, open)
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::ConsentRequired { code: got } if got == code),
            "{code}: {error}"
        );
    }
    harness
        .fault(
            json!({ "path": "/common/oauth2/v2.0/token", "status": 400, "reason": "AADSTS90093" }),
        )
        .await;
    let (open, _page) = browser(Clone::clone);
    let error = harness
        .auth
        .sign_in_calendar_microsoft(EMAIL, None, open)
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::ConsentRequired { code: 90093 }),
        "{error}"
    );

    // Missing Calendars.ReadWrite scope fails with MissingScopes.
    let (open, _page) =
        browser(|url| without_scope(url, outlook::identity::scopes::CALENDARS_READ_WRITE));
    let error = harness
        .auth
        .sign_in_calendar_microsoft(EMAIL, None, open)
        .await
        .unwrap_err();
    assert!(
        matches!(
            &error,
            Error::MissingScopes(missing)
                if missing == &[outlook::identity::scopes::CALENDARS_READ_WRITE]
        ),
        "{error}"
    );

    // Account without an Exchange Online mailbox fails calendar verification with NoMailbox.
    harness
        .fault(json!({
            "path": "/v1.0/me/calendar",
            "status": 404,
            "reason": "MailboxNotEnabledForRESTAPI",
        }))
        .await;
    let (open, _page) = browser(|url| picking(url, PERSONAL));
    let error = harness
        .auth
        .sign_in_calendar_microsoft(EMAIL, None, open)
        .await
        .unwrap_err();
    assert!(matches!(error, Error::NoMailbox), "{error}");

    assert!(harness.auth.accounts()[0].calendars.is_empty());
}

#[tokio::test]
async fn microsoft_calendar_sign_in_works_without_a_google_client_file() {
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

    let (open, _page) = browser(Clone::clone);
    let owner = auth.sign_in_microsoft(None, open).await.unwrap();
    assert_eq!(owner.key(), MS_OWNER);

    let (open, _page) = browser(Clone::clone);
    let cal_key = auth
        .sign_in_calendar_microsoft(&owner.key(), Some(OUTLOOK_PERSONAL), open)
        .await
        .unwrap();
    assert_eq!(cal_key, format!("microsoft {OUTLOOK_PERSONAL}"));
    let graph = outlook::Client::new(
        auth.microsoft_endpoints().clone(),
        auth.calendar_tokens(&owner.key(), &cal_key).unwrap(),
    );
    assert!(graph.calendar().calendars().await.is_ok());

    let (open, _page) = browser(Clone::clone);
    let error = auth
        .sign_in_calendar(&owner.key(), None, open)
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::MissingClient { ref path, from_env: false } if *path == missing),
        "{error}"
    );
}
