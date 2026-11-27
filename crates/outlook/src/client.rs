//! The HTTP client: authentication, the headers every Graph request carries, Outlook's
//! throttling, paging and response decoding.

use std::future::{Future, IntoFuture};
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, StatusCode, header};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use tokio::sync::Semaphore;
use url::Url;

use crate::endpoints::{Endpoints, join};
use crate::error::{ApiError, Error, ErrorBody, ErrorResponse};
use crate::odata::Page;

pub(crate) use response::{Answer, Decode};

/// Boxed future returned by [`TokenProvider::access_token`].
pub type TokenFuture<'a> = Pin<Box<dyn Future<Output = Result<String, Error>> + Send + 'a>>;

/// Supplies access tokens. Implement it to plug in refresh and persistence. The client calls it
/// once per request (once per batch), so cache the token and refresh it shortly before expiry.
pub trait TokenProvider: Send + Sync {
    /// A currently valid access token.
    fn access_token(&self) -> TokenFuture<'_>;

    /// Graph rejected `token`, which this provider handed out (the client calls this on every
    /// 401). Drop it if it's still the cached one, so the next
    /// [`access_token`](Self::access_token) call mints a fresh token. Callers retry once
    /// afterwards. The default does nothing, for providers that can't refresh.
    fn invalidate(&self, _token: &str) {}
}

/// One provider shared by several clients.
impl<T: TokenProvider + ?Sized> TokenProvider for Arc<T> {
    fn access_token(&self) -> TokenFuture<'_> {
        (**self).access_token()
    }

    fn invalidate(&self, token: &str) {
        (**self).invalidate(token);
    }
}

/// A fixed access token, for tests and tools.
#[derive(Clone)]
pub struct StaticToken(pub String);

/// Leaves the token out.
impl std::fmt::Debug for StaticToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("StaticToken").finish_non_exhaustive()
    }
}

impl TokenProvider for StaticToken {
    fn access_token(&self) -> TokenFuture<'_> {
        let token = self.0.clone();
        Box::pin(async move { Ok(token) })
    }
}

/// How long [`Client::new`]'s requests may take, start to finish.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Outlook serves an app at most four requests at once per mailbox, and answers a fifth with 429
/// (learn.microsoft.com/graph/throttling-limits, "Outlook service limits").
pub const MAX_CONCURRENT: usize = 4;

/// How many times a throttled request is sent again.
const RETRIES: u32 = 3;

/// The longest `Retry-After` the client waits out itself. Past it, the 429 comes back with its
/// [`ApiError::retry_after`], for the caller to schedule.
const LONGEST_WAIT: Duration = Duration::from_secs(60);

/// The preference every request carries, so an item keeps its ID when it moves. It applies only
/// to the request it's on (learn.microsoft.com/graph/outlook-immutable-id).
const IMMUTABLE_IDS: &str = "IdType=\"ImmutableId\"";

/// Async client for one mailbox's Graph calls. Cheap to clone, and clones share the mailbox's
/// four request slots, so give every caller of one mailbox a clone of one client.
#[derive(Clone)]
pub struct Client {
    pub(crate) http: reqwest::Client,
    pub(crate) endpoints: Arc<Endpoints>,
    tokens: Arc<dyn TokenProvider>,
    slots: Arc<Semaphore>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("endpoints", &self.endpoints)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// A client for `endpoints` that authenticates with `tokens`. A request that hasn't finished
    /// after five minutes fails.
    pub fn new(endpoints: Endpoints, tokens: impl TokenProvider + 'static) -> Self {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("the TLS backend initializes, as reqwest::Client::new assumes");
        Self::with_http(http, endpoints, tokens)
    }

    /// Like [`Client::new`], reusing an existing `reqwest` client (connection pool, timeouts).
    pub fn with_http(
        http: reqwest::Client,
        endpoints: Endpoints,
        tokens: impl TokenProvider + 'static,
    ) -> Self {
        Self {
            http,
            endpoints: Arc::new(endpoints),
            tokens: Arc::new(tokens),
            slots: Arc::new(Semaphore::new(MAX_CONCURRENT)),
        }
    }

