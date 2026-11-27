//! Every provider call goes through [`Calls::retry`], which applies one retry policy for sync
//! and the outbox. It backs off with jitter on network errors, 5xx and rate limits, honors the
//! provider's retry hints, and refreshes the token once on 401. Before it goes out, a call spends
//! its quota units from the account's [`Budget`], which paces calls under the provider's quota and
//! holds them while the provider is rate limiting the account. How each call ends tells the
//! account's [`Network`] what's down and what's back. Gmail's calls are [`Api::call`], which reads
//! Gmail's errors (`Precondition check failed` is worth retrying, for one).

use std::future::IntoFuture;
use std::path::Path;
use std::time::Duration;

use gmail::{Call, Client, Gmail};
use jiff::{SignedDuration, Timestamp};
use serde::de::DeserializeOwned;

use crate::connection::{Connection, Debounce, Network, Outage};
use crate::error::{Error, ProviderError};
use crate::quota::{Budget, Lane, MAX_WAIT, Quota};

pub(crate) struct Api {
    client: Client,
    calls: Calls,
}

/// An account's retry policy, quota and connection, which every call to its provider goes
/// through.
pub(crate) struct Calls {
    /// The provider, as the log names it.
    service: &'static str,
    /// The first retry's delay. Later ones double.
    base: Duration,
    budget: Budget,
    network: Network,
}

/// How a call failed, once the policy gave up on it.
#[derive(Debug)]
pub(crate) enum Failure {
    /// No response at all.
    Offline(ProviderError),
    /// The provider is rate limiting the account and takes calls again at this time, which is
    /// later than a call waits.
    Throttled(Timestamp),
    /// Gmail won't send mail for the account until this time (its sending limit,
    /// `(Mail sending)`), which is later than a call waits. Only sending is held, and the
    /// account's other calls go on.
    SendingLimited(Timestamp),
    /// 401 even with a fresh token.
    Unauthorized(ProviderError),
    /// Worth retrying later (5xx and the like), but retries ran out.
    Transient(ProviderError),
    /// Retrying won't help (4xx).
    Permanent(ProviderError),
}

impl Failure {
    /// The provider said the resource doesn't exist (404; Gmail's 400 `Invalid id value`,
    /// Graph's 400 `ErrorInvalidIdMalformed`).
    pub fn is_not_found(&self) -> bool {
        match self {
            Self::Permanent(ProviderError::Gmail(error)) => {
                error.is_not_found()
                    || matches!(error, gmail::Error::Api(api) if api.message.contains("Invalid id value"))
            }
            Self::Permanent(ProviderError::Outlook(error)) => {
                error.is_not_found() || error.code() == Some("ErrorInvalidIdMalformed")
            }
            _ => false,
        }
    }

    /// The provider refused the call in a way retrying won't change (a 4xx other than rate
    /// limits and credentials).
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent(_))
    }

    /// Gmail won't send what was asked for in one answer (a thread too large for `threads.get`,
    /// 500 `responseTooLarge`).
    pub fn is_too_large(&self) -> bool {
        let Self::Permanent(ProviderError::Gmail(gmail::Error::Api(api))) = self else {
            return false;
        };
        too_large(api)
    }

    /// The provider's error message, for logs and failure reports.
    pub fn message(&self) -> String {
        match self {
            Self::Throttled(until) => format!("rate limited until {until}"),
            Self::SendingLimited(until) => format!("Gmail's sending limit until {until}"),
            Self::Offline(error)
            | Self::Unauthorized(error)
            | Self::Transient(error)
            | Self::Permanent(error) => error.to_string(),
        }
    }
}

impl From<Failure> for Error {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Offline(error) => Self::Offline(error),
            Failure::Throttled(until) | Failure::SendingLimited(until) => Self::Throttled { until },
            Failure::Unauthorized(error) => Self::Unauthorized(error),
            Failure::Transient(error) | Failure::Permanent(error) => error.into(),
        }
    }
}

/// What a provider's error says about retrying the call.
pub(crate) enum Kind {
    Network,
    /// Rate limited, asked to wait `after`. A `sending` limit holds only sending.
    RateLimited {
        after: Option<Duration>,
        sending: bool,
    },
    Unauthorized,
    Transient,
    Permanent,
}

