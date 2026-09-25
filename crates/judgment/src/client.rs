//! HTTP client for `POST /v1/systemone` and `GET /v1/models`.
//!
//! # Defaults
//!
//! The defaults follow the official SDKs': the key from `TYPESAFE_API_KEY`,
//! base URL `https://api.typesafe.ai`, model `jev-latest`, a 10 s timeout
//! per attempt, and two retries with exponential backoff and jitter. Where
//! the retries deliberately differ from the SDKs (the total budget and the
//! server-wait cap among them) is stated on [`RetryPolicy`].
//! The user agent is `judgment/<crate version>`, so the API's logs can tell
//! this client from the SDKs and from the application embedding it. The
//! client sends no header of its own beyond `authorization`, `content-type`
//! and that user agent; [`ClientBuilder::default_header`] adds one to every
//! call, under the rule in `# Per-call options`.
//!
//! The client follows no redirect, like the Python SDK (httpx follows none
//! by default) and unlike the JS SDK, whose `fetch` follows them. A 3xx is
//! [`Error::Http`] with that status, not retried. Both paths are fixed and
//! neither the API reference nor the OpenAPI document gives either one a
//! redirect, so a 3xx means a base URL that points at something else, and
//! following it would be worse than failing: `reqwest` strips
//! `authorization` on a cross-origin redirect but not a header set with
//! [`ClientBuilder::default_header`] or [`CallOptions::header`], so a
//! gateway credential would reach whatever `Location` named, over plain HTTP
//! if it said so; a 307 or 308 re-sends the body, the caller's state
//! included; and even a same-origin redirect re-sends a billed call. No
//! same-origin policy is offered instead: a moved API is a base URL to
//! change.
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
//! is still load on the upstream and still a symptom. The shared retry loop
//! reports those, by status or as `transport`. The client also reports a
//! 2xx it could not use, which the loop counts as a success: `decode` for a
//! body that does not decode, `unfit` for a response that does not answer
//! the questions it was sent (`# Errors`). Token usage of every response
//! that decoded goes to the client's own observer when one was set on the
//! builder, otherwise to the same process-wide one; an unfit response is
//! billed like any other, so its usage is reported too.
//!
//! # Errors
//!
//! Before anything is sent, a per-call option or a default header that
//! would replace what the client sets itself is refused
//! ([`Error::ReservedHeader`], [`Error::ReservedField`]; `# Per-call
//! options`), with no attempt made or counted. After, the final response is
//! classified by status into [`Error`], grouped by what fixes it:
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
//!   retried, and reported to the process-wide observer as a failed attempt
//!   with status `decode`; a transport failure is [`Error::Transport`] once
//!   the policy stopped.
//! * A 2xx that decodes but does not answer the questions it was sent is
//!   the error [`crate::Response::verify`] names: [`Error::MissingAnswer`],
//!   [`Error::AnswerTypeMismatch`], [`Error::UnknownOption`] or
//!   [`Error::InvalidAnswer`] ([`Error::is_unfit`]), carrying the request
//!   id. It is not retried: the call went through and was billed, and a
//!   second attempt is billed again for an answer that is no likelier to
//!   fit. Its usage is still reported, since the tokens were spent, and it
//!   is reported as a failed attempt with status `unfit`. Checking here,
//!   rather than leaving it to [`crate::Response::get`], means a caller never
//!   holds a response that answers some other question, and an off-list
//!   Choice option cannot be read as a guess.
//!
//! Each carries the attempt count where one applies, and every error that
//! came from an HTTP response carries its request id (below). The key is
//! marked sensitive and redacted from `Debug` output, so a client or builder
//! printed with `{:?}` cannot leak it, and no error message quotes it.
//!
//! What the API may add is not an error (`answer` module docs, `# Decoding
//! is tolerant, reading is strict`). An answer of a kind this release does
//! not know is kept as [`Answer::Unknown`] and logged once per answer at
//! `warn`, inside the `typesafe.evaluate` span, with the answer's key and
//! its kind, both escaped and cut to 64 characters, since the server chose
//! both (the key is the question id, or any string at all for an answer to a
//! question that was not asked): the sign that the API has a primitive this
//! build cannot read, and that upgrading the crate is due.
//! The Python SDK logs a warning too and skips the answer; this client keeps
//! it, so a recording and a caller can still see it, and reading it through
//! a handle is [`Error::AnswerTypeMismatch`]. An absent `usage` reads as
//! zero, and undocumented top-level fields are kept in [`Response::extra`]
//! without a warning, since a compatible server may add them to every
//! response. A [`Replay`](crate::Replay), a [`Fake`](crate::Fake) and
//! [`crate::eval::read_recording`] do not warn: a Fake answers what its
//! test scripted, and a recorded answer was warned about when the client
//! received it.
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
//! # Per-call options
//!
//! One client usually serves calls that want different things: a batch job
//! that can wait longer than an interactive request, a billed call that must
//! not be retried, a header a gateway in front of the API routes on, or a
//! server parameter this crate does not model. A client per combination
//! would duplicate the connection pool and the key checks, so
//! [`Client::evaluate_with`] takes a [`CallOptions`] for one call: a
//! per-attempt timeout, a retry policy, headers and extra top-level body
//! fields. [`ClientBuilder::default_header`] sets a header for every call.
//! The model is not an option: it is already per call, on the [`Request`],
//! and a second place to set it would let the span's `model` field name one
//! model while another was sent.
//!
//! Precedence, lowest first: the builder's settings and default headers,
//! then the call's options, then what the client owns. What the client owns
//! is refused, never overwritten: the `authorization`, `content-type`,
//! `user-agent` and `x-typesafe-retry-count` headers, and the headers HTTP
//! itself owns (`content-length`, `transfer-encoding`, `host`,
//! `connection`, `te` and `upgrade`), are [`Error::ReservedHeader`]; the
//! `state`, `model` and `questions` fields are [`Error::ReservedField`]. A
//! call's option is refused when it is added to the [`CallOptions`], and a
//! default header when the client is built, where the key and URL are
//! checked too; both before anything is sent.
//!
//! The official SDKs do not refuse. They silently keep their own headers
//! over a caller's, and the Python SDK merges `extra_body` last-write-wins,
//! so an extra `questions` replaces the questions. This crate is stricter on
//! purpose: an overwritten `questions` or `state` would send a request that
//! the response is not read against and that a recording is not filed
//! under; an overwritten `model` would make the span name a model that was
//! not sent; and a per-call `authorization` would really replace the key,
//! authenticating as another account through a client built for one. An
//! error names the conflict where the SDKs hide it. `x-typesafe-retry-count`
//! is not sent by this release, but both SDKs own it and strip a caller's
//! value, so reserving it now means sending it later breaks no caller.
//!
//! The HTTP headers are refused because the HTTP stack keeps a caller's
//! value over its own instead of refusing it: a `content-length` shorter
//! than the body sends the JSON truncated, a longer one stalls the write
//! until the attempt fails, `transfer-encoding`, `connection`, `te` and
//! `upgrade` change how the body is framed or the connection used, and a
//! `host` sends the key to whatever virtual host a gateway routes that name
//! to rather than the one the base URL names. The base URL is the one place
//! the target is set; a caller who needs another host sets it there.
//! Refusing a name now and allowing it in a later release breaks nobody,
//! where the reverse would.
//!
//! How each option behaves:
//!
//! * The timeout replaces the client's for that call, rather than the
//!   shorter of the two winning, so a call can be given longer as well as
//!   shorter. It keeps the crate's meaning: one deadline per attempt, from
//!   connecting to the end of the body. That is the JS SDK's per-call
//!   timeout; the Python SDK's is httpx's per-phase timeout, which this one
//!   is stricter than.
//! * The retry policy replaces the client's whole policy, as in the Python
//!   SDK. The JS SDK merges a partial policy into the client's field by
//!   field; here that is `RetryPolicy { max_retries: 1,
//!   ..client.retry().clone() }`. A budget, when wanted, comes with the
//!   policy.
//! * A header replaces a default header of the same name for that call.
//!   Names are case-insensitive.
//! * Extra fields go at the top level, after `state`, `model` and
//!   `questions`; `null` is sent as `null`. A server may use, ignore or
//!   refuse a field it does not know; the live test
//!   `an_unknown_extra_field_is_answered_or_refused_by_name` records which.
//! * Headers and the timeout reach every attempt, retries included.
//! * No option's value is ever a span field, since a header can be a
//!   credential and an extra field a piece of the state.
//!
//! Without options a call sends exactly what it did before options existed:
//! the same body bytes, headers and span.
//!
//! The options stop at the [`SystemOne`](crate::SystemOne) trait: its
//! implementation for [`Client`] calls [`Client::evaluate`] with the
//! client's own settings, and [`Fake`](crate::Fake),
//! [`Recorder`](crate::Recorder) and [`Replay`](crate::Replay) take none.
//! A recording is filed under a hash of the state and the questions
//! ([`crate::eval::request_hash`]), and an extra field can change the
//! answer, so if extras ever cross the trait they must enter that hash, with
//! an empty set hashing as it does today; otherwise a replay would return
//! the answer to a different request. [`Client::list_models`] takes no
//! options either; the default headers reach it.
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