    /// The endpoints this client talks to.
    #[must_use]
    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    /// The page at `link`: a [`Page::next_link`], or a [`Page::delta_link`] kept from the last
    /// round. The link holds the query, so add only preferences (`max_page_size`). A link off
    /// Graph fails with [`Error::ForeignLink`] instead of carrying the token elsewhere.
    pub fn follow<T>(&self, link: &Url) -> Call<'_, Page<T>> {
        Call::new(self, Method::GET, link.clone())
    }

    /// `/v1.0/{segments}`.
    pub(crate) fn call<'b, T>(
        &self,
        method: Method,
        segments: impl IntoIterator<Item = &'b str>,
    ) -> Call<'_, T> {
        let url = join(&self.endpoints.graph, segments);
        Call::new(self, method, url)
    }

    /// `/v1.0/me/{segments}`.
    pub(crate) fn me_call<'b, T>(
        &self,
        method: Method,
        segments: impl IntoIterator<Item = &'b str>,
    ) -> Call<'_, T> {
        self.call(method, std::iter::once("me").chain(segments))
    }

    /// Drops `token` from the token provider's cache when Graph rejects it (including inside a
    /// `$batch` response).
    pub(crate) fn invalidate_token(&self, token: &str) {
        self.tokens.invalidate(token);
    }

    /// Sends `request` in one of the mailbox's slots. A throttled answer (429 or 503, or what
    /// `throttled` says) is sent again after its `Retry-After` (Graph's advice, else 1, 2 and
    /// 4 s), up to [`RETRIES`] times, unless Graph asks for more than [`LONGEST_WAIT`]
    /// (learn.microsoft.com/graph/throttling).
    pub(crate) async fn send(
        &self,
        request: &Request,
        throttled: fn(&Answer) -> bool,
    ) -> Result<Answer, Error> {
        let (answer, _) = self.send_with_token(request, throttled).await?;
        Ok(answer)
    }

    /// Like [`Client::send`], also returning the bearer token sent on the final attempt so
    /// [`Client::batch`] can invalidate that exact token if an inner response returns 401.
    pub(crate) async fn send_with_token(
        &self,
        request: &Request,
        throttled: fn(&Answer) -> bool,
    ) -> Result<(Answer, Option<String>), Error> {
        if request.authenticated && !self.endpoints.is_graph(&request.url) {
            return Err(Error::ForeignLink(request.url.clone()));
        }
        let mut retries = 0;
        loop {
            let token = if request.authenticated {
                Some(self.tokens.access_token().await?)
            } else {
                None
            };
            let answer = {
                let _slot = self
                    .slots
                    .acquire()
                    .await
                    .expect("the slots are never closed");
                log::debug!("{} {}", request.method, request.url.path());
                let response = request.build(&self.http, token.as_deref()).send().await?;
                Answer::read(response).await?
            };
            if let Some(token) = &token
                && answer.status == StatusCode::UNAUTHORIZED
            {
                self.tokens.invalidate(token);
            }
            if throttled(&answer) && retries < RETRIES {
                let wait = answer
                    .retry_after
                    .unwrap_or(Duration::from_secs(1 << retries));
                if wait <= LONGEST_WAIT {
                    log::info!("Graph throttled a request, sending it again in {wait:?}");
                    tokio::time::sleep(wait).await;
                    retries += 1;
                    continue;
                }
            }
            return Ok((answer, token));
        }
    }
}

/// One API call, ready to run. `.await` it, or hand several to [`Client::batch`].
#[must_use = "a Call does nothing until awaited"]
pub struct Call<'a, T> {
    pub(crate) client: &'a Client,
    pub(crate) request: Request,
    response: PhantomData<fn() -> T>,
}

/// A request in transport-neutral form, so a call can run alone or inside a batch.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub method: Method,
    pub url: Url,
    /// Preferences besides [`IMMUTABLE_IDS`], e.g. `odata.maxpagesize=50`.
    pub prefer: Vec<String>,
    pub body: Body,
    /// Whether it carries the access token: every Graph request does, an upload session's
    /// pre-authenticated chunks don't.
    pub authenticated: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum Body {
    Empty,
    Json(Box<RawValue>),
    /// An RFC 822 message, sent base64-encoded as `text/plain`, which is how Graph takes MIME
    /// (learn.microsoft.com/graph/outlook-send-mime-message).
    Mime(Vec<u8>),
    /// A range of an upload session's file.
    Chunk {
        content_range: String,
        data: Vec<u8>,
    },
}

