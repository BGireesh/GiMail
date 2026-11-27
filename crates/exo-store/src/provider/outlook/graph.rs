//! Graph's calls for the account, through the store's retry policy ([`Calls`]). The client sends
//! at most four requests at once, Outlook's limit per mailbox, and the budget has as many slots,
//! so calls take them in lane order and a click never waits behind sync in the client's queue. A
//! 429 or 503 the client couldn't wait out pauses every call, as Gmail's rate limits do.

use std::future::IntoFuture;
use std::path::Path;
use std::time::Duration;

use outlook::{Call, Client, MAX_BATCH, Order};
use serde::de::DeserializeOwned;

use crate::api::{Calls, Failure, Kind, from_now};
use crate::connection::{Connection, Debounce, Outage};
use crate::error::ProviderError;
use crate::quota::{Budget, Lane, Quota};

/// The account's Graph client, and the policy its calls go through.
pub(crate) struct Graph {
    client: Client,
    calls: Calls,
}

/// A call ready to go, and the units it spends: one per request, a batch's every request.
struct Priced<F> {
    units: u32,
    call: F,
}

impl<F: IntoFuture> IntoFuture for Priced<F> {
    type Output = F::Output;
    type IntoFuture = F::IntoFuture;

    fn into_future(self) -> Self::IntoFuture {
        self.call.into_future()
    }
}

impl Graph {
    /// Calls through `client`, paced under `quota` (`None` paces nothing), for a store in `dir`
    /// whose outages end as `debounce` says.
    pub fn new(
        client: Client,
        base: Duration,
        quota: Option<Quota>,
        debounce: Debounce,
        dir: &Path,
    ) -> Self {
        let budget = Budget::new("Outlook", quota).with_slots(outlook::MAX_CONCURRENT);
        Self {
            client,
            calls: Calls::new("Outlook", base, budget, debounce, dir),
        }
    }

    /// The connection to Graph, as calls have found it.
    pub fn connection(&self) -> tokio::sync::watch::Receiver<Connection> {
        self.calls.connection()
    }

    /// Something other than a Graph call found `outage`.
    pub fn down(&self, outage: Outage) {
        self.calls.down(outage);
    }

    /// Another API on the same account answered (`mail == false`), clearing network/auth outages.
    pub fn reached(&self, mail: bool) {
        self.calls.reached(mail);
    }

    /// Runs the call `make` builds for `lane`, trying up to `attempts` times. The call may be a
    /// collection's every page ([`Call::all`]), which goes as one.
    pub async fn call<'a, C, T>(
        &'a self,
        lane: Lane,
        attempts: u32,
        mut make: impl FnMut(&'a Client) -> C,
    ) -> Result<T, Failure>
    where
        C: IntoFuture<Output = Result<T, outlook::Error>>,
    {
        let result = self
            .calls
            .retry(
                Some(lane),
                attempts,
                || Priced {
                    units: 1,
                    call: make(&self.client),
                },
                |priced| priced.units,
                classify,
            )
            .await;
        self.report(&result);
        result
    }

