//! HTTP client for `POST /v1/systemone` and `GET /v1/models`.
//!
//! # Defaults
//!
//! The defaults are the official SDKs' defaults, so a call behaves the same
//! from Rust as from Python or JavaScript and an incident seen in one is
//! reproducible in the others: the key from `TYPESAFE_API_KEY`, base URL
//! `https://api.typesafe.ai`, model `jev-latest`, a 10 s timeout per
//! attempt, and two retries with exponential backoff and jitter. Where the
//! retries deliberately differ from the SDKs is stated on [`RetryPolicy`].
//! The user agent is `judgment/<crate version>`, so the API's logs can tell
//! this client from the SDKs and from the application embedding it.
//!
//! The key is checked when the client is built, with the Python SDK's
//! (0.7.1) rules: surrounding whitespace is trimmed (the characters Python's
//! `str.isspace` accepts, so a key file's trailing newline is fine), a blank
//! key is [`Error::MissingApiKey`], and a key with whitespace inside it, a
//! control character or a non-ASCII character is [`Error::InvalidApiKey`].
//! A key passed to the builder is used or refused as it stands and never
//! replaced by the environment's. The trimmed key is the one sent.
//!
//! # Retries
//!
//! A transient failure must not fail a call that a second attempt would have
//! completed, and a persistent one must surface quickly enough for the caller
//! to fall back. [`RetryPolicy`] sits between those two costs; its defaults
//! are the official SDKs', the reasoning behind each field is on that type,
//! and so is the table of where it matches the SDKs and where it
//! deliberately differs. In short:
//!
//! * The retried statuses are a set on the policy, by default 408, 429 and
//!   every 5xx (TypeSafe's 529 included), because they are transient by
//!   definition; 400, 401, 403 and 422 are not, because a retry cannot fix a
//!   request body, a key or an account's access. Every transport failure is
//!   retried by default.
//! * The backoff doubles from 0.5 s to 5 s, and its jitter only shortens a
//!   wait, so the nominal backoff is also the longest.
//! * The server's wait, from `retry-after-ms` or from `Retry-After` in
//!   seconds or as an HTTP date, replaces the backoff when present, on any
//!   retried status, up to `retry_after_max`.
//! * An overall budget is available and off by default: with the defaults a
//!   call makes at most three attempts of 10 s each plus two waits, so a
//!   caller knows the bound before it adds a deadline of its own.
//!
//! [`RetryPolicy::conservative`] retries only what cannot have been billed
//! twice (408, 429 and a connection that was never made), for a caller who
//! would rather fail a call than pay for it again.
//!
//! Every failed attempt, retried or not, is reported to the process-wide
//! [`Observer`] under the service label `typesafe`, because a retried failure
//! is still load on the upstream and still a symptom. Token usage of every
//! successful response goes to the client's own observer when one was set on
//! the builder, otherwise to the same process-wide one.
//!
//! # Errors
//!
//! The final response is classified by status into [`Error`], grouped by
//! what fixes it:
//!
//! * 400 and 422 are [`Error::InvalidRequest`], with `status` telling them
//!   apart. Its `detail` is the server's message (read from `error`,
//!   `error.message`, `message`, `detail` or `detail.message`, in the Python
//!   SDK's order), otherwise the validation issues as `path: msg`, otherwise
//!   the body, truncated; `issues` keeps the fields a validation body names
//!   as [`ValidationIssue`]s.
//! * 401 is [`Error::Unauthorized`]; 403 is [`Error::PermissionDenied`],
//!   with the server's message, because a new key does not fix a 403.
//! * 429 is [`Error::RateLimited`], with the last response's wait; 529 is
//!   [`Error::Overloaded`]. Like [`Error::Transport`], both come back once
//!   the retry policy stopped: retries used up, the budget reached, or a
//!   status or transport failure the policy does not retry.
//! * Any other non-success status is [`Error::Http`] with the body. A 404
//!   stays there: both paths are fixed and carry no resource id, so a 404
//!   always means a base URL that is not the API or a server without the
//!   path, and the body is what says which.
//! * A 2xx whose body is not the documented shape is [`Error::Decode`], not
//!   retried; a transport failure is [`Error::Transport`] once the policy
//!   stopped.
//!
//! Each carries the attempt count where one applies, and every error that
//! came from an HTTP response carries its request id (below). The key is
//! marked sensitive and redacted from `Debug` output, so a client or builder
//! printed with `{:?}` cannot leak it, and no error message quotes it.
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
//! [`Client::list_models`] calls the model listing: the models and aliases
//! the account may send, each with a name, a description and a `YYYY-MM-DD`
//! release date. It is in the published OpenAPI document (0.2.0), and both
//! official SDKs call it `models.list()`; only the HTTP API reference page,
//! which covers the evaluation endpoint, leaves it out. A compatible server
//! may not serve it (laya-serve 0.3.20 does not), and then the call is
//! [`Error::Http`] with status 404. It stays because a readiness probe and a
//! model listing need it; nothing else in the crate depends on it.

