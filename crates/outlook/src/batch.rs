//! JSON batching (learn.microsoft.com/graph/json-batching): up to 20 requests in one round trip,
//! each answered with its own status inside the batch's 200.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use crate::client::{Body, Call, Client, Decode, Request, retry_after};
use crate::endpoints::VERSION;
use crate::error::{ApiError, Error, ErrorResponse};

/// Most requests Graph takes in one batch.
pub const MAX_BATCH: usize = 20;

/// How many times a batch's throttled requests are sent again, as for single requests.
const RETRIES: u32 = 3;

/// The longest `Retry-After` waited out before sending throttled requests again.
const LONGEST_WAIT: Duration = Duration::from_secs(60);

/// How a batch's requests run. Graph runs a batch either fully in parallel or as one chain
/// (learn.microsoft.com/graph/json-batching).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Independently, in any order.
    Parallel,
    /// One at a time, in the order given: each request `dependsOn` the one before, and once one
    /// fails, the rest fail with 424 without running.
    Sequential,
}

#[derive(Serialize)]
struct Batch<'a> {
    requests: Vec<Item<'a>>,
}

/// One request of a batch, as Graph takes it.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Item<'a> {
    id: String,
    method: &'a str,
    /// The URL under `/v1.0`, e.g. `/me/messages/{id}?$select=subject`.
    url: String,
    /// Batched requests don't inherit the batch's headers.
    headers: BTreeMap<&'static str, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a RawValue>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
}

#[derive(Deserialize)]
struct Answers {
    responses: Vec<Answer>,
}

/// One request's answer.
#[derive(Deserialize)]
struct Answer {
    id: String,
    status: u16,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    body: Option<Value>,
}

impl Answer {
    fn throttled(&self) -> bool {
        matches!(
            StatusCode::from_u16(self.status),
            Ok(StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE)
        )
    }

    /// `Retry-After` from the answer's own headers.
    fn retry_after(&self) -> Option<Duration> {
        self.headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
            .and_then(|(_, value)| retry_after(value))
    }

    fn result<T: DeserializeOwned>(self) -> Result<T, Error> {
        let retry_after = self.retry_after();
        let body = self.body.unwrap_or(Value::Null);
        if (200..300).contains(&self.status) {
            return serde_json::from_value(body).map_err(|error| Error::Decode {
                status: self.status,
                reason: format!("batched response: {error}"),
            });
        }
        match serde_json::from_value::<ErrorResponse>(body) {
            Ok(response) => Err(Error::Api(ApiError {
                status: self.status,
                error: response.error,
                retry_after,
            })),
            Err(_) => Err(Error::Decode {
                status: self.status,
                reason: "batched response without a Graph error body".into(),
            }),
        }
    }
}