    /// The `count` calls `make` builds, by index, in `$batch`es of up to [`MAX_BATCH`]. Each batch
    /// is a chain, which the mailbox serves one request at a time, as one of its slots. A call
    /// that didn't run because one before it in its chain failed (424) goes in the next batch.
    /// Each call's own outcome comes back in order; the batch's own failure fails them all.
    pub async fn each<'a, T: DeserializeOwned + 'a>(
        &'a self,
        lane: Lane,
        attempts: u32,
        count: usize,
        make: impl Fn(&'a Client, usize) -> Call<'a, T>,
    ) -> Result<Vec<Result<T, outlook::Error>>, Failure> {
        let mut results: Vec<Option<Result<T, outlook::Error>>> =
            std::iter::repeat_with(|| None).take(count).collect();
        let mut pending: Vec<usize> = (0..count).collect();
        let mut refreshed = false;
        while !pending.is_empty() {
            let chunk: Vec<usize> = pending.drain(..pending.len().min(MAX_BATCH)).collect();
            let chunk_len = chunk.len();
            let units = u32::try_from(chunk_len).unwrap_or(u32::MAX);
            let answered = self
                .calls
                .retry(
                    Some(lane),
                    attempts,
                    || Priced {
                        units,
                        call: self.client.batch(
                            chunk
                                .iter()
                                .map(|&index| make(&self.client, index))
                                .collect(),
                            Order::Sequential,
                        ),
                    },
                    |priced| priced.units,
                    classify,
                )
                .await;
            let batch = match answered {
                Ok(batch) => batch,
                Err(failure) => {
                    if let Some(outage) = self.outage(&failure) {
                        self.calls.down(outage);
                    }
                    return Err(failure);
                }
            };
            let mut again: Vec<(usize, outlook::Error)> = Vec::new();
            let mut answered_items = Vec::with_capacity(chunk_len);
            let mut retry_unauthorized = None;
            let mut iter = chunk.into_iter().zip(batch);
            while let Some((index, result)) = iter.next() {
                match result {
                    Err(error) => match classify(&error) {
                        Kind::Unauthorized if !refreshed => {
                            log::info!(
                                "Outlook rejected the token in a batch; retrying remaining items once"
                            );
                            refreshed = true;
                            let mut rest: Vec<usize> = again.drain(..).map(|(i, _)| i).collect();
                            rest.push(index);
                            rest.extend(iter.map(|(i, _)| i));
                            retry_unauthorized = Some(rest);
                            break;
                        }
                        Kind::Unauthorized => return Err(Failure::Unauthorized(error.into())),
                        Kind::RateLimited { after, .. } => {
                            let until =
                                self.calls.pushback(after.unwrap_or(Duration::from_secs(1)));
                            let failure = Failure::Throttled(until);
                            if let Some(outage) = self.outage(&failure) {
                                self.calls.down(outage);
                            }
                            return Err(failure);
                        }
                        _ if error.status() == Some(424) => again.push((index, error)),
                        _ => answered_items.push((index, Err(error))),
                    },
                    Ok(value) => answered_items.push((index, Ok(value))),
                }
            }
            if let Some(mut rest) = retry_unauthorized {
                for (index, result) in answered_items {
                    results[index] = Some(result);
                }
                rest.append(&mut pending);
                pending = rest;
                continue;
            }
            self.calls.reached(true);
            if again.len() == chunk_len {
                let (_, error) = again.into_iter().next().expect("chunk is non-empty");
                return Err(Failure::Transient(error.into()));
            }
            for (index, result) in answered_items {
                results[index] = Some(result);
            }
            let mut next: Vec<usize> = again.into_iter().map(|(index, _)| index).collect();
            next.append(&mut pending);
            pending = next;
        }
        Ok(results
            .into_iter()
            .map(|result| result.expect("every call ends answered"))
            .collect())
    }

    /// Tells the connection how a call ended.
    fn report<T>(&self, result: &Result<T, Failure>) {
        match result {
            Ok(_) => self.calls.reached(true),
            Err(failure) => {
                if let Some(outage) = self.outage(failure) {
                    self.calls.down(outage);
                }
            }
        }
    }

    /// What a call that finally failed says is down. A 5xx that outlasted its retries got an
    /// answer, so it says nothing.
    fn outage(&self, failure: &Failure) -> Option<Outage> {
        match failure {
            Failure::Offline(ProviderError::Outlook(outlook::Error::Http(http)))
                if http.url() == Some(&self.client.endpoints().identity.token) =>
            {
                Some(Outage::Auth)
            }
            Failure::Offline(_) => Some(Outage::Gmail),
            Failure::Throttled(_) => Some(Outage::Throttled),
            Failure::Permanent(ProviderError::Outlook(error))
                if error.code() == Some("MailboxNotEnabledForRESTAPI") =>
            {
                Some(Outage::GmailDisabled)
            }
            _ => None,
        }
    }
}

/// How one call of a batch failed, as [`Graph::call`] would have given up on it.
pub(super) fn failed(error: outlook::Error) -> Failure {
    match classify(&error) {
        Kind::Network => Failure::Offline(error.into()),
        Kind::Unauthorized => Failure::Unauthorized(error.into()),
        Kind::RateLimited { after, .. } => {
            Failure::Throttled(from_now(after.unwrap_or(Duration::from_secs(1))))
        }
        Kind::Transient => Failure::Transient(error.into()),
        Kind::Permanent => Failure::Permanent(error.into()),
    }
}

/// What Graph's error says about retrying. The client has already waited out the throttling it
/// could, so a 429 or 503 that comes back pauses the account.
pub(crate) fn classify(error: &outlook::Error) -> Kind {
    match error {
        outlook::Error::Http(http) => match http.status() {
            None => Kind::Network,
            Some(status) if status.is_server_error() => Kind::Transient,
            Some(_) => Kind::Permanent,
        },
        outlook::Error::Api(api) => match api.status {
            401 => Kind::Unauthorized,
            429 | 503 => Kind::RateLimited {
                after: api.retry_after,
                sending: false,
            },
            500..=599 => Kind::Transient,
            _ => Kind::Permanent,
        },
        outlook::Error::Decode { status, .. } if *status >= 500 => Kind::Transient,
        outlook::Error::Token(_) | outlook::Error::Identity(_) => Kind::Unauthorized,
        outlook::Error::Decode { .. }
        | outlook::Error::Batch(_)
        | outlook::Error::ForeignLink(_) => Kind::Permanent,
    }
}
