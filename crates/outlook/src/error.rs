//! Errors, including Graph's JSON error body.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

/// Everything that can go wrong talking to Microsoft.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Graph answered with its JSON error body.
    #[error(transparent)]
    Api(#[from] ApiError),
    /// The identity platform's token endpoint refused a grant.
    #[error(transparent)]
    Identity(Box<crate::identity::IdentityError>),
    /// The request never got a response (connection, TLS, timeout, ...).
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// A response arrived but didn't have the shape expected.
    #[error("unexpected response (HTTP {status}): {reason}")]
    Decode {
        /// HTTP status of the response.
        status: u16,
        /// What failed to parse, and a prefix of the body.
        reason: String,
    },
    /// The token provider couldn't produce an access token.
    #[error("couldn't get an access token: {0}")]
    Token(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A batch couldn't be sent, or its response couldn't be read.
    #[error("batch: {0}")]
    Batch(String),
    /// A paging link Graph returned points somewhere other than Graph, so it isn't followed with
    /// the access token.
    #[error("not a Graph link: {0}")]
    ForeignLink(url::Url),
}

impl Error {
    /// The HTTP status Graph answered with, for API and decoding errors.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api(error) => Some(error.status),
            Self::Decode { status, .. } => Some(*status),
            Self::Http(error) => error.status().map(|status| status.as_u16()),
            Self::Identity(_) | Self::Token(_) | Self::Batch(_) | Self::ForeignLink(_) => None,
        }
    }

    /// Graph's error code (`ErrorItemNotFound`, `SyncStateNotFound`, ...), for API errors.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Api(error) => Some(&error.error.code),
            _ => None,
        }
    }

    /// Whether Graph said the item doesn't exist. With default (not immutable) IDs, an item that
    /// moved answers this under its old ID.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.status() == Some(404)
    }
}

/// A failed Graph request: the HTTP status, Graph's error body and, for a throttled request,
/// how long Graph asked to wait.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{status} {}: {}", error.code, error.message)]
pub struct ApiError {
    /// HTTP status code.
    pub status: u16,
    /// Graph's error.
    pub error: ErrorBody,
    /// `Retry-After`, on a 429 or 503 the client gave up retrying.
    pub retry_after: Option<Duration>,
}

/// The `{"error": {...}}` body of every failed Graph request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// The error itself.
    pub error: ErrorBody,
}

/// Graph's error resource (<https://learn.microsoft.com/graph/errors>). Code against `code`,
/// never `message`.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    /// The error code, e.g. `ErrorItemNotFound`, `TooManyRequests`, `SyncStateNotFound`.
    pub code: String,
    /// Developer-facing detail.
    #[serde(default)]
    pub message: String,
    /// Diagnostics, and sometimes a more specific code. The docs spell it both ways.
    #[serde(alias = "innererror")]
    pub inner_error: Option<Box<InnerError>>,
}

/// `innerError`: when and where the request ran, and sometimes a more specific error.
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InnerError {
    /// A more specific code, e.g. `invalidRange` for an upload session's misplaced range.
    pub code: Option<String>,
    /// Detail for the more specific code.
    pub message: Option<String>,
    /// When the request ran, in UTC without a zone (`2026-11-17T14:00:54`).
    pub date: Option<String>,
    /// Graph's ID for the request.
    #[serde(rename = "request-id")]
    pub request_id: Option<String>,
    /// The client's ID for the request, or Graph's when the client sent none.
    #[serde(rename = "client-request-id")]
    pub client_request_id: Option<String>,
    /// A further, more specific error.
    #[serde(alias = "innererror")]
    pub inner_error: Option<Box<InnerError>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_graphs_error_body() {
        let body = r#"{"error":{"code":"badRequest","message":"Bad range","innerError":{"code":"invalidRange","date":"2026-11-17T14:00:54","request-id":"r","client-request-id":"c"}}}"#;
        let error: ErrorResponse = serde_json::from_str(body).unwrap();
        assert_eq!(error.error.code, "badRequest");
        let inner = error.error.inner_error.unwrap();
        assert_eq!(inner.code.as_deref(), Some("invalidRange"));
        assert_eq!(inner.request_id.as_deref(), Some("r"));
        // The errors page's property table spells it in lower case.
        let lower: ErrorResponse =
            serde_json::from_str(r#"{"error":{"code":"x","innererror":{"code":"y"}}}"#).unwrap();
        assert_eq!(
            lower.error.inner_error.and_then(|inner| inner.code),
            Some("y".into())
        );
    }
}