impl Request {
    /// The `Prefer` header: immutable IDs, then the call's own preferences.
    pub(crate) fn prefer_header(&self) -> String {
        std::iter::once(IMMUTABLE_IDS)
            .chain(self.prefer.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn build(&self, http: &reqwest::Client, token: Option<&str>) -> reqwest::RequestBuilder {
        let mut request = http.request(self.method.clone(), self.url.clone());
        if let Some(token) = token {
            request = request
                .bearer_auth(token)
                .header("Prefer", self.prefer_header());
        }
        match &self.body {
            // A body-less POST still says `Content-Length: 0`, which servers may insist on.
            Body::Empty if self.method == Method::POST => request.body(Vec::new()),
            Body::Empty => request,
            Body::Json(json) => request
                .header(header::CONTENT_TYPE, "application/json")
                .body(json.get().as_bytes().to_vec()),
            Body::Mime(mime) => request
                .header(header::CONTENT_TYPE, "text/plain")
                .body(crate::wire::encode_base64(mime)),
            Body::Chunk {
                content_range,
                data,
            } => request
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header(header::CONTENT_RANGE, content_range)
                .body(data.clone()),
        }
    }
}

impl<'a, T> Call<'a, T> {
    pub(crate) fn new(client: &'a Client, method: Method, url: Url) -> Self {
        Self {
            client,
            request: Request {
                method,
                url,
                prefer: Vec::new(),
                body: Body::Empty,
                authenticated: true,
            },
            response: PhantomData,
        }
    }

    /// Adds a query parameter.
    pub(crate) fn query(mut self, name: &str, value: impl ToString) -> Self {
        self.request
            .url
            .query_pairs_mut()
            .append_pair(name, &value.to_string());
        self
    }

    pub(crate) fn json(mut self, body: &impl Serialize) -> Self {
        self.request.body = Body::Json(
            serde_json::value::to_raw_value(body).expect("API request types always serialize"),
        );
        self
    }

    pub(crate) fn mime(mut self, rfc822: Vec<u8>) -> Self {
        self.request.body = Body::Mime(rfc822);
        self
    }

    fn prefer(mut self, preference: String) -> Self {
        self.request.prefer.push(preference);
        self
    }

    /// `$select`: only these properties (comma-separated), plus `id`.
    pub fn select(self, properties: &str) -> Self {
        self.query("$select", properties)
    }

    /// `$expand`, e.g. `attachments` or `extensions($filter=id eq 'dev.exo.x')`.
    pub fn expand(self, expand: &str) -> Self {
        self.query("$expand", expand)
    }

    /// `$filter`. String literals go through [`crate::odata::quote`].
    pub fn filter(self, filter: &str) -> Self {
        self.query("$filter", filter)
    }

    /// `$orderby`, e.g. `receivedDateTime desc`. With `$filter`, Outlook wants the ordered
    /// properties to lead the filter, in the same order.
    pub fn order_by(self, order: &str) -> Self {
        self.query("$orderby", order)
    }

    /// `$search`, KQL in double quotes (`"from:ada subject:plans"`).
    pub fn search(self, query: &str) -> Self {
        self.query("$search", query)
    }

    /// `$top`: the page size of a list, or how many items a delta's first round returns.
    pub fn top(self, count: u32) -> Self {
        self.query("$top", count)
    }

    /// `Prefer: odata.maxpagesize`: a delta's page size. Send it again on each link followed.
    pub fn max_page_size(self, size: u32) -> Self {
        self.prefer(format!("odata.maxpagesize={size}"))
    }

    /// `Prefer: outlook.body-content-type="text"`: bodies as plain text rather than HTML.
    pub fn text_body(self) -> Self {
        self.prefer("outlook.body-content-type=\"text\"".to_owned())
    }

    /// `Prefer: outlook.timezone="..."`: returns event, meeting-mail and schedule times in `zone`
    /// (Windows or IANA name) instead of UTC.
    ///
    /// Ignores `zone` if it is empty or contains non-ASCII, control characters, quotes,
    /// backslashes, commas, or semicolons so untrusted values cannot break or inject `Prefer`
    /// directives.
    pub fn timezone(self, zone: &str) -> Self {
        let trimmed = zone.trim_matches(' ');
        let valid = !trimmed.is_empty()
            && zone
                .bytes()
                .all(|b| (0x20..=0x7e).contains(&b) && !matches!(b, b'"' | b'\\' | b',' | b';'));
        if valid {
            self.prefer(format!("outlook.timezone=\"{trimmed}\""))
        } else {
            log::warn!("ignoring invalid outlook.timezone Prefer header value");
            self
        }
    }

    /// The same call with its answer's body read and dropped, so calls whose answers differ
    /// can go in one batch.
    pub fn discarding(self) -> Call<'a, serde::de::IgnoredAny> {
        Call {
            client: self.client,
            request: self.request,
            response: PhantomData,
        }
    }
}

impl<'a, T: DeserializeOwned + Send + 'a> Call<'a, Page<T>> {
    /// Every page, following `@odata.nextLink` with the same preferences, as one page: all the
    /// items, and the round's `@odata.deltaLink` for a delta.
    ///
    /// # Errors
    /// The first page's error, or any later one's.
    pub async fn all(self) -> Result<Page<T>, Error> {
        let (client, prefer) = (self.client, self.request.prefer.clone());
        let mut page = self.await?;
        let mut items = std::mem::take(&mut page.value);
        while let Some(link) = page.next_link.take() {
            let mut next = client.follow::<T>(&link);
            next.request.prefer.clone_from(&prefer);
            page = next.await?;
            items.append(&mut page.value);
        }
        page.value = items;
        Ok(page)
    }
}

