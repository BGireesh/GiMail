//! Google and Microsoft sign-in for Exo, the half without UI.
//!
//! Exo has no server to keep refresh tokens and hand out access tokens, so it runs each
//! provider's installed-app flow itself (RFC 8252: PKCE and a loopback redirect, see
//! [`Auth::sign_in`] and [`Auth::sign_in_microsoft`]), keeps refresh tokens in the macOS Keychain
//! ([`Keychain`]) and mints access tokens in-process ([`Tokens`]). The list of accounts, in the
//! user's order, lives in `accounts.json` in the app's state folder, and their profile photos
//! beside it ([`Auth::set_profile_picture`]). An account is its provider and its address
//! ([`Account::key`]), which every call names it by.
//!
//! ```no_run
//! # async fn demo(config_dir: &std::path::Path, state_dir: &std::path::Path) -> Result<(), exo_auth::Error> {
//! # fn open_url(_: &url::Url) -> std::io::Result<()> { Ok(()) }
//! let client = exo_auth::ClientConfig::load(config_dir)?;
//! let auth = exo_auth::Auth::open(client, state_dir, exo_auth::Keychain)?;
//! let account = auth.sign_in(None, open_url).await?;
//! let gmail = gmail::Client::new(auth.endpoints().clone(), auth.tokens(&account.key())?);
//!
//! let account = auth.sign_in_microsoft(None, open_url).await?;
//! let tokens = auth.tokens(&account.key())?;
//! let graph = outlook::Client::new(auth.microsoft_endpoints().clone(), tokens);
//! # Ok(()) }
//! ```

mod accounts;
mod auth;
mod config;
mod error;
mod loopback;
mod microsoft;
mod profile_pictures;
mod secrets;
mod sign_in;
mod tokens;

pub use accounts::{Account, Provider};
pub use auth::Auth;
pub use config::ClientConfig;
pub use error::Error;
pub use microsoft::{EXO_CLIENT_ID, MicrosoftClient};
#[cfg(target_os = "macos")]
pub use secrets::Keychain;
pub use secrets::{MemorySecrets, SecretStore};
pub use sign_in::TIMEOUT;
pub use tokens::Tokens;

use gmail::oauth::scopes;

/// What Exo asks Google for. Every one is required, so sign-in fails when the user unticks any of
/// them on Google's consent screen.
pub const SCOPES: &[&str] = &[
    // Read, label, archive, trash, draft and send. Exo never deletes mail permanently (trash is a
    // label), so it doesn't need full access (`https://mail.google.com/`).
    scopes::GMAIL_MODIFY,
    // Filters (block sender, auto-archive), the vacation responder, send-as aliases and
    // signatures.
    scopes::GMAIL_SETTINGS_BASIC,
    // The calendar pane: reading events, RSVPs, creating and editing events.
    scopes::CALENDAR,
    // Recipient autocomplete from saved contacts, which Exo only reads.
    scopes::CONTACTS_READONLY,
    // Recipient autocomplete from "other contacts", the people you've emailed.
    scopes::CONTACTS_OTHER_READONLY,
    // Recipient autocomplete from a Workspace domain's directory (`people:listDirectoryPeople`).
    scopes::DIRECTORY_READONLY,
    // Which account signed in.
    scopes::USERINFO_EMAIL,
    // The account's name and photo, for the account switcher.
    scopes::USERINFO_PROFILE,
];

/// What a calendar account asks Google for: Calendar and the address only. A calendar account is
/// another Google account, signed in for its calendars alone.
pub const CALENDAR_SCOPES: &[&str] = &[scopes::CALENDAR, scopes::USERINFO_EMAIL];

/// What Exo asks Microsoft for: exactly the permissions Exo's registration lists, so that an
/// administrator's one approval covers every one. Every Graph permission is required, so sign-in
/// fails when a tenant grants fewer.
pub const MICROSOFT_SCOPES: &[&str] = &[
    // A refresh token, and an ID token naming the account.
    outlook::identity::scopes::OFFLINE_ACCESS,
    outlook::identity::scopes::OPENID,
    outlook::identity::scopes::PROFILE,
    outlook::identity::scopes::EMAIL,
    // The account's name and address.
    outlook::identity::scopes::USER_READ,
    // Read, move, flag, categorize and draft mail.
    outlook::identity::scopes::MAIL_READ_WRITE,
    // Send it.
    outlook::identity::scopes::MAIL_SEND,
    // Automatic replies and the mailbox's time zone.
    outlook::identity::scopes::MAILBOX_SETTINGS_READ_WRITE,
    // The calendar pane.
    outlook::identity::scopes::CALENDARS_READ_WRITE,
    // Recipient autocomplete from saved contacts.
    outlook::identity::scopes::CONTACTS_READ_WRITE,
    // Recipient autocomplete from the people the account writes with.
    outlook::identity::scopes::PEOPLE_READ,
    // Recipient autocomplete from a work account's directory.
    outlook::identity::scopes::USER_READ_BASIC_ALL,
];

/// What a calendar account asks Microsoft for: Calendar and the account's identity only. Every
/// Graph permission is required, so sign-in fails when a tenant grants fewer.
pub const MICROSOFT_CALENDAR_SCOPES: &[&str] = &[
    // A refresh token, and an ID token naming the account.
    outlook::identity::scopes::OFFLINE_ACCESS,
    outlook::identity::scopes::OPENID,
    outlook::identity::scopes::PROFILE,
    outlook::identity::scopes::EMAIL,
    // The account's name and address.
    outlook::identity::scopes::USER_READ,
    // The calendar pane: reading events, RSVPs, creating and editing events.
    outlook::identity::scopes::CALENDARS_READ_WRITE,
];
