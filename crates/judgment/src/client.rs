//! HTTP client for `POST /v1/systemone` and `GET /v1/models`.
//!
//! # Defaults
//!
//! The defaults mirror the official SDKs, so a call behaves the same from
//! Rust as from Python and an incident seen in one is reproducible in the
//! other: the key from `TYPESAFE_API_KEY`, base URL `https://api.typesafe.ai`,
//! model `jev-latest`, a 10 s timeout per attempt, and two retries with
//! exponential backoff and jitter. The user agent is `judgment/<crate
//! version>`, so the API's logs can tell this client from the SDKs and from
//! the application embedding it.
//!
//! # Retries
//!
//! A transient failure must not fail a call that a second attempt would have
//! completed, and a persistent one must surface quickly enough for the caller
//! to fall back. [`RetryPolicy`] sits between those two costs; its defaults
//! are the Python SDK's, and the reasoning behind each field is on that type.
//! In short: 408, 429 and every 5xx (TypeSafe's 529 included) are retried
//! because they are transient by definition; 401 and 422 are not, because a
//! retry cannot fix a key or a request body; `Retry-After` wins over the
//! backoff when present, only up to `retry_after_max` and only in its
//! delay-seconds form; and with the defaults a call makes at most three
//! attempts of 10 s each plus two waits, so a caller knows the bound before
//! it adds a deadline of its own.
//!
//! Every failed attempt, retried or not, is reported to the process-wide
//! [`Observer`] under the service label `typesafe`, because a retried failure
//! is still load on the upstream and still a symptom. Token usage of every
//! successful response goes to the client's own observer when one was set on
//! the builder, otherwise to the same process-wide one.
//!
//! # Errors
//!
//! The final response is classified by status into [`Error`]: 401 is
//! [`Error::Unauthorized`]; 422 is [`Error::InvalidRequest`] with the body;
//! 429 after the retries is [`Error::RateLimited`] with the last
//! `Retry-After`; 529 after the retries is [`Error::Overloaded`]; any other
//! non-success status is [`Error::Http`]; a 2xx whose body is not the
//! documented shape is [`Error::Decode`], not retried; and a transport
//! failure after the retries is [`Error::Transport`]. Each carries the
//! attempt count where one applies, and every error that came from an HTTP
//! response carries its request id (below). The key is marked sensitive and
//! redacted from `Debug` output, so a client or builder printed with `{:?}`
//! cannot leak it.
//!
//! # Request id
//!
//! TypeSafe identifies a call by an `x-typesafe-request-id` response header
//! ([`REQUEST_ID_HEADER`]): the one link from a failed call or a surprising
//! answer to TypeSafe's own logs, and what its support asks for. Both
//! official SDKs expose it on success and on errors. This client reads it
//! here, not in the shared loop, which stays free of any one vendor's
//! header names, and puts it in three places: [`Response::request_id`] on
//! success, [`Error::request_id`] and a ` [request_id …]` suffix on the
//! message of every HTTP-derived error (a 2xx that fails to decode
//! included), and the `request_id` field of the `typesafe.evaluate` and
//! `typesafe.list_models` spans, so a trace names the call too.
//!
//! What it does not cover:
//!
//! * It is the last attempt's id. A call that was retried had earlier
//!   requests with ids of their own; the loop drops those responses.
//! * A transport failure has none: either no response came back, or its
//!   body could not be read and the shared loop drops that response's
//!   headers, as the SDKs do.
//! * [`Client::list_models`] returns a plain list, so its id is only on the
//!   span and on its errors.
//! * It is optional everywhere, like the JS SDK's `requestId`; the Python
//!   SDK's success-side property raises when the header is absent instead.
//!   The OpenAPI document lists no response headers, a self-hosted server
//!   may send none, and this client has not yet seen one from the hosted
//!   API. A value that is empty, not printable ASCII, or longer than 256
//!   bytes is ignored, so what reaches a span or a message stays bounded.
//!
//! # `GET /v1/models`
//!
//! [`Client::list_models`] calls an endpoint the official SDKs expose but the
//! HTTP API reference does not document. It is observed rather than
//! documented and could change without notice. It stays because a readiness
//! probe and a model listing need it; nothing else in the crate depends on it.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::answer::Response;
use crate::error::{Error, Result};
use crate::http::{self, Completed, Exhausted};
use crate::observer::Observer;
use crate::question::Questions;