impl<'a, T: Decode + Send + 'a> IntoFuture for Call<'a, T> {
    type Output = Result<T, Error>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            let answer = self.client.send(&self.request, T::throttled).await?;
            T::decode(answer)
        })
    }
}

/// A binary response: a message's MIME, an attachment's or a photo's bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Media {
    /// The `Content-Type` Graph labelled it with.
    pub content_type: Option<String>,
    /// The bytes.
    pub data: Vec<u8>,
}

/// Leaves the bytes out.
impl std::fmt::Debug for Media {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Media")
            .field("content_type", &self.content_type)
            .field("len", &self.data.len())
            .finish()
    }
}

/// How responses turn into results. The module is private, so no other crate can name or
/// implement the trait.
mod response {
    use super::*;

    /// A response, read in full.
    pub struct Answer {
        pub status: StatusCode,
        pub content_type: Option<String>,
        pub retry_after: Option<Duration>,
        pub body: Vec<u8>,
    }

    impl Answer {
        pub(crate) async fn read(response: reqwest::Response) -> Result<Self, Error> {
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let retry_after = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(retry_after);
            Ok(Self {
                status: response.status(),
                content_type,
                retry_after,
                body: response.bytes().await?.into(),
            })
        }

        /// What Graph said went wrong.
        pub(crate) fn error(&self) -> Error {
            match serde_json::from_slice::<ErrorResponse>(&self.body) {
                Ok(response) => Error::Api(ApiError {
                    status: self.status.as_u16(),
                    error: response.error,
                    retry_after: self.retry_after,
                }),
                Err(_) => self.decode_error("not a Graph error body"),
            }
        }

        pub(crate) fn decode_error(&self, reason: impl std::fmt::Display) -> Error {
            Error::Decode {
                status: self.status.as_u16(),
                reason: format!(
                    "{reason}; body starts with {:?}",
                    String::from_utf8_lossy(&self.body[..self.body.len().min(200)])
                ),
            }
        }
    }

    /// What a call's response decodes to: JSON types, or [`Media`].
    pub trait Decode: Sized {
        /// The result `answer` stands for.
        fn decode(answer: Answer) -> Result<Self, Error>;

        /// Whether `answer` is Graph turning the request away for now.
        fn throttled(answer: &Answer) -> bool {
            matches!(
                answer.status,
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
            )
        }
    }

    impl<T: DeserializeOwned> Decode for T {
        fn decode(answer: Answer) -> Result<Self, Error> {
            if !answer.status.is_success() {
                return Err(answer.error());
            }
            parse(&answer.body).map_err(|error| answer.decode_error(error))
        }
    }

    impl Media {
        /// Outlook has been seen to answer a download with 200 and a JSON `ApplicationThrottled`
        /// body, which would otherwise be mistaken for the downloaded file.
        fn throttled_in_200(answer: &Answer) -> Option<ErrorBody> {
            let json = answer
                .content_type
                .as_deref()
                .and_then(|content_type| content_type.split(';').next())
                .is_some_and(|media_type| {
                    media_type.trim().eq_ignore_ascii_case("application/json")
                });
            if !answer.status.is_success() || !json {
                return None;
            }
            serde_json::from_slice::<ErrorResponse>(&answer.body)
                .ok()
                .map(|response| response.error)
                .filter(|error| error.code == "ApplicationThrottled")
        }
    }