use reqwest::header::{
    AUTHORIZATION, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, HOST, HeaderMap, TE,
    TRANSFER_ENCODING, UPGRADE, USER_AGENT,
};
use reqwest::{RequestBuilder, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::answer::{Answer, Response, sanitize_server_str};
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
/// The header types [`CallOptions::header`] and
/// [`ClientBuilder::default_header`] take, re-exported from `reqwest` so a
/// caller needs no direct dependency on it.
pub use reqwest::header::{HeaderName, HeaderValue};

/// The retry-count header both official SDKs send on a retry and strip from
/// a caller's headers. This release does not send it; it is reserved so that
/// sending it later is not a break for a caller who set it.
const RETRY_COUNT_HEADER: HeaderName = HeaderName::from_static("x-typesafe-retry-count");
/// Every header the client sets itself, plus the one both SDKs own (module
/// docs, `# Per-call options`). Refused as a per-call or default header.
const RESERVED_HEADERS: [HeaderName; 4] =
    [AUTHORIZATION, CONTENT_TYPE, USER_AGENT, RETRY_COUNT_HEADER];
/// The headers HTTP itself owns: they frame the body, manage the connection
/// or pick the virtual host, and the HTTP stack keeps a caller's value over
/// the one it would compute (a short `content-length` truncates the body).
/// Refused like [`RESERVED_HEADERS`] (module docs, `# Per-call options`).
const TRANSPORT_HEADERS: [HeaderName; 6] = [
    CONTENT_LENGTH,
    TRANSFER_ENCODING,
    HOST,
    CONNECTION,
    TE,
    UPGRADE,
];
/// The body fields the client sets from the [`Request`]. Refused as extra
/// fields, matched exactly.
const RESERVED_FIELDS: [&str; 3] = ["state", "model", "questions"];

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

/// What one call to [`Client::evaluate_with`] changes from the client's own
/// settings: the per-attempt timeout, the retry policy, extra headers and
/// extra top-level body fields (module docs, `# Per-call options`).
///
/// Empty by default, and an empty set is exactly [`Client::evaluate`]: the
/// same body bytes, headers and span. Anything the client or HTTP sets
/// itself is refused when it is added, with [`Error::ReservedHeader`] or
/// [`Error::ReservedField`], so a set that was built can always be sent as
/// it reads.
/// The model is not here: it is already per call, on the [`Request`].
///
/// ```
/// use std::time::Duration;
/// use judgment::client::{CallOptions, HeaderName, HeaderValue};
/// use judgment::RetryPolicy;
///
/// # fn main() -> judgment::Result<()> {
/// let options = CallOptions::new()
///     .timeout(Duration::from_secs(30))
///     .retry(RetryPolicy::conservative())
///     .header(
///         HeaderName::from_static("x-team"),
///         HeaderValue::from_static("billing"),
///     )?
///     .extra("beam_width", 4)?;
/// assert!(CallOptions::new().extra("model", "other").is_err());
/// # let _ = options;
/// # Ok(()) }
/// ```
///
/// `Debug` prints the timeout, the policy and the names of the headers and
/// extra fields, never their values, since a header can be a credential and
/// an extra field can be a piece of the state.
#[derive(Clone, Default)]
pub struct CallOptions {
    timeout: Option<Duration>,
    retry: Option<RetryPolicy>,
    headers: HeaderMap,
    extra: Map<String, Value>,
}

impl fmt::Debug for CallOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallOptions")
            .field("timeout", &self.timeout)
            .field("retry", &self.retry)
            .field("headers", &header_names(&self.headers))
            .field("extra", &self.extra.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl CallOptions {
    /// No options: what [`Client::evaluate`] sends.
    pub fn new() -> Self {
        Self::default()
    }

    /// The timeout of each attempt of this call, from connecting to the end
    /// of the body, in place of the client's (not the shorter of the two:
    /// a call may be given longer than the client's default as well as
    /// shorter). It is per attempt, like the client's, so with retries the
    /// call can take longer. [`RetryPolicy::budget`] stops any retry whose
    /// wait would end at or past the budget, but it never cuts an attempt in
    /// flight, so a call can run to just under the budget plus one
    /// per-attempt timeout (`# The budget` on [`RetryPolicy`]); wrap the call
    /// in `tokio::time::timeout` for a hard deadline. Not validated, like
    /// [`ClientBuilder::timeout`].
    #[must_use]
    pub fn timeout(mut self, per_attempt: Duration) -> Self {
        self.timeout = Some(per_attempt);
        self
    }

    /// The retry policy of this call, in place of the client's whole policy,
    /// as the Python SDK does. The JS SDK merges a partial policy field by
    /// field instead; the Rust spelling of that merge is struct update
    /// syntax over the client's own policy:
    ///
    /// ```
    /// # use judgment::{Client, RetryPolicy, client::CallOptions};
    /// # fn run(client: &Client) {
    /// let once_more = CallOptions::new().retry(RetryPolicy {
    ///     max_retries: 1,
    ///     ..client.retry().clone()
    /// });
    /// # let _ = once_more; }
    /// ```
    ///
    /// A budget, when wanted, comes with the policy given here.
    #[must_use]
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = Some(policy);
        self
    }

    /// A header sent on every attempt of this call. It replaces a
    /// [`ClientBuilder::default_header`] of the same name, and a second call
    /// with the same name replaces the first; names are case-insensitive.
    ///
    /// A header the client sets itself is [`Error::ReservedHeader`]:
    /// `authorization`, `content-type`, `user-agent` and
    /// `x-typesafe-retry-count`, and so is one HTTP owns: `content-length`,
    /// `transfer-encoding`, `host`, `connection`, `te` and `upgrade` (module
    /// docs, `# Per-call options`). Mark a secret value with
    /// [`HeaderValue::set_sensitive`], so `reqwest` and anything printing the
    /// request with `{:?}` redact it; this type never prints header values
    /// either way. The client follows no redirect (module docs,
    /// `# Defaults`), so a header is sent to the base URL's host only.
    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Result<Self> {
        refuse_reserved_header(&name)?;
        self.headers.insert(name, value);
        Ok(self)
    }

    /// A top-level body field sent beside `state`, `model` and `questions`,
    /// for a server parameter this crate does not model. `null` is sent as
    /// `null`, and a second value with the same name replaces the first.
    ///
    /// `state`, `model` and `questions` are [`Error::ReservedField`], matched
    /// exactly, so `Model` is allowed. The server decides what an unknown
    /// field means: it may use it, ignore it or refuse the request with a
    /// 400 or 422.
    pub fn extra(mut self, name: impl Into<String>, value: impl Into<Value>) -> Result<Self> {
        let name = name.into();
        if RESERVED_FIELDS.contains(&name.as_str()) {
            return Err(Error::ReservedField(name));
        }
        self.extra.insert(name, value.into());
        Ok(self)
    }

    /// Put this call's headers and timeout on one attempt's request. Called
    /// inside the retry loop's `make`, so every attempt carries them.
    fn apply(&self, mut rb: RequestBuilder) -> RequestBuilder {
        if !self.headers.is_empty() {
            rb = rb.headers(self.headers.clone());
        }
        if let Some(timeout) = self.timeout {
            rb = rb.timeout(timeout);
        }
        rb
    }
}

/// `Err(ReservedHeader)` when `name` is one the client sets itself or one
/// HTTP owns.
fn refuse_reserved_header(name: &HeaderName) -> Result<()> {
    if RESERVED_HEADERS.contains(name) || TRANSPORT_HEADERS.contains(name) {
        return Err(Error::ReservedHeader(name.as_str().to_owned()));
    }
    Ok(())
}

/// The names in `headers`, once each and in order, for a `Debug` that must
/// not print values.
fn header_names(headers: &HeaderMap) -> Vec<&str> {
    headers.keys().map(HeaderName::as_str).collect()
}

/// The body [`Client::evaluate_with`] sends: the request's fields, then the
/// extra ones. Without extras it serialises to the same bytes as the
/// [`Request`] alone, so a call without options is on the wire exactly what
/// it was before options existed (pinned by a unit test).
#[derive(Serialize)]
struct Body<'r, 'a, S: Serialize> {
    #[serde(flatten)]
    request: &'r Request<'a, S>,
    #[serde(flatten)]
    extra: &'r Map<String, Value>,
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
    headers: HeaderMap,
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
            .field("default_headers", &header_names(&self.headers))
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
            headers: HeaderMap::new(),
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
    /// observer: from the shared retry loop in [`crate::http`] by status or
    /// as `transport`, and from this client as `decode` or `unfit` for a 2xx
    /// it could not use (module docs, `# Errors`).
    #[must_use]
    pub fn observer(mut self, observer: Arc<dyn Observer>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// A header sent on every request this client makes, the models list
    /// included: a routing or tenant header a gateway in front of the API
    /// wants, for instance. A [`CallOptions::header`] of the same name
    /// replaces it for one call, and a second default with the same name
    /// replaces the first; names are case-insensitive.
    ///
    /// A header the client sets itself (`authorization`, `content-type`,
    /// `user-agent`, `x-typesafe-retry-count`) or one HTTP owns
    /// (`content-length`, `transfer-encoding`, `host`, `connection`, `te`,
    /// `upgrade`) is refused by [`build`](Self::build) with
    /// [`Error::ReservedHeader`], beside the key and URL checks, so this
    /// method can stay infallible and the refusal still comes before
    /// anything is sent. Mark a secret value with
    /// [`HeaderValue::set_sensitive`]; the builder's `Debug` prints header
    /// names only. The client follows no redirect (module docs,
    /// `# Defaults`), so a header is sent to the base URL's host only.
    #[must_use]
    pub fn default_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
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
    /// is [`Error::Url`]. A [`default_header`](Self::default_header) the
    /// client or HTTP sets itself is [`Error::ReservedHeader`].
    pub fn build(self) -> Result<Client> {
        let api_key = resolve_api_key(self.api_key, || std::env::var(API_KEY_ENV))?;
        let base_url = Url::parse(&self.base_url).map_err(|e| Error::Url(e.to_string()))?;
        for name in self.headers.keys() {
            refuse_reserved_header(name)?;
        }
        // Unreachable after `resolve_api_key` (printable ASCII always fits a
        // header), kept so a future change to that check cannot turn a bad
        // key into a panic or back into the URL error it used to be.
        let mut auth = HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|_| {
            Error::InvalidApiKey {
                reason: "the key cannot be sent in an HTTP header".into(),
            }
        })?;
        auth.set_sensitive(true);
        // The caller's defaults first, then what the client owns; none of
        // the caller's can collide with it after the check above.
        let mut headers = self.headers;
        headers.insert(AUTHORIZATION, auth);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(self.timeout)
            .user_agent(concat!("judgment/", env!("CARGO_PKG_VERSION")))
            // A 3xx is an error, never followed (module docs, `# Defaults`).
            .redirect(reqwest::redirect::Policy::none())
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

    /// The default model: the one [`Client::system_one`] sends, and the
    /// value to put on a [`Request`] built for [`Client::evaluate_with`]
    /// when the call wants no other. [`SystemOne::answer`](crate::SystemOne::answer)
    /// sends the model it is given instead.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The client's retry policy, which every call uses unless
    /// [`CallOptions::retry`] replaces it; the starting point for a per-call
    /// policy that changes one field.
    pub fn retry(&self) -> &RetryPolicy {
        &self.retry
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

    /// Evaluate a fully specified request with the client's own settings:
    /// [`Client::evaluate_with`] with no [`CallOptions`].
    ///
    /// The response's [`Response::request_id`] is the last attempt's
    /// `x-typesafe-request-id`, and it is recorded on the span, on success
    /// and on failure (module docs, `# Request id`).
    pub async fn evaluate<S: Serialize + Sync>(
        &self,
        request: &Request<'_, S>,
    ) -> Result<Response> {
        self.evaluate_with(request, &CallOptions::default()).await
    }

    /// Evaluate a fully specified request with per-call options: a timeout,
    /// a retry policy, headers or extra body fields for this call only
    /// (module docs, `# Per-call options`). Without options it is exactly
    /// [`Client::evaluate`].
    ///
    /// The `typesafe.evaluate` span is this method's, so there is one per
    /// call whichever of the two was called. None of the options is a span
    /// field: a header can be a credential and an extra field a piece of
    /// the state. The response's [`Response::request_id`] is the last
    /// attempt's `x-typesafe-request-id`, recorded on the span on success
    /// and on failure (module docs, `# Request id`).
    ///
    /// The response is verified against `request.questions` before it is
    /// returned ([`Response::verify`]): one that does not answer them is an
    /// error, not retried, with its usage reported and the attempt counted
    /// as `unfit` (module docs, `# Errors`).
    #[tracing::instrument(
        name = "typesafe.evaluate",
        skip_all,
        fields(
            model = request.model,
            input_tokens = tracing::field::Empty,
            request_id = tracing::field::Empty
        )
    )]
    pub async fn evaluate_with<S: Serialize + Sync>(
        &self,
        request: &Request<'_, S>,
        options: &CallOptions,
    ) -> Result<Response> {
        let url = self.url("v1/systemone")?;
        let body = serde_json::to_vec(&Body {
            request,
            extra: &options.extra,
        })?;
        let policy = options.retry.as_ref().unwrap_or(&self.retry);
        let reply = self
            .send(policy, || {
                options.apply(self.http.post(url.clone()).body(body.clone()))
            })
            .await
            .inspect_err(|e| record_request_id(e.request_id()))?;
        record_request_id(reply.request_id.as_deref());
        let mut response: Response = reply.decode()?;
        // The field means "the header": a body key of the same name is
        // overwritten, with `None` when the header was absent.
        response.request_id = reply.request_id;
        // Inside this method's span, so the event carries `model` and
        // `request_id` from it. The kind is the server's string, and so is
        // the key of an answer to a question that was not asked (`verify`
        // walks only the asked ones): both escaped and cut.
        for (id, answer) in &response.answers {
            if matches!(answer, Answer::Unknown(_)) {
                tracing::warn!(
                    question = %sanitize_server_str(id),
                    kind = %sanitize_server_str(answer.kind()),
                    "answer of a kind this client does not know; kept as Answer::Unknown"
                );
            }
        }
        tracing::Span::current().record("input_tokens", response.usage.input_tokens);
        // Billed whether or not it fits, so reported before the check.
        self.observer().on_usage(&response.model, &response.usage);
        // Not retried: the call went through, and another attempt is billed
        // again for an answer no likelier to fit.
        if let Err(e) = response.verify(request.questions) {
            crate::observer::global().on_failed_attempt("typesafe", "unfit");
            return Err(e);
        }
        Ok(response)
    }

    /// List the models and aliases this account may send (`GET /v1/models`,
    /// in the OpenAPI document; module docs, `` # `GET /v1/models` ``). A
    /// server that does not serve it answers [`Error::Http`] with status 404.
    ///
    /// The list carries no request id; the `typesafe.list_models` span and
    /// every error do. It takes no [`CallOptions`]: it sends the client's
    /// default headers under the client's retry policy.
    #[tracing::instrument(
        name = "typesafe.list_models",
        skip_all,
        fields(request_id = tracing::field::Empty)
    )]
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let url = self.url("v1/models")?;
        let reply = self
            .send(&self.retry, || self.http.get(url.clone()))
            .await
            .inspect_err(|e| record_request_id(e.request_id()))?;
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
    ///
    /// A failure is also reported to the process-wide observer as a failed
    /// attempt with status `decode`: the retry loop counted the 2xx as a
    /// success, so without this a server answering in the wrong shape would
    /// show no failures at all. It is not retried.
    fn decode<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.body).map_err(|source| {
            crate::observer::global().on_failed_attempt("typesafe", "decode");
            Error::Decode {
                source,
                request_id: self.request_id.clone(),
            }
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
/// `detail` list joined as `path: msg; …`, when that is not empty; then the
/// body itself. The issues are parsed from a `detail` list whatever supplies
/// the message, so code gets them even when the server also sent prose. An
/// entry without a string `msg` is skipped, a missing or non-list `loc` is
/// an empty location, a non-string location item keeps its JSON text, and a
/// missing `type` is an empty kind. A body that is a JSON string is the
/// message itself, as in the SDK, unless it is empty; a blank one is kept,
/// since the SDK keeps it too.
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
        // Issues that join to nothing (an empty `msg` at an empty path) are
        // no message either, as in the SDK (`"; ".join(parts) or None`).
        .filter(|message| !message.is_empty())
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
/// [`REQUEST_ID_HEADER`], trimmed, when it is printable ASCII, not empty,
/// and at most [`REQUEST_ID_MAX_LEN`] bytes. `to_str` refuses CR, LF, DEL
/// and any byte that is not ASCII but lets a tab through, so a tab left
/// inside the value after trimming is refused here; a space inside it is
/// printable and kept. A value that is too long is dropped and logged at
/// `debug` with its length only.
fn read_request_id(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(REQUEST_ID_HEADER)?.to_str().ok()?.trim();
    if v.is_empty() || v.bytes().any(|b| b.is_ascii_control()) {
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
        // `to_str` lets a tab through; one left inside after trimming is
        // not printable, so the value is refused. A space is printable.
        assert_eq!(read(HeaderValue::from_static("req\t1")), None, "inner tab");
        assert_eq!(
            read(HeaderValue::from_static("\treq_3\t")).as_deref(),
            Some("req_3"),
            "outer tabs are trimmed"
        );
        assert_eq!(
            read(HeaderValue::from_static("req 1")).as_deref(),
            Some("req 1"),
            "inner space kept"
        );
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
        // Issues that join to nothing are no message: the raw body, as in
        // the SDK, with the issue still parsed.
        let body = r#"{"detail":[{"loc":["body"],"msg":"","type":"x"}]}"#;
        let (message, issues) = detail(body);
        assert_eq!(message, body);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].msg, "");
        // Two of them join to "; ", which is kept, as the SDK keeps it.
        let (message, _) =
            detail(r#"{"detail":[{"loc":["body"],"msg":""},{"loc":["body"],"msg":""}]}"#);
        assert_eq!(message, "; ");

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

    fn questions() -> Questions {
        let mut q = Questions::new();
        q.noul("urgent", "Does `message` convey urgency?", None)
            .unwrap();
        q.score("severity", "How bad is `message`?", ["minor", "major"])
            .unwrap();
        q
    }

    fn body<S: Serialize>(request: &Request<'_, S>, options: &CallOptions) -> Vec<u8> {
        serde_json::to_vec(&Body {
            request,
            extra: &options.extra,
        })
        .unwrap()
    }

    #[test]
    fn a_call_without_extras_is_the_request_byte_for_byte() {
        let q = questions();
        let object = serde_json::json!({ "message": "help", "nested": { "b": 1, "a": [2, 3] } });
        let text = "Help! My payouts have been failing.";
        let none = CallOptions::new();
        // Headers, a timeout and a retry policy change nothing in the body.
        let no_extras = CallOptions::new()
            .timeout(Duration::from_secs(1))
            .retry(RetryPolicy::none())
            .header(
                HeaderName::from_static("x-team"),
                HeaderValue::from_static("a"),
            )
            .unwrap();
        let object_request = Request {
            state: &object,
            model: "jev-latest",
            questions: &q,
        };
        let text_request = Request {
            state: &text,
            model: "jev-latest",
            questions: &q,
        };
        for options in [&none, &no_extras] {
            assert_eq!(
                body(&object_request, options),
                serde_json::to_vec(&object_request).unwrap()
            );
            assert_eq!(
                body(&text_request, options),
                serde_json::to_vec(&text_request).unwrap()
            );
        }
        // And extras come after the documented fields, which are unchanged.
        let with = CallOptions::new()
            .extra("beam_width", 4)
            .unwrap()
            .extra("tag", Value::Null)
            .unwrap();
        let sent = String::from_utf8(body(&text_request, &with)).unwrap();
        let bare = String::from_utf8(serde_json::to_vec(&text_request).unwrap()).unwrap();
        assert_eq!(
            sent,
            format!(
                "{},\"beam_width\":4,\"tag\":null}}",
                bare.strip_suffix('}').unwrap()
            )
        );
    }

    #[test]
    fn an_extra_field_may_not_replace_state_model_or_questions() {
        // The names themselves, not the constant: a name dropped from it
        // must fail here. A name added to it must be added here too.
        assert_eq!(RESERVED_FIELDS.len(), 3);
        for name in ["state", "model", "questions"] {
            let err = CallOptions::new().extra(name, "x").unwrap_err();
            assert!(
                matches!(&err, Error::ReservedField(n) if n == name),
                "{name}: {err:?}"
            );
            assert_eq!(
                err.to_string(),
                format!("body field {name:?} is set by the client and cannot be an extra field")
            );
            assert_eq!(err.request_id(), None);
        }
        // Matched exactly: another case is another field.
        let options = CallOptions::new()
            .extra("Model", "x")
            .unwrap()
            .extra("STATE", 1)
            .unwrap();
        assert_eq!(options.extra.len(), 2);
        // A second value replaces the first.
        let options = CallOptions::new()
            .extra("tag", 1)
            .unwrap()
            .extra("tag", 2)
            .unwrap();
        assert_eq!(options.extra.get("tag"), Some(&Value::from(2)));
    }

    #[test]
    fn the_client_owned_headers_are_refused_whatever_their_case() {
        let value = || HeaderValue::from_static("x");
        for spelled in [
            "Authorization",
            "AUTHORIZATION",
            "Content-Type",
            "content-type",
            "User-Agent",
            "X-TypeSafe-Retry-Count",
            "x-typesafe-retry-count",
            // Owned by HTTP: a caller's value would reframe the body or
            // pick another virtual host.
            "Content-Length",
            "content-length",
            "Transfer-Encoding",
            "Host",
            "HOST",
            "Connection",
            "TE",
            "Upgrade",
        ] {
            let name = HeaderName::from_bytes(spelled.as_bytes()).unwrap();
            let lower = spelled.to_ascii_lowercase();
            let expected =
                format!("header {lower:?} is set by the client and cannot be overridden");

            let err = CallOptions::new()
                .header(name.clone(), value())
                .unwrap_err();
            assert!(
                matches!(&err, Error::ReservedHeader(n) if *n == lower),
                "{spelled}: {err:?}"
            );
            assert_eq!(err.to_string(), expected);
            assert_eq!(err.request_id(), None);

            // As a default header, refused at build(), before any request:
            // nothing listens on the base URL.
            let err = Client::builder()
                .api_key("sk-test")
                .base_url("http://127.0.0.1:1")
                .default_header(HeaderName::from_static("x-team"), value())
                .default_header(name, value())
                .build()
                .unwrap_err();
            assert!(
                matches!(&err, Error::ReservedHeader(n) if *n == lower),
                "{spelled}: {err:?}"
            );
        }
        // The key is still checked first.
        let err = Client::builder()
            .api_key(" ")
            .default_header(AUTHORIZATION, value())
            .build()
            .unwrap_err();
        assert!(matches!(err, Error::MissingApiKey), "{err:?}");
        // Any other header is fine.
        CallOptions::new()
            .header(HeaderName::from_static("x-team"), value())
            .unwrap();
        Client::builder()
            .api_key("sk-test")
            .default_header(HeaderName::from_static("x-team"), value())
            .build()
            .unwrap();
    }

    #[test]
    fn debug_output_names_headers_and_extras_but_never_their_values() {
        let mut secret = HeaderValue::from_static("SECRET-HEADER");
        secret.set_sensitive(true);
        let options = CallOptions::new()
            .timeout(Duration::from_millis(250))
            .header(HeaderName::from_static("x-gateway-token"), secret.clone())
            .unwrap()
            .header(
                HeaderName::from_static("x-team"),
                HeaderValue::from_static("PLAIN-HEADER"),
            )
            .unwrap()
            .extra("beam_width", "SECRET-EXTRA")
            .unwrap();
        let dbg = format!("{options:?}");
        for name in ["x-gateway-token", "x-team", "beam_width", "250ms"] {
            assert!(dbg.contains(name), "{name}: {dbg}");
        }
        for value in ["SECRET-HEADER", "PLAIN-HEADER", "SECRET-EXTRA"] {
            assert!(!dbg.contains(value), "{value}: {dbg}");
        }

        let builder = Client::builder()
            .api_key("sk-secret")
            .default_header(HeaderName::from_static("x-gateway-token"), secret)
            .default_header(
                HeaderName::from_static("x-team"),
                HeaderValue::from_static("PLAIN-HEADER"),
            );
        let dbg = format!("{builder:?}");
        assert!(dbg.contains("x-gateway-token"), "{dbg}");
        assert!(dbg.contains("x-team"), "{dbg}");
        for value in ["SECRET-HEADER", "PLAIN-HEADER", "sk-secret"] {
            assert!(!dbg.contains(value), "{value}: {dbg}");
        }
        let dbg = format!("{:?}", builder.build().unwrap());
        for value in ["SECRET-HEADER", "PLAIN-HEADER", "sk-secret"] {
            assert!(!dbg.contains(value), "{value}: {dbg}");
        }
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