impl Calls {
    /// The policy for `service`'s calls, paced by `budget`, for a store in `dir` whose outages
    /// end as `debounce` says.
    pub fn new(
        service: &'static str,
        base: Duration,
        budget: Budget,
        debounce: Debounce,
        dir: &Path,
    ) -> Self {
        Self {
            service,
            base,
            budget,
            network: Network::new(debounce, dir),
        }
    }

    /// The connection to the provider, as calls have found it.
    pub fn connection(&self) -> tokio::sync::watch::Receiver<Connection> {
        self.network.subscribe()
    }

    /// Something other than a call found `outage`.
    pub fn down(&self, outage: Outage) {
        self.network.down(outage);
    }

    /// A call got through: a mail call (`mail`) answers for every service, another for the
    /// network and sign-in alone.
    pub fn reached(&self, mail: bool) {
        self.network.reached(mail);
    }

    /// Pauses the account's budget for `wait` after an inner batch item was rate limited.
    pub fn pushback(&self, wait: Duration) -> Timestamp {
        self.budget.pushback(wait)
    }

    /// Runs the call `make` builds, trying up to `attempts` times, each spending `units` of it
    /// from the budget in `lane` (no lane spends nothing, for APIs whose quotas are their own).
    /// `classify` reads the provider's errors.
    pub async fn retry<C, T, E>(
        &self,
        lane: Option<Lane>,
        attempts: u32,
        mut make: impl FnMut() -> C,
        units: impl Fn(&C) -> u32,
        classify: impl Fn(&E) -> Kind,
    ) -> Result<T, Failure>
    where
        C: IntoFuture<Output = Result<T, E>>,
        E: Into<ProviderError> + std::fmt::Display,
    {
        let service = self.service;
        let mut refreshed = false;
        let mut attempt = 0;
        loop {
            let call = make();
            // Held until the call ends, when the budget caps the calls in flight.
            let _slot = match lane {
                Some(lane) => Some(
                    self.budget
                        .spend(lane, units(&call))
                        .await
                        .map_err(Failure::Throttled)?,
                ),
                None => None,
            };
            attempt += 1;
            let error = match call.await {
                Ok(value) => {
                    if lane.is_some() {
                        self.budget.settled();
                    }
                    return Ok(value);
                }
                Err(error) => error,
            };
            let wait = match classify(&error) {
                // The client has handed the rejected token back, so the retry gets a fresh one.
                Kind::Unauthorized if !refreshed => {
                    log::info!("{service} rejected the token; retrying once with a fresh one");
                    refreshed = true;
                    attempt -= 1;
                    continue;
                }
                Kind::Unauthorized => return Err(Failure::Unauthorized(error.into())),
                Kind::Permanent => return Err(Failure::Permanent(error.into())),
                Kind::RateLimited { after, sending } => {
                    let wait = after.unwrap_or_else(|| self.delay(attempt));
                    // A sending limit holds only sending, for hours at a time, and pauses nothing
                    // else.
                    if lane.is_some() && !sending {
                        // The retry, like every other call, waits for the pause in `spend`.
                        let until = self.budget.pushback(wait);
                        if attempt >= attempts {
                            return Err(Failure::Throttled(until));
                        }
                        log::debug!(
                            "{service} rate limited a call ({error}); retrying after {until}"
                        );
                        continue;
                    }
                    if wait > MAX_WAIT || attempt >= attempts {
                        return Err(if sending {
                            Failure::SendingLimited(from_now(wait))
                        } else {
                            Failure::Throttled(from_now(wait))
                        });
                    }
                    wait
                }
                Kind::Network if attempt >= attempts => {
                    return Err(Failure::Offline(error.into()));
                }
                Kind::Transient if attempt >= attempts => {
                    return Err(Failure::Transient(error.into()));
                }
                Kind::Network | Kind::Transient => self.delay(attempt),
            };
            log::debug!("{service} call failed ({error}); retrying in {wait:?}");
            tokio::time::sleep(wait).await;
        }
    }