/// Environment variable holding the API key: the one secret this client
/// reads itself. Base URL and model come from the configuration layer.
pub const API_KEY_ENV: &str = "TYPESAFE_API_KEY";
/// Production API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";
/// Alias for the current stable release.
pub const DEFAULT_MODEL: &str = "jev-latest";
/// Default per-attempt timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Response header carrying TypeSafe's id for the request: see the module's
/// `# Request id` section.
pub const REQUEST_ID_HEADER: &str = "x-typesafe-request-id";
/// Longest request id kept, in bytes. Generous for an identifier (a UUID is
/// 36); short enough that a hostile or broken header cannot put an unbounded
/// value into every span and error message.
const REQUEST_ID_MAX_LEN: usize = 256;

pub use crate::http::RetryPolicy;

/// The body of `POST /v1/systemone`.
#[derive(Debug, Clone, Serialize)]
pub struct Request<'a, S: Serialize> {
    /// Content to evaluate: string, object or array.
    pub state: &'a S,
    /// Model name or alias.
    pub model: &'a str,
    /// The questions.
    pub questions: &'a Questions,
}

/// One entry from `GET /v1/models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Name accepted by the `model` field.
    pub name: String,
    /// What the model is for.
    pub description: String,
    /// Release date.
    pub release_date: String,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    models: Vec<ModelInfo>,
}

/// Builder for [`Client`].
#[derive(Clone)]
pub struct ClientBuilder {
    api_key: Option<String>,
    base_url: String,
    model: String,
    timeout: Duration,
    retry: RetryPolicy,
    observer: Option<Arc<dyn Observer>>,
}

impl fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientBuilder")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("timeout", &self.timeout)
            .field("retry", &self.retry)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("observer", &self.observer.as_ref().map(|_| "set"))
            .finish()
    }
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: DEFAULT_MODEL.to_owned(),
            timeout: DEFAULT_TIMEOUT,
            retry: RetryPolicy::default(),
            observer: None,
        }
    }
}

impl ClientBuilder {
    /// Set the API key explicitly (otherwise read from `TYPESAFE_API_KEY`).
    #[must_use]
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Override the base URL (for tests or a proxy).
    #[must_use]
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Default model for [`Client::system_one`].
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Per-attempt timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Retry policy.
    #[must_use]
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Where this client reports token usage. Without one it reports to
    /// [`crate::observer::global`]. Failed attempts always go to the global
    /// observer, from the shared retry loop in [`crate::http`].
    #[must_use]
    pub fn observer(mut self, observer: Arc<dyn Observer>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Build the client. Reads `TYPESAFE_API_KEY` if no key was set.
    pub fn build(self) -> Result<Client> {
        let api_key = self
            .api_key
            .or_else(|| std::env::var(API_KEY_ENV).ok())
            .filter(|k| !k.trim().is_empty())
            .ok_or(Error::MissingApiKey)?;
        let base_url = Url::parse(&self.base_url).map_err(|e| Error::Url(e.to_string()))?;
        let mut auth = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| Error::Url("API key contains characters invalid in a header".into()))?;
        auth.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, auth);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(self.timeout)
            .user_agent(concat!("judgment/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Error::Transport {
                attempts: 0,
                source: e,
            })?;
        Ok(Client {
            http,
            base_url,
            model: self.model,
            retry: self.retry,
            observer: self.observer,
        })
    }
}