use std::env::VarError;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::answer::Response;
use crate::error::{Error, Result, ValidationIssue};
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

pub use crate::http::{RetryPolicy, TransportRetry};

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
    /// Release date, `YYYY-MM-DD` per the OpenAPI document. Kept as the
    /// string sent, so a server that formats it otherwise still lists.
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
    ///
    /// [`build`](Self::build) trims it and refuses a malformed one. A key
    /// set here is never replaced by the environment's, not even when it is
    /// blank: a blank key is [`Error::MissingApiKey`], because a caller that
    /// passed a key meant that one, and silently authenticating with another
    /// would bill or authorise the wrong account.
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
    ///
    /// The key is checked first, before the URL and before anything is
    /// sent, with the Python SDK's (0.7.1) rules (module docs, `# Defaults`):
    /// trimmed, [`Error::MissingApiKey`] when blank, [`Error::InvalidApiKey`]
    /// when it has whitespace inside it, a control character or a non-ASCII
    /// character, or when `TYPESAFE_API_KEY` is not valid UTF-8. Neither
    /// message contains any part of the key. A base URL that does not parse
    /// is [`Error::Url`].
    pub fn build(self) -> Result<Client> {
        let api_key = resolve_api_key(self.api_key, || std::env::var(API_KEY_ENV))?;
        let base_url = Url::parse(&self.base_url).map_err(|e| Error::Url(e.to_string()))?;
        // Unreachable after `resolve_api_key` (printable ASCII always fits a
        // header), kept so a future change to that check cannot turn a bad
        // key into a panic or back into the URL error it used to be.
        let mut auth = HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|_| {
            Error::InvalidApiKey {
                reason: "the key cannot be sent in an HTTP header".into(),
            }
        })?;
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

    /// Production defaults with the API key from `TYPESAFE_API_KEY`,
    /// trimmed and checked as [`ClientBuilder::build`] says: unset or blank
    /// is [`Error::MissingApiKey`], malformed or not UTF-8 is
    /// [`Error::InvalidApiKey`].
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

    /// List the models and aliases this account may send (`GET /v1/models`,
    /// in the OpenAPI document; module docs, `` # `GET /v1/models` ``). A
    /// server that does not serve it answers [`Error::Http`] with status 404.
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
        code @ (400 | 422) => {
            let (detail, issues) = error_detail(&body);
            Error::InvalidRequest {
                status: code,
                detail,
                issues,
                request_id,
            }
        }
        401 => Error::Unauthorized { request_id },
        403 => Error::PermissionDenied {
            detail: error_detail(&body).0,
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
        // 404 included: both paths are fixed and carry no resource id, so a
        // 404 is always a base URL that is not the API or a server without
        // the path, and the body is what tells the two apart.
        code => Error::Http {
            status: code,
            body: http::truncate(body),
            request_id,
        },
    }
}