    /// `min(32 s, base × 2^(n−1) + jitter(0..base))`.
    fn delay(&self, attempt: u32) -> Duration {
        let exponential = self
            .base
            .saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1).min(16)));
        let jitter = self.base.mul_f64(random_unit());
        (exponential + jitter).min(MAX_WAIT)
    }
}

impl Api {
    /// Calls through `client`, paced under `quota` (`None` paces nothing), for a store in `dir`
    /// whose outages end as `debounce` says.
    pub fn new(
        client: Client,
        base: Duration,
        quota: Option<Quota>,
        debounce: Debounce,
        dir: &Path,
    ) -> Self {
        Self {
            client,
            calls: Calls::new("Gmail", base, Budget::new("Gmail", quota), debounce, dir),
        }
    }

    /// The connection to Gmail, as calls have found it.
    pub fn connection(&self) -> tokio::sync::watch::Receiver<Connection> {
        self.calls.connection()
    }

    /// Asks for Gmail's profile, once (`users.getProfile`, 1 unit).
    pub async fn ping(&self) -> Result<(), Failure> {
        self.call(Lane::Act, 1, |gmail| gmail.profile())
            .await
            .map(drop)
    }

    /// Something other than a Gmail call found `outage`.
    pub fn down(&self, outage: Outage) {
        self.calls.down(outage);
    }

    /// Another API on the same account answered (`mail == false`), clearing network/auth outages.
    pub fn reached(&self, mail: bool) {
        self.calls.reached(mail);
    }

