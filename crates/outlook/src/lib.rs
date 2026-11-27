//! Typed models and an async client for Microsoft Graph v1.0's mail, calendar, mail settings
//! and people, and the Microsoft identity platform's sign-in, for Outlook.com and Microsoft 365
//! mailboxes.
//!
//! The models are transcribed by hand from Graph's reference and keep to what Exo reads and
//! writes. `mock-servers` serves the same wire format. Service origins are configuration
//! ([`Endpoints`]), so pointing Exo at the mock is a matter of swapping them.
//!
//! Every request asks for immutable IDs, so a message keeps its ID through moves. A client
//! serves one mailbox and sends at most four of its requests at once, Outlook's limit, and
//! waits out a throttled request's `Retry-After` before sending it again.
//!
//! ```no_run
//! # async fn demo() -> Result<(), outlook::Error> {
//! use outlook::{Client, Endpoints, StaticToken};
//!
//! let client = Client::new(Endpoints::microsoft(), StaticToken("eyJ0...".into()));
//! let inbox = client
//!     .mail()
//!     .messages_delta(outlook::well_known::INBOX)
//!     .max_page_size(50)
//!     .all()
//!     .await?;
//! # Ok(()) }
//! ```

mod batch;
pub mod calendar;
mod client;
mod endpoints;
mod error;
pub mod identity;
mod mail;
mod model;
pub mod odata;
pub mod people;
pub mod settings;
pub mod wire;

pub use batch::{MAX_BATCH, Order};
pub use calendar::Calendars;
pub use client::{Call, Client, MAX_CONCURRENT, Media, StaticToken, TokenFuture, TokenProvider};
pub use endpoints::{Endpoints, IdentityEndpoints};
pub use error::{ApiError, Error, ErrorBody, ErrorResponse, InnerError};
pub use mail::Mail;
pub use model::*;
pub use odata::Page;