/// The readable message and the validation issues of a 400, 403 or 422
/// body, for [`Error::InvalidRequest`] and [`Error::PermissionDenied`].
///
/// The message is the first non-empty string of `error`, `error.message`,
/// `message`, `detail` (a string) and `detail.message`, which is the Python
/// SDK's order (`extract_message` in its `errors.py`); then the issues of a
/// `detail` list joined as `path: msg; …`; then the body itself. The issues
/// are parsed from a `detail` list whatever supplies the message, so code
/// gets them even when the server also sent prose. An entry without a string
/// `msg` is skipped, a missing or non-list `loc` is an empty location, a
/// non-string location item keeps its JSON text, and a missing `type` is an
/// empty kind. A body that is a JSON string is the message itself, as in the
/// SDK, unless it is empty; a blank one is kept, since the SDK keeps it too.
/// The result is truncated to 2,000 bytes, like every body this crate quotes.
///
/// Where it differs from the SDK, on purpose:
///
/// * An empty string does not win: `{"error": "", "message": "x"}` reads
///   `x`, where the SDK takes the empty `error` and falls back to the raw
///   body.
/// * Only a leading `body` location segment is dropped
///   ([`ValidationIssue::path`]); the SDK drops every one, which hides a
///   question whose id is `body`.
/// * A body that is not JSON is quoted truncated to 2,000 bytes; the SDK
///   keeps it whole. The raw-body fallback is 2,000 bytes too, where the SDK
///   cuts its message at 200 characters.
/// * A non-list `loc` gives an empty path, as in the SDK.
///
/// The value a validation entry echoes under `input` (and its `ctx`) is
/// dropped from the issues and the joined message, since `input` can be a
/// piece of the state. That is best-effort: a body this function does not
/// recognise is still quoted as it came, truncated.
fn error_detail(body: &str) -> (String, Vec<ValidationIssue>) {
    if body.trim().is_empty() {
        return ("no body".to_owned(), Vec::new());
    }
    let object = match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(object)) => object,
        Ok(Value::String(text)) if !text.is_empty() => {
            return (http::truncate(text), Vec::new());
        }
        _ => return (http::truncate(body.to_owned()), Vec::new()),
    };
    let issues: Vec<ValidationIssue> = object
        .get("detail")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter_map(validation_issue).collect())
        .unwrap_or_default();
    let nested = |key: &str| object.get(key).and_then(|v| v.get("message"));
    let message = non_empty_str(object.get("error"))
        .or_else(|| non_empty_str(nested("error")))
        .or_else(|| non_empty_str(object.get("message")))
        .or_else(|| non_empty_str(object.get("detail")))
        .or_else(|| non_empty_str(nested("detail")))
        .map(str::to_owned)
        .or_else(|| {
            (!issues.is_empty()).then(|| {
                issues
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
        })
        .unwrap_or_else(|| body.to_owned());
    (http::truncate(message), issues)
}

/// `value` when it is a non-empty JSON string: an empty message does not
/// win over the next field.
fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// One entry of a validation `detail` list, or `None` when it has no string
/// `msg`. `input` and `ctx` are not read.
fn validation_issue(entry: &Value) -> Option<ValidationIssue> {
    let msg = entry.get("msg")?.as_str()?.to_owned();
    let loc = entry
        .get("loc")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map_or_else(|| item.to_string(), str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    let kind = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Some(ValidationIssue { loc, msg, kind })
}

/// Whether `c` is whitespace to Python's `str.isspace`, which the Python
/// SDK trims from a key: Rust's `char::is_whitespace` plus the four
/// information separators U+001C to U+001F, which Python counts and Rust
/// does not. The two sets are otherwise equal.
fn is_sdk_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// The key to send: `explicit` when the builder was given one, otherwise
/// what `env` returns for `TYPESAFE_API_KEY`, trimmed and checked with the
/// Python SDK's (0.7.1) rules (module docs, `# Defaults`).
///
/// The environment is a parameter so tests can supply one: setting a
/// process variable is `unsafe` in edition 2024, and this workspace forbids
/// `unsafe`. `env` is not called when a key was passed explicitly, blank or
/// not. No error names any part of the key; the reason names where it came
/// from and what is wrong with it.
fn resolve_api_key(
    explicit: Option<String>,
    env: impl FnOnce() -> Result<String, VarError>,
) -> Result<String> {
    let (raw, origin) = match explicit {
        Some(key) => (key, "the key passed to the client builder"),
        None => match env() {
            Ok(key) => (key, API_KEY_ENV),
            Err(VarError::NotPresent) => return Err(Error::MissingApiKey),
            Err(VarError::NotUnicode(_)) => {
                return Err(Error::InvalidApiKey {
                    reason: format!("{API_KEY_ENV} is not valid UTF-8"),
                });
            }
        },
    };
    let key = raw.trim_matches(is_sdk_space);
    if key.is_empty() {
        return Err(Error::MissingApiKey);
    }
    if let Some(c) = key.chars().find(|c| !c.is_ascii_graphic()) {
        let what = if is_sdk_space(c) {
            "has whitespace inside it"
        } else if c.is_control() {
            "contains a control character"
        } else {
            // A byte-order mark (U+FEFF) lands here too: it is not
            // whitespace to Python either, so it is refused, not trimmed.
            "contains a non-ASCII character"
        };
        return Err(Error::InvalidApiKey {
            reason: format!("{origin} {what}"),
        });
    }
    Ok(key.to_owned())
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
    #![allow(clippy::unwrap_used)]

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

    /// An environment that must not be read.
    fn no_env() -> std::result::Result<String, VarError> {
        panic!("the environment was read although a key was passed")
    }

    fn resolved(explicit: &str) -> Result<String> {
        resolve_api_key(Some(explicit.to_owned()), no_env)
    }

    #[test]
    fn a_key_is_trimmed_like_the_sdk() {
        assert_eq!(resolved("  sk-abc\r\n").ok().as_deref(), Some("sk-abc"));
        // The information separators are whitespace to Python, not to Rust.
        assert_eq!(
            resolved("\u{1f}sk-abc\u{1c}").ok().as_deref(),
            Some("sk-abc")
        );
        assert_eq!(
            resolved("\u{a0}sk-abc\u{3000}").ok().as_deref(),
            Some("sk-abc")
        );
    }

    #[test]
    fn a_blank_explicit_key_is_missing_and_never_reads_the_environment() {
        for blank in ["", "   ", "\n", "\t\u{1d}\u{a0}"] {
            let err = resolved(blank).unwrap_err();
            assert!(matches!(err, Error::MissingApiKey), "{blank:?}: {err:?}");
        }
    }

    #[test]
    fn an_unset_or_blank_environment_is_missing() {
        let from_env = |value: std::result::Result<&str, VarError>| {
            resolve_api_key(None, || value.map(str::to_owned))
        };
        assert!(matches!(
            from_env(Err(VarError::NotPresent)),
            Err(Error::MissingApiKey)
        ));
        assert!(matches!(from_env(Ok("  ")), Err(Error::MissingApiKey)));
        assert_eq!(from_env(Ok("sk-env\n")).ok().as_deref(), Some("sk-env"));
    }

    #[test]
    fn a_malformed_key_is_invalid_and_never_quoted() {
        let cases = [
            ("SECRET XYZ", "has whitespace inside it"),
            ("SECRET\tXYZ", "has whitespace inside it"),
            ("SECRET\u{a0}XYZ", "has whitespace inside it"),
            ("SECRET\u{7f}XYZ", "contains a control character"),
            ("SECRETXYZ\u{0}", "contains a control character"),
            ("SECRETXYZé", "contains a non-ASCII character"),
            ("SECRET\u{200b}XYZ", "contains a non-ASCII character"),
            ("\u{feff}SECRETXYZ", "contains a non-ASCII character"),
        ];
        for (key, what) in cases {
            let err = resolved(key).unwrap_err();
            let Error::InvalidApiKey { reason } = &err else {
                panic!("{key:?}: {err:?}");
            };
            assert_eq!(
                reason,
                &format!("the key passed to the client builder {what}"),
                "{key:?}"
            );
            for shown in [err.to_string(), format!("{err:?}")] {
                assert!(!shown.contains("SECRET"), "{key:?}: {shown}");
                assert!(!shown.contains("XYZ"), "{key:?}: {shown}");
            }
        }
        // From the environment, the reason names the variable instead.
        let err = resolve_api_key(None, || Ok("SECRET XYZ".to_owned())).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid API key: TYPESAFE_API_KEY has whitespace inside it"
        );
    }

    #[test]
    fn a_non_utf8_environment_key_is_invalid() {
        let err = resolve_api_key(None, || {
            Err(VarError::NotUnicode(std::ffi::OsString::from("sk")))
        })
        .unwrap_err();
        assert!(
            matches!(&err, Error::InvalidApiKey { reason } if reason == "TYPESAFE_API_KEY is not valid UTF-8"),
            "{err:?}"
        );
    }

    #[test]
    fn build_refuses_an_invalid_key_before_any_request() {
        // Nothing listens on the base URL: a request would be a transport
        // error. 0.1 returned a URL error for a key the header refuses (a
        // control character such as an inner CR) and sent a key with a space
        // as it was; both are a key error now, before any request.
        for key in ["sk\rabc", "sk abc"] {
            let err = Client::builder()
                .api_key(key)
                .base_url("http://127.0.0.1:1")
                .build()
                .unwrap_err();
            assert!(
                matches!(err, Error::InvalidApiKey { .. }),
                "{key:?}: {err:?}"
            );
        }
        // The key is checked before the URL, so a bad key is reported even
        // when the URL is bad too.
        let err = Client::builder()
            .api_key("sk\u{e9}")
            .base_url("not a url")
            .build()
            .unwrap_err();
        assert!(matches!(err, Error::InvalidApiKey { .. }), "{err:?}");
        let err = Client::builder().api_key(" \n").build().unwrap_err();
        assert!(matches!(err, Error::MissingApiKey), "{err:?}");
    }

    fn detail(body: &str) -> (String, Vec<ValidationIssue>) {
        error_detail(body)
    }

    fn only_detail(body: &str) -> String {
        let (message, issues) = error_detail(body);
        assert!(issues.is_empty(), "{body}: {issues:?}");
        message
    }

    #[test]
    fn error_detail_reads_every_body_shape() {
        // The OpenAPI document's example, at
        // /components/schemas/HTTPValidationError/properties/detail/examples/0.
        let (message, issues) = detail(
            r#"{"detail":[{"loc":["body","state"],"msg":"Field required","type":"missing"}]}"#,
        );
        assert_eq!(message, "state: Field required");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].path(), "state");
        assert_eq!(issues[0].kind, "missing");
        assert_eq!(issues[0].loc, ["body", "state"]);

        // The ValidationError.loc example: a question error names the id
        // second and the question type third.
        let (message, issues) = detail(
            r#"{"detail":[{"loc":["body","questions","urgency","score","criteria"],"msg":"List should have at least 2 items","type":"too_short"}]}"#,
        );
        assert_eq!(issues[0].path(), "questions.urgency.score.criteria");
        assert_eq!(
            message,
            "questions.urgency.score.criteria: List should have at least 2 items"
        );

        // Two issues are joined in order.
        let (message, issues) = detail(
            r#"{"detail":[{"loc":["body","state"],"msg":"Field required","type":"missing"},{"loc":["body","model"],"msg":"Input should be a valid string","type":"string_type"}]}"#,
        );
        assert_eq!(issues.len(), 2);
        assert_eq!(
            message,
            "state: Field required; model: Input should be a valid string"
        );

        // The server's own message, in the SDK's order.
        assert_eq!(
            only_detail(r#"{"detail":"questions.x.criteria: invalid"}"#),
            "questions.x.criteria: invalid"
        );
        assert_eq!(
            only_detail(r#"{"error":"model mismatch"}"#),
            "model mismatch"
        );
        assert_eq!(
            only_detail(r#"{"error":{"message":"bad question","type":"invalid_request"}}"#),
            "bad question"
        );
        assert_eq!(only_detail(r#"{"message":"slow down"}"#), "slow down");
        assert_eq!(only_detail(r#"{"detail":{"message":"nested"}}"#), "nested");
        assert_eq!(
            only_detail(r#"{"error":"first","message":"second"}"#),
            "first"
        );
        // A message wins over the issues, which are still parsed.
        let (message, issues) = detail(
            r#"{"message":"invalid body","detail":[{"loc":["body","state"],"msg":"Field required","type":"missing"}]}"#,
        );
        assert_eq!(message, "invalid body");
        assert_eq!(issues.len(), 1);
        // Pinned deviation: an empty string does not win.
        assert_eq!(only_detail(r#"{"error":"","message":"x"}"#), "x");

        // Not a message: the body, truncated.
        let html = "<html><body>Bad Gateway</body></html>";
        assert_eq!(only_detail(html), html);
        let long = format!("<html>{}</html>", "x".repeat(5_000));
        let quoted = only_detail(&long);
        assert!(quoted.len() <= 2_000 + '…'.len_utf8(), "{}", quoted.len());
        assert!(quoted.ends_with('…'));
        assert_eq!(only_detail(""), "no body");
        assert_eq!(only_detail(" \n"), "no body");
        assert_eq!(only_detail(r#"{"detail":[]}"#), r#"{"detail":[]}"#);
        assert_eq!(only_detail(r#"{"other":1}"#), r#"{"other":1}"#);
        assert_eq!(only_detail("[1,2]"), "[1,2]");
        assert_eq!(only_detail("42"), "42");
        // A JSON string body is its own message, blank included, as in the
        // SDK (`body or None`); only an empty one is quoted.
        assert_eq!(only_detail(r#""model not found""#), "model not found");
        assert_eq!(only_detail(r#"" ""#), " ");
        assert_eq!(only_detail(r#""""#), r#""""#);

        // Entries without a string msg are skipped; the rest still parse.
        let (message, issues) = detail(
            r#"{"detail":[{"loc":["body","a"]},{"loc":["body","b"],"msg":7},"text",{"loc":["body","c"],"msg":"kept","type":"x"}]}"#,
        );
        assert_eq!(issues.len(), 1);
        assert_eq!(message, "c: kept");
        // No entry has a msg: the raw body.
        let body = r#"{"detail":[{"loc":["body","a"]}]}"#;
        assert_eq!(only_detail(body), body);

        // A missing loc, or one that is not a list, is an empty path.
        let (message, issues) = detail(r#"{"detail":[{"msg":"Field required","type":"missing"}]}"#);
        assert!(issues[0].loc.is_empty());
        assert_eq!(message, "Field required");
        let (message, issues) =
            detail(r#"{"detail":[{"loc":"body.state","msg":"Field required"}]}"#);
        assert!(issues[0].loc.is_empty());
        assert_eq!(issues[0].kind, "", "a missing type is an empty kind");
        assert_eq!(message, "Field required");

        // An integer segment is stringified.
        let (message, issues) = detail(
            r#"{"detail":[{"loc":["body","questions","risk","score","criteria",0],"msg":"Input should be a valid string","type":"string_type"}]}"#,
        );
        assert_eq!(issues[0].loc.last().map(String::as_str), Some("0"));
        assert_eq!(
            message,
            "questions.risk.score.criteria.0: Input should be a valid string"
        );
    }

    #[test]
    fn validation_input_never_reaches_the_message() {
        let (message, issues) = detail(
            r#"{"detail":[{"loc":["body","state"],"msg":"Input should be a valid dictionary","type":"dict_type","input":"SECRET-STATE","ctx":{"hint":"SECRET-CTX"}}]}"#,
        );
        assert_eq!(message, "state: Input should be a valid dictionary");
        let shown = format!("{message} {issues:?}");
        assert!(!shown.contains("SECRET"), "{shown}");

        // Best-effort only: a body with no usable entry is still quoted.
        let body = r#"{"detail":[{"loc":["body","state"],"input":"SECRET-STATE"}]}"#;
        assert!(only_detail(body).contains("SECRET-STATE"));
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