    /// Runs the Gmail call `make` builds for `lane`, trying up to `attempts` times. Sync passes
    /// a handful, and the outbox passes 1 and retries at its own pace.
    pub async fn call<'a, T: DeserializeOwned + 'a>(
        &'a self,
        lane: Lane,
        attempts: u32,
        mut make: impl FnMut(Gmail<'a>) -> Call<'a, T>,
    ) -> Result<T, Failure> {
        self.send(&self.client, Some(lane), attempts, |client| {
            make(client.gmail())
        })
        .await
    }

    /// [`call`](Self::call) for Google's other APIs (Calendar, People), whose quotas are their
    /// own, so Gmail's quota neither paces nor pauses them.
    pub async fn request<'a, T: DeserializeOwned + 'a>(
        &'a self,
        attempts: u32,
        make: impl FnMut(&'a Client) -> Call<'a, T>,
    ) -> Result<T, Failure> {
        self.send(&self.client, None, attempts, make).await
    }

    /// Runs the call through `client`, and tells the connection how it ended. A Gmail call
    /// (`lane` set) reports on every service, and another API's call reports only on the network
    /// and Google's sign-in.
    async fn send<'a, T: DeserializeOwned + 'a>(
        &'a self,
        client: &'a Client,
        lane: Option<Lane>,
        attempts: u32,
        mut make: impl FnMut(&'a Client) -> Call<'a, T>,
    ) -> Result<T, Failure> {
        let gmail = lane.is_some();
        let result = self
            .calls
            .retry(lane, attempts, || make(client), Call::units, classify)
            .await;
        match &result {
            Ok(_) => self.calls.reached(gmail),
            Err(failure) => {
                if let Some(outage) = self.outage(failure, gmail) {
                    self.calls.down(outage);
                }
            }
        }
        result
    }

    /// What a call that finally failed says is down. A 5xx that outlasted its retries got an
    /// answer, so it says nothing. Calendar and People that get no answer say the network is
    /// down. Their rate limits are their own, not Gmail's throttling.
    fn outage(&self, failure: &Failure, gmail: bool) -> Option<Outage> {
        match failure {
            Failure::Offline(ProviderError::Gmail(gmail::Error::Http(http)))
                if http.url() == Some(&self.client.endpoints().oauth.token) =>
            {
                Some(Outage::Auth)
            }
            Failure::Offline(_) if gmail => Some(Outage::Gmail),
            Failure::Offline(_) => Some(Outage::Offline),
            Failure::Throttled(_) if gmail => Some(Outage::Throttled),
            Failure::Permanent(ProviderError::Gmail(gmail::Error::Api(api)))
                if gmail && api.code == 400 && mentions(api, "Mail service not enabled") =>
            {
                Some(Outage::GmailDisabled)
            }
            _ => None,
        }
    }
}

pub(crate) fn classify(error: &gmail::Error) -> Kind {
    match error {
        gmail::Error::Http(http) => match http.status() {
            None => Kind::Network,
            Some(status) if status.is_server_error() => Kind::Transient,
            Some(_) => Kind::Permanent,
        },
        gmail::Error::Api(api) => {
            // These count anywhere in the body, so `rateLimitExceeded` also finds
            // `userRateLimitExceeded`.
            let says = |needle: &str| mentions(api, needle);
            match api.code {
                401 => Kind::Unauthorized,
                429 => rate_limited(api),
                403 if says("rateLimitExceeded") || says("Quota exceeded") => rate_limited(api),
                503 if says("CommandConcurrencyLimitReached") => rate_limited(api),
                // The thread is as large every time, so the caller asks for it another way.
                _ if too_large(api) => Kind::Permanent,
                500..=599 => Kind::Transient,
                400 if api.message.contains("Precondition check failed") => Kind::Transient,
                _ => Kind::Permanent,
            }
        }
        gmail::Error::Decode { status, .. } if *status >= 500 => Kind::Transient,
        gmail::Error::Token(_) | gmail::Error::OAuth(_) => Kind::Unauthorized,
        gmail::Error::Decode { .. } | gmail::Error::Batch(_) => Kind::Permanent,
    }
}

/// Gmail rate limiting a call, as long as it asks. Its daily sending limit holds only sending:
/// for hours, Gmail answers sends with a rate limit "mentioning `(Mail sending)`" (the Gmail API's
/// "Resolve errors", 429), while the account's other calls still go.
fn rate_limited(api: &gmail::ApiError) -> Kind {
    Kind::RateLimited {
        after: retry_after(api),
        sending: mentions(api, "(Mail sending)"),
    }
}

/// Gmail's 500 for an answer too large to send (`responseTooLarge`).
fn too_large(api: &gmail::ApiError) -> bool {
    api.code == 500 && mentions(api, "responseTooLarge")
}

/// Whether Google's error body says `needle` anywhere: message, reasons or details.
fn mentions(api: &gmail::ApiError, needle: &str) -> bool {
    api.message.contains(needle)
        || api
            .errors
            .iter()
            .any(|item| item.reason.contains(needle) || item.message.contains(needle))
        || api
            .details
            .iter()
            .any(|detail| detail.to_string().contains(needle))
}

/// How long Gmail asks to wait: the `Retry after <RFC 3339>` it writes into rate-limit
/// messages, or the `retryDelay` of a `google.rpc.RetryInfo` detail (`"12s"`). Google's APIs put
/// retry hints in the error body rather than a `Retry-After` header.
fn retry_after(api: &gmail::ApiError) -> Option<Duration> {
    let in_message = || {
        let (_, rest) = api.message.split_once("Retry after ")?;
        let until: Timestamp = rest.split_whitespace().next()?.parse().ok()?;
        Duration::try_from(until.duration_since(Timestamp::now())).ok()
    };
    let in_details = || {
        api.details.iter().find_map(|detail| {
            let kind = detail.get("@type")?.as_str()?;
            if !kind.ends_with("google.rpc.RetryInfo") {
                return None;
            }
            let delay = detail.get("retryDelay")?.as_str()?.strip_suffix('s')?;
            Duration::try_from_secs_f64(delay.parse().ok()?).ok()
        })
    };
    in_message().or_else(in_details)
}

/// `wait` from now, or the end of time.
pub(crate) fn from_now(wait: Duration) -> Timestamp {
    Timestamp::now()
        .checked_add(SignedDuration::try_from(wait).unwrap_or(SignedDuration::MAX))
        .unwrap_or(Timestamp::MAX)
}

/// A uniform sample from `[0, 1)`.
fn random_unit() -> f64 {
    let bits = getrandom::u64().unwrap_or_default() >> 11;
    bits as f64 / (1u64 << 53) as f64
}