/// A configured TypeSafe API client. Cheap to clone.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: Url,
    model: String,
    retry: RetryPolicy,
    observer: Option<Arc<dyn Observer>>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base_url.as_str())
            .field("model", &self.model)
            .field("retry", &self.retry)
            .field("api_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Start building a client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Production defaults with the API key from `TYPESAFE_API_KEY`.
    pub fn from_env() -> Result<Self> {
        ClientBuilder::default().build()
    }

    /// Evaluate `state` against `questions` with the default model.
    pub async fn system_one<S: Serialize + Sync>(
        &self,
        state: &S,
        questions: &Questions,
    ) -> Result<Response> {
        self.evaluate(&Request {
            state,
            model: &self.model,
            questions,
        })
        .await
    }

    /// Evaluate a fully specified request.
    ///
    /// The response's [`Response::request_id`] is the last attempt's
    /// `x-typesafe-request-id`, and it is recorded on the span, on success
    /// and on failure (module docs, `# Request id`).
    #[tracing::instrument(
        name = "typesafe.evaluate",
        skip_all,
        fields(
            model = request.model,
            input_tokens = tracing::field::Empty,
            request_id = tracing::field::Empty
        )
    )]
    pub async fn evaluate<S: Serialize + Sync>(
        &self,
        request: &Request<'_, S>,
    ) -> Result<Response> {
        let url = self.url("v1/systemone")?;
        let body = serde_json::to_vec(request)?;
        let reply = match self
            .send(&self.retry, || {
                self.http.post(url.clone()).body(body.clone())
            })
            .await
        {
            Ok(reply) => reply,
            Err(e) => {
                record_request_id(e.request_id());
                return Err(e);
            }
        };
        record_request_id(reply.request_id.as_deref());
        let mut response: Response = reply.decode()?;
        // The field means "the header": a body key of the same name is
        // overwritten, with `None` when the header was absent.
        response.request_id = reply.request_id;
        tracing::Span::current().record("input_tokens", response.usage.input_tokens);
        self.observer().on_usage(&response.model, &response.usage);
        Ok(response)
    }

    /// List the models this account may send. Observed rather than
    /// documented: the SDKs expose it, the HTTP API reference does not.
    ///
    /// The list carries no request id; the `typesafe.list_models` span and
    /// every error do.
    #[tracing::instrument(
        name = "typesafe.list_models",
        skip_all,
        fields(request_id = tracing::field::Empty)
    )]
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let url = self.url("v1/models")?;
        let reply = match self.send(&self.retry, || self.http.get(url.clone())).await {
            Ok(reply) => reply,
            Err(e) => {
                record_request_id(e.request_id());
                return Err(e);
            }
        };
        record_request_id(reply.request_id.as_deref());
        let parsed: ModelsResponse = reply.decode()?;
        Ok(parsed.models)
    }

    /// This client's observer, or the process-wide one.
    fn observer(&self) -> Arc<dyn Observer> {
        self.observer
            .clone()
            .unwrap_or_else(crate::observer::global)
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.base_url
            .join(path)
            .map_err(|e| Error::Url(e.to_string()))
    }

    /// Run `make` through the shared retry loop under `policy` and turn the
    /// last response into a [`Reply`] or an [`Error`], reading the request
    /// id from its headers. It does not touch the span: the instrumented
    /// caller records the id, so the write stays beside the field's
    /// declaration and cannot land on some other span a future caller has
    /// open.
    async fn send(
        &self,
        policy: &RetryPolicy,
        make: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<Reply> {
        match http::send_with_retries(policy, "typesafe", make).await {
            Ok(Completed {
                status,
                body,
                attempts,
                retry_after,
                headers,
                ..
            }) => {
                let request_id = read_request_id(&headers);
                if status.is_success() {
                    Ok(Reply { body, request_id })
                } else {
                    Err(classify(status, body, attempts, retry_after, request_id))
                }
            }
            Err(Exhausted {
                attempts, source, ..
            }) => Err(Error::Transport { attempts, source }),
        }
    }
}