    impl Decode for Media {
        fn decode(answer: Answer) -> Result<Self, Error> {
            if !answer.status.is_success() {
                return Err(answer.error());
            }
            if let Some(error) = Self::throttled_in_200(&answer) {
                return Err(Error::Api(ApiError {
                    status: StatusCode::TOO_MANY_REQUESTS.as_u16(),
                    error,
                    retry_after: answer.retry_after,
                }));
            }
            Ok(Self {
                content_type: answer.content_type,
                data: answer.body,
            })
        }

        fn throttled(answer: &Answer) -> bool {
            matches!(
                answer.status,
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
            ) || Self::throttled_in_200(answer).is_some()
        }
    }
}

/// A successful response's body as `T`.
pub(crate) fn parse<T: DeserializeOwned>(body: &[u8]) -> serde_json::Result<T> {
    // Sends and deletes answer 202 or 204 with no body, and `()` deserializes from `null`.
    let body = if body.iter().all(u8::is_ascii_whitespace) {
        b"null".as_slice()
    } else {
        body
    };
    serde_json::from_slice(body)
}

/// `Retry-After` in seconds, the form Graph sends.
pub(crate) fn retry_after(value: &str) -> Option<Duration> {
    value.trim().parse().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(status: u16, content_type: &str, body: &str) -> Answer {
        Answer {
            status: StatusCode::from_u16(status).unwrap(),
            content_type: Some(content_type.into()),
            retry_after: None,
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn a_throttled_download_is_not_the_file() {
        let throttled = r#"{"error":{"code":"ApplicationThrottled","message":"Application is over its MailboxConcurrency limit."}}"#;
        assert!(<Media as Decode>::throttled(&answer(
            200,
            "Application/JSON; charset=utf-8",
            throttled
        )));
        let mut long_wait = answer(200, "Application/JSON; charset=utf-8", throttled);
        long_wait.retry_after = Some(Duration::from_secs(120));
        let error = <Media as Decode>::decode(long_wait).unwrap_err();
        let Error::Api(api) = error else {
            panic!("expected Api error, got {error:?}");
        };
        assert_eq!(api.status, 429);
        assert_eq!(api.error.code, "ApplicationThrottled");
        assert_eq!(api.retry_after, Some(Duration::from_secs(120)));

        assert!(!<Media as Decode>::throttled(&answer(
            200,
            "message/rfc822",
            throttled
        )));
        assert!(<Media as Decode>::throttled(&answer(503, "text/plain", "")));
        // JSON calls decode such a body as an error already.
        assert!(!<serde_json::Value as Decode>::throttled(&answer(
            200,
            "application/json",
            throttled
        )));
    }

    #[test]
    fn retry_after_is_in_seconds() {
        assert_eq!(retry_after(" 10"), Some(Duration::from_secs(10)));
        assert_eq!(retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    }

    #[test]
    fn every_request_prefers_immutable_ids() {
        let client = Client::new(Endpoints::microsoft(), StaticToken("t".into()));
        let call = client
            .call::<()>(Method::GET, ["me", "messages"])
            .max_page_size(25)
            .text_body()
            .timezone("America/New_York");
        assert_eq!(
            call.request.prefer_header(),
            "IdType=\"ImmutableId\", odata.maxpagesize=25, outlook.body-content-type=\"text\", outlook.timezone=\"America/New_York\""
        );
        let token = StaticToken("secret".into());
        assert!(!format!("{token:?}").contains("secret"));
    }

    #[test]
    fn timezone_validates_prefer_header_value() {
        let client = Client::new(Endpoints::microsoft(), StaticToken("t".into()));
        for valid in [
            "UTC",
            "America/New_York",
            "Etc/GMT+5",
            "W. Europe Standard Time",
            "  Eastern Standard Time  ",
        ] {
            let call = client
                .call::<()>(Method::GET, ["me", "calendarView"])
                .timezone(valid);
            assert!(
                call.request
                    .prefer_header()
                    .contains(&format!("outlook.timezone=\"{}\"", valid.trim())),
                "expected valid zone {valid:?} in {:?}",
                call.request.prefer_header()
            );
        }
        for invalid in [
            "",
            "   ",
            "UTC\", IdType=\"x",
            "UTC\r\nX-Injected: 1",
            "UTC\n",
            "UTC, respond-async",
            "UTC;foo=bar",
            "UTC\\x",
            "UTC\0",
            "Europe/Zürich",
        ] {
            let call = client
                .call::<()>(Method::GET, ["me", "calendarView"])
                .timezone(invalid);
            assert_eq!(
                call.request.prefer_header(),
                "IdType=\"ImmutableId\"",
                "expected invalid zone {invalid:?} to be omitted"
            );
        }
    }
}