impl Client {
    /// Runs up to [`MAX_BATCH`] calls in one request (`POST /v1.0/$batch`), in `order`.
    ///
    /// Each request is throttled on its own: one Graph turned away (429 or 503 inside the
    /// batch's 200) is sent again after its `Retry-After`, in a new batch with the requests
    /// that depend on it, as single requests are. The outer `Result` fails when the batch
    /// itself can't be sent or read. Each inner one is that call's own outcome, in the order
    /// given. MIME bodies can't be batched.
    ///
    /// # Errors
    /// [`Error::Batch`] for too many calls, a MIME body or a malformed response, and the
    /// batch's own HTTP and API errors.
    pub async fn batch<T: DeserializeOwned>(
        &self,
        calls: Vec<Call<'_, T>>,
        order: Order,
    ) -> Result<Vec<Result<T, Error>>, Error> {
        if calls.len() > MAX_BATCH {
            return Err(Error::Batch(format!(
                "{} calls; Graph takes at most {MAX_BATCH}",
                calls.len()
            )));
        }
        let items = calls
            .iter()
            .enumerate()
            .map(|(index, call)| self.item(index, &call.request))
            .collect::<Result<Vec<_>, _>>()?;
        let mut results: Vec<Option<Result<T, Error>>> = calls.iter().map(|_| None).collect();
        let mut pending: Vec<usize> = (0..calls.len()).collect();
        let mut retries = 0;
        while !pending.is_empty() {
            let requests = pending
                .iter()
                .enumerate()
                .map(|(position, &index)| {
                    let mut item = items[index].clone();
                    if order == Order::Sequential && position > 0 {
                        item.depends_on = vec![pending[position - 1].to_string()];
                    }
                    item
                })
                .collect();
            let mut answers = self.send_batch(&Batch { requests }).await?;
            let mut answered: Vec<(usize, Option<Answer>)> = pending
                .iter()
                .map(|&index| (index, answers.remove(&index.to_string())))
                .collect();

            // What's sent again: the throttled requests, and in a chain everything after the
            // first of them, which failed only because it did.
            let throttled =
                |answer: &Option<Answer>| answer.as_ref().is_some_and(Answer::throttled);
            let again: Vec<usize> = match order {
                Order::Parallel => answered
                    .iter()
                    .filter(|(_, answer)| throttled(answer))
                    .map(|(index, _)| *index)
                    .collect(),
                Order::Sequential => answered
                    .iter()
                    .position(|(_, answer)| throttled(answer))
                    .map(|first| answered[first..].iter().map(|(index, _)| *index).collect())
                    .unwrap_or_default(),
            };
            let wait = answered
                .iter()
                .filter(|(_, answer)| throttled(answer))
                .filter_map(|(_, answer)| answer.as_ref()?.retry_after())
                .max()
                .unwrap_or(Duration::from_secs(1 << retries));
            let retrying = !again.is_empty() && retries < RETRIES && wait <= LONGEST_WAIT;
            for (index, answer) in answered.iter_mut() {
                if retrying && again.contains(index) {
                    continue;
                }
                results[*index] = Some(match answer.take() {
                    Some(answer) => answer.result(),
                    None => Err(Error::Batch(format!("no response for request {index}"))),
                });
            }
            if !retrying {
                break;
            }
            log::info!(
                "Graph throttled {} batched requests, sending them again in {wait:?}",
                again.len()
            );
            tokio::time::sleep(wait).await;
            retries += 1;
            pending = again;
        }
        Ok(results
            .into_iter()
            .map(|result| result.expect("every request ends answered or failed"))
            .collect())
    }

    /// Request `index` as a batch item.
    fn item<'r>(&self, index: usize, request: &'r Request) -> Result<Item<'r>, Error> {
        if !self.endpoints.is_graph(&request.url) {
            return Err(Error::ForeignLink(request.url.clone()));
        }
        let path = request
            .url
            .path()
            .strip_prefix(&format!("/{VERSION}"))
            .unwrap_or_default();
        let url = match request.url.query() {
            Some(query) => format!("{path}?{query}"),
            None => path.to_owned(),
        };
        let mut headers = BTreeMap::from([("Prefer", request.prefer_header())]);
        let body = match &request.body {
            Body::Empty => None,
            Body::Json(json) => {
                headers.insert("Content-Type", "application/json".to_owned());
                Some(json.as_ref())
            }
            Body::Mime(_) | Body::Chunk { .. } => {
                return Err(Error::Batch("MIME bodies can't be batched".into()));
            }
        };
        Ok(Item {
            id: index.to_string(),
            method: request.method.as_str(),
            url,
            headers,
            body,
            depends_on: Vec::new(),
        })
    }

    /// Sends one batch and returns its answers by ID. The batch is throttled and retried like
    /// any request. If any inner response returns 401, the token used for the batch is
    /// invalidated so subsequent calls refresh.
    async fn send_batch(&self, batch: &Batch<'_>) -> Result<HashMap<String, Answer>, Error> {
        let call = self.call::<()>(Method::POST, ["$batch"]).json(batch);
        log::debug!("POST $batch ({} requests)", batch.requests.len());
        let (answer, token) = self.send_with_token(&call.request, <()>::throttled).await?;
        if !answer.status.is_success() {
            return Err(answer.error());
        }
        let answers: Answers = crate::client::parse(&answer.body)
            .map_err(|error| Error::Batch(format!("unreadable batch response: {error}")))?;
        if let Some(token) = token.as_deref()
            && answers
                .responses
                .iter()
                .any(|response| response.status == StatusCode::UNAUTHORIZED.as_u16())
        {
            self.invalidate_token(token);
        }
        Ok(answers
            .responses
            .into_iter()
            .map(|answer| (answer.id.clone(), answer))
            .collect())
    }
}