/// A 2xx body and the request id its response carried, not yet decoded.
struct Reply {
    body: String,
    request_id: Option<String>,
}

impl Reply {
    /// Decode the body; a failure keeps the response's request id, since a
    /// 2xx that does not decode is still a call TypeSafe can look up.
    fn decode<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.body).map_err(|source| Error::Decode {
            source,
            request_id: self.request_id.clone(),
        })
    }
}

/// Classify a final non-success response by status (module docs,
/// `# Errors`).
fn classify(
    status: StatusCode,
    body: String,
    attempts: u32,
    retry_after: Option<Duration>,
    request_id: Option<String>,
) -> Error {
    match status.as_u16() {
        401 => Error::Unauthorized { request_id },
        422 => Error::InvalidRequest {
            detail: http::truncate(body),
            request_id,
        },
        429 => Error::RateLimited {
            attempts,
            retry_after,
            request_id,
        },
        529 => Error::Overloaded {
            attempts,
            request_id,
        },
        code => Error::Http {
            status: code,
            body: http::truncate(body),
            request_id,
        },
    }
}

/// The usable request id in `headers`, if any: the first value of
/// [`REQUEST_ID_HEADER`], trimmed, when it is printable ASCII (`to_str`
/// accepts only that, spaces and tabs; CR, LF and any other byte are
/// refused), not empty, and at most [`REQUEST_ID_MAX_LEN`] bytes. A longer
/// one is dropped and logged at `debug` with its length only.
fn read_request_id(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(REQUEST_ID_HEADER)?.to_str().ok()?.trim();
    if v.is_empty() {
        return None;
    }
    if v.len() > REQUEST_ID_MAX_LEN {
        tracing::debug!(len = v.len(), "x-typesafe-request-id ignored: too long");
        return None;
    }
    Some(v.to_owned())
}

/// Record `id` as the current span's `request_id`. Called only from a
/// method whose `#[tracing::instrument]` declares `request_id`, so the
/// current span is that method's; recording nothing leaves the field
/// absent, which is how a trace says no id was sent.
fn record_request_id(id: Option<&str>) {
    if let Some(id) = id {
        tracing::Span::current().record("request_id", id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_is_read_only_when_usable() {
        let read = |value: HeaderValue| {
            let mut h = HeaderMap::new();
            h.insert(REQUEST_ID_HEADER, value);
            read_request_id(&h)
        };
        assert_eq!(
            read(HeaderValue::from_static("req_1")).as_deref(),
            Some("req_1")
        );
        assert_eq!(read_request_id(&HeaderMap::new()), None, "absent");
        assert_eq!(read(HeaderValue::from_static("")), None, "empty");
        assert_eq!(read(HeaderValue::from_static("   ")), None, "whitespace");
        assert_eq!(
            read(HeaderValue::from_static(" req_2 ")).as_deref(),
            Some("req_2"),
            "trimmed"
        );
        let longest = "r".repeat(REQUEST_ID_MAX_LEN);
        assert_eq!(
            read(HeaderValue::from_str(&longest).unwrap_or_else(|e| panic!("{e}"))),
            Some(longest.clone()),
            "256 bytes is kept"
        );
        let too_long = "r".repeat(REQUEST_ID_MAX_LEN + 1);
        assert_eq!(
            read(HeaderValue::from_str(&too_long).unwrap_or_else(|e| panic!("{e}"))),
            None,
            "257 bytes is dropped"
        );
        let opaque = HeaderValue::from_bytes(b"req\x80").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read(opaque), None, "not printable ASCII");
    }

    #[test]
    fn debug_output_redacts_the_key() {
        let c = Client::builder()
            .api_key("sk-secret")
            .build()
            .unwrap_or_else(|e| panic!("{e}"));
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("sk-secret"));
        assert!(dbg.contains("<redacted>"));
    }
}
