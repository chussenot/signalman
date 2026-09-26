//! HTTP plumbing any client over `reqwest` can share: retry policy, backoff,
//! the server's wait (`retry-after-ms` and `Retry-After`) and a send loop
//! that retries transient failures.
//!
//! It is public so that one loop serves every upstream: the TypeSafe client
//! uses it, and the application this crate was extracted from runs two more
//! clients through it, so the retry rules and the failure counting are
//! written once and every upstream behaves the same way under failure. The
//! loop returns the last response, headers included; each client classifies
//! it into its own error type and reads its own upstream's headers from it.
//! Nothing here names a header or a status that belongs to one vendor:
//! `retry-after-ms` is read because it is a convention several vendors'
//! SDKs share, not a TypeSafe identifier, and the loop needs the wait before
//! it sleeps.
//!
//! The defaults follow the official TypeSafe SDKs'; where this module
//! differs from them, the total budget and the server-wait cap among them,
//! is stated on [`RetryPolicy`].

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use reqwest::header::{DATE, HeaderMap, HeaderName, RETRY_AFTER};

/// The server's wait in milliseconds. Not a registered header: a convention
/// of the `OpenAI` and Anthropic SDKs that both TypeSafe SDKs read too, and
/// it wins over `Retry-After` when both are sent.
const RETRY_AFTER_MS: HeaderName = HeaderName::from_static("retry-after-ms");

/// Which transport failures (no usable response) a [`RetryPolicy`] retries.
///
/// Three levels, because the question that matters for a billed call is
/// whether the request left the process. A connection that was never made
/// sent nothing and cannot have been processed; a timeout, a reset or a
/// body cut short may have been, and retrying it may pay for the same work
/// twice. The SDKs split transport failures by kind instead (a connection
/// error, a timeout), which does not answer that question, so one setting
/// with three values replaces their two flags.
///
/// Non-exhaustive, so a new level is an addition rather than a break.
///
/// [`BeforeSend`](Self::BeforeSend) relies on reqwest's
/// [`is_connect`](reqwest::Error::is_connect). An error reqwest does not
/// mark as a connect error is not retried, so a misclassification can only
/// make it retry less, never retry a request that was sent. A connect or
/// TLS handshake that hangs until the per-attempt timeout ends as a timeout,
/// not as a connect error, and is not retried either: no client in this
/// crate sets a separate connect timeout, because doing so would change the
/// defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum TransportRetry {
    /// Retry every transport failure: a refused or reset connection, a DNS
    /// or TLS failure, a timeout, and a response whose body could not be
    /// read. The SDKs' default.
    #[default]
    Any,
    /// Retry only a failure to connect, when nothing was sent.
    BeforeSend,
    /// Never retry a transport failure.
    Never,
}

/// Retry behaviour: which failures are retried, how many times, how long
/// between attempts, how far to trust the server's own wait, and, when set,
/// how long retrying may go on in total.
///
/// The defaults follow the official SDKs': two retries, 0.5 s doubling to
/// 5 s with up to a quarter of each wait taken off at random, and 408, 429,
/// every 5xx and every transport failure retried. The total budget and the
/// server-wait cap differ, and so do a few edge rules; all of them are
/// listed below, under "Parity with the official SDKs". Two retries also
/// bound the worst case: three attempts, each under the client's
/// per-attempt timeout, plus two waits of at most `retry_after_max` each
/// when the server names a wait, or at most 1.5 s of backoff in total when
/// it does not.
///
/// What each field protects against, at its extremes:
///
/// * `max_retries` at 0 turns every blip into a failed call; set high, it
///   keeps a caller waiting on an upstream that is down and multiplies load
///   on one that is overloaded.
/// * `backoff_initial` and `backoff_max` too small hammer an upstream that
///   asked for room (at zero, retries follow each other with no wait); too
///   large make every recoverable failure slow.
/// * `backoff_jitter` at 0 lets many clients that failed together retry in
///   lockstep; at 1 a wait can shrink to nothing. Jitter only ever shortens
///   a wait, so the nominal backoff is also the longest one. A value above 1
///   is read as 1, and a negative or `NaN` value as 0, rather than refused.
/// * `http_statuses` too wide retries what a retry cannot fix (a 400, 401,
///   403 or 422 is a request body, a key or an account's access) and adds
///   load for nothing; too narrow fails a call a second attempt would have
///   completed. The default is 408, 429 and 500 to 599, TypeSafe's 529
///   included. A 2xx is never retried, even when listed: re-sending a call
///   that succeeded would pay for it again.
/// * `retry_after_max` is the ceiling on how long the server may ask the
///   client to wait. The server knows better than the client how long to
///   back off, so its wait wins when present, but a hostile or
///   misconfigured header must not stall a caller for minutes; above the cap
///   the client's own backoff applies instead. At zero only a zero wait is
///   taken from the server.
/// * `transport` at [`TransportRetry::Any`] retries a failure that may have
///   reached the server; at [`TransportRetry::Never`] a dropped connection
///   fails the call.
/// * `budget` too short turns a slow upstream into a failed call after one
///   attempt; `None`, the default, leaves the bound to the attempt count.
/// * `max_body_bytes` is the most of a response body the loop will buffer,
///   8 MiB by default: a `Content-Length` above it fails the call before a
///   byte is read, and a body without one is read until it passes the cap.
///   Every body is decoded from that buffer, so an upstream that answers
///   with far more than it should, or a proxy that answers in its place,
///   cannot grow the process without bound. Too small fails a legitimate
///   page; no upstream this crate serves sends a body near the default. The
///   failure is [`Exhausted::TooLarge`], never retried: the next attempt
///   would carry the same body.
///
/// # The server's wait
///
/// Read from each failed response, in this order: `retry-after-ms`
/// (milliseconds), `Retry-After` in seconds, then `Retry-After` as an HTTP
/// date in any of the three RFC 9110 forms (`IMF-fixdate`, RFC 850,
/// asctime). A date is measured against the response's own `Date` header
/// when it has one that parses, so a local clock that disagrees with the
/// server's neither stretches nor cuts the wait; otherwise against this
/// machine's clock. A date in the past means retry now. A value that is
/// empty, negative, not finite or too large for a [`Duration`] is ignored:
/// a bad `retry-after-ms` leaves `Retry-After` to decide, and a bad
/// `Retry-After` leaves the backoff. The wait is honoured on any retried status, not
/// only 429, and replaces the backoff with no jitter: the server named a
/// time, and shortening it would retry before the server said it was ready.
/// [`parse_retry_after`] is the parser.
///
/// # The budget
///
/// `budget` counts from the first send: every attempt and every wait.
/// Before each wait the loop checks whether the time elapsed plus the wait
/// would reach the budget, and if so returns the last failure now instead of
/// waiting (tenacity's `stop_before_delay`, which the Python SDK uses). It
/// never cuts an attempt in flight, so a call can run to just under the
/// budget plus one per-attempt timeout; a caller that needs a hard deadline
/// wraps the call in `tokio::time::timeout`, which composes with any policy.
/// `Some(Duration::ZERO)` means no retries. A server wait longer than the
/// time left returns the failure at once, so a 429 comes back as a rate
/// limit error carrying the server's wait, which the caller can still
/// honour. A budget that stops a retry logs `retry budget spent; returning
/// the last failure` at `warn`.
///
/// It is off by default: the attempt count and the per-attempt timeout
/// already bound a call, the JS SDK has no budget either, and a default one
/// would change the behaviour of every client that shares this loop.
///
/// ```
/// use std::time::Duration;
/// use judgment::{Client, RetryPolicy};
///
/// // Up to four retries, but none that would start 20 s or more after
/// // the first send.
/// let policy = RetryPolicy {
///     max_retries: 4,
///     budget: Some(Duration::from_secs(20)),
///     ..RetryPolicy::default()
/// };
/// let builder = Client::builder().retry(policy);
/// # let _ = builder;
/// ```
///
/// # Parity with the official SDKs (Python 0.7.1, JS 0.6.0)
///
/// Matches both, unless one is named:
///
/// * `max_retries` 2, and 0 disables retries.
/// * Backoff from 0.5 s doubling to 5 s, with jitter 0.25 that is only
///   subtracted: a wait is `backoff × (1 − U·j)` for a uniform `U` in
///   `[0, 1)`.
/// * A settable status set, by default 408, 429 and 500 to 599. A 2xx is
///   never retried, even when listed (the JS SDK returns on `res.ok` before
///   it reads the set; the Python SDK retries only a raised error).
/// * A per-call policy: [`crate::client::CallOptions::retry`] on
///   [`crate::Client::evaluate_with`] replaces the client's whole policy for
///   that call, as the Python SDK does (the JS SDK merges field by field;
///   `CallOptions::retry` shows the struct-update spelling of that).
/// * The server's wait read as `retry-after-ms`, then `Retry-After` in
///   seconds, then as a date; a past date means 0; non-finite values are
///   ignored.
/// * The server's wait replaces the backoff, with no jitter.
/// * A server wait above the cap falls back to the backoff (the JS SDK's
///   `maxRetryAfterMs`; the Python SDK has no cap).
/// * Every transport failure is retried by default, a body that could not
///   be read included.
/// * The budget, when set, stops before a wait that would reach it and
///   returns the last failure (the Python SDK's `timeout`).
/// * Dropping the future cancels the call, a wait included.
///
/// Deliberately differs:
///
/// * The budget is off by default (Python: 30 s; JS: none).
/// * The cap is 30 s (JS: 60 s; Python: none).
/// * An HTTP date is measured against the response's `Date` header when it
///   has one (the SDKs use the local clock).
/// * An empty header is ignored (the SDKs read it as 0 and retry at once).
/// * One three-level `transport` setting instead of two flags
///   (`api_connection_error`, `api_timeout_error`); [`TransportRetry`] says
///   why.
/// * A jitter outside `[0, 1]` is clamped instead of raising an error.
/// * Waits are not rounded to milliseconds.
/// * `budget: Some(Duration::ZERO)` is accepted and means no retries (the
///   Python SDK refuses a timeout of 0 or less).
/// * A `retry-after-ms` too large for a [`Duration`] falls through to
///   `Retry-After` (the SDKs return it).
/// * A `Retry-After` in seconds that is too large for a [`Duration`] falls
///   back to the backoff, while the Python SDK honours it and then stops on
///   its 30 s budget.
/// * Dates are the three RFC 9110 forms only (JS `Date.parse` also takes
///   ISO 8601; Python's `email.utils.parsedate_to_datetime` also takes RFC
///   5322 forms, such as a numeric offset or no weekday).
/// * [`RetryPolicy::conservative`] has no SDK equivalent.
/// * `max_body_bytes` has no SDK equivalent either: both SDKs buffer
///   whatever the server sends.
///
/// Not implemented:
///
/// * `respect_retry_after`: set `retry_after_max` to zero instead. That
///   still takes a zero wait, so `Retry-After: 0` is retried immediately.
/// * Exception and predicate hooks: a closure field would cost the type its
///   `PartialEq` and `Debug`.
/// * `X-TypeSafe-Retry-Count`, which both SDKs send on a retry: not sent in
///   this release, but reserved (a caller header of that name is
///   [`crate::Error::ReservedHeader`]), so sending it later breaks no
///   caller.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryPolicy {
    /// Retries after the first attempt; 0 disables retries.
    pub max_retries: u32,
    /// First backoff delay; doubled each retry.
    pub backoff_initial: Duration,
    /// Cap on the computed backoff.
    pub backoff_max: Duration,
    /// Fraction of each backoff that may be taken off at random: 0.25 means
    /// a wait between 75 % and 100 % of the nominal backoff. Clamped to
    /// `[0, 1]`; `NaN` reads as 0.
    pub backoff_jitter: f64,
    /// HTTP statuses that are retried; any other status is returned at once.
    /// A 2xx is never retried, even when listed. Default: 408, 429 and 500
    /// to 599.
    pub http_statuses: BTreeSet<u16>,
    /// Longest server wait the client will honour; above it the client's
    /// own backoff applies. Prevents a hostile or misconfigured header from
    /// stalling a caller for minutes.
    pub retry_after_max: Duration,
    /// Which transport failures are retried; default [`TransportRetry::Any`].
    pub transport: TransportRetry,
    /// Time from the first send that retrying may use: no wait starts that
    /// would end at or beyond it. `None` (the default) for no budget; see
    /// "The budget" above.
    pub budget: Option<Duration>,
    /// Most bytes of a response body the loop buffers; over it the call
    /// fails as [`Exhausted::TooLarge`] without a retry. Default 8 MiB.
    pub max_body_bytes: usize,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            backoff_initial: Duration::from_millis(500),
            backoff_max: Duration::from_secs(5),
            backoff_jitter: 0.25,
            http_statuses: [408, 429].into_iter().chain(500..=599).collect(),
            retry_after_max: Duration::from_secs(30),
            transport: TransportRetry::Any,
            budget: None,
            max_body_bytes: 8 * 1024 * 1024,
        }
    }
}

impl RetryPolicy {
    /// No retries at all.
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    /// Retry only what cannot have been charged twice: 408, 429, and
    /// transport failures before the request left the process.
    ///
    /// A System One call is a billed POST: the OpenAPI document describes
    /// `Usage.input_tokens` as the "Number of billable input tokens", and the
    /// API reference does not say whether a call that ended in a 5xx or ran
    /// into a timeout was charged. A 408 (RFC 9110 §15.5.9) and a 429 (RFC
    /// 6585 §4) say the server did not process the request, and a connection
    /// that was never made sent nothing, so retrying those cannot pay twice.
    /// A 529 is excluded although the API reference gives it the same retry
    /// advice as a 429, because nothing there says an overloaded server
    /// refused the request before processing it.
    ///
    /// The cost: a call fails on the first 5xx or timeout that a retry would
    /// have saved. Set on the client, the policy applies to every call it
    /// makes, [`crate::Client::list_models`] included, unless a call's
    /// [`crate::client::CallOptions::retry`] replaces it. reqwest's own
    /// retry of an HTTP/2 request the server refused before processing
    /// still happens, below this policy.
    pub fn conservative() -> Self {
        Self {
            http_statuses: BTreeSet::from([408, 429]),
            transport: TransportRetry::BeforeSend,
            ..Self::default()
        }
    }

    /// Whether this policy retries `status`: it is not a success and it is
    /// in [`http_statuses`](Self::http_statuses). By default that is 408,
    /// 429 and every 5xx, transient by definition; 400, 401, 403 and 422
    /// are not retried, since a retry cannot fix a request body, a key or
    /// an account's access and would only add load. A 2xx is never retried,
    /// even when listed, as in both SDKs: the call succeeded, and a System
    /// One call is billed, so sending it again would pay for it twice.
    pub fn is_retryable(&self, status: StatusCode) -> bool {
        !status.is_success() && self.http_statuses.contains(&status.as_u16())
    }

    /// Whether this policy retries the transport failure `e`, by
    /// [`transport`](Self::transport).
    pub fn retries_transport(&self, e: &reqwest::Error) -> bool {
        match self.transport {
            TransportRetry::Any => true,
            TransportRetry::BeforeSend => e.is_connect(),
            TransportRetry::Never => false,
        }
    }

    /// Delay before retry number `retry` (1-based): the server's wait when
    /// it gave one within `retry_after_max`, otherwise the exponential
    /// backoff shortened by a random share of at most `backoff_jitter`.
    pub fn delay(&self, retry: u32, retry_after: Option<Duration>) -> Duration {
        self.delay_with(retry, retry_after, fastrand::f64())
    }

    /// [`delay`](Self::delay) with the random draw `unit` (in `[0, 1]`)
    /// passed in, so the arithmetic is testable.
    fn delay_with(&self, retry: u32, retry_after: Option<Duration>, unit: f64) -> Duration {
        if let Some(ra) = retry_after
            && ra <= self.retry_after_max
        {
            return ra;
        }
        let exp = self
            .backoff_initial
            .saturating_mul(2u32.saturating_pow(retry.saturating_sub(1)))
            .min(self.backoff_max);
        // `NaN > 0.0` is false, so a NaN jitter reads as no jitter.
        let jitter = if self.backoff_jitter > 0.0 {
            self.backoff_jitter.min(1.0)
        } else {
            0.0
        };
        if jitter == 0.0 {
            return exp;
        }
        // Never `mul_f64`: `Duration::MAX.mul_f64(1.0)` panics, since the
        // product rounds above the largest representable duration.
        let cut = Duration::try_from_secs_f64(exp.as_secs_f64() * unit.clamp(0.0, 1.0) * jitter)
            .unwrap_or(exp);
        exp.saturating_sub(cut)
    }

    /// The wait before the retry that follows attempt number `attempt`, or
    /// `None` when the policy stops here: retries are used up, or the
    /// budget would be reached by the time the wait ends.
    fn next_wait(
        &self,
        attempt: u32,
        retry_after: Option<Duration>,
        elapsed: Duration,
    ) -> Option<Duration> {
        if attempt > self.max_retries {
            return None;
        }
        let delay = self.delay(attempt, retry_after);
        if let Some(budget) = self.budget
            && elapsed.saturating_add(delay) >= budget
        {
            tracing::warn!(
                attempt,
                ?delay,
                ?elapsed,
                ?budget,
                "retry budget spent; returning the last failure"
            );
            return None;
        }
        Some(delay)
    }
}

/// The last response of a retry loop, ready for the caller to classify.
///
/// Non-exhaustive so the loop can hand back more of the response without a
/// breaking change; destructure it with `..`.
#[derive(Debug)]
#[non_exhaustive]
pub struct Completed {
    /// HTTP status.
    pub status: StatusCode,
    /// Response body as text.
    pub body: String,
    /// Total attempts made, including the first.
    pub attempts: u32,
    /// The last response's wait, from `retry-after-ms` or `Retry-After`
    /// (seconds or a date), when it named one ([`parse_retry_after`]).
    pub retry_after: Option<Duration>,
    /// The last response's headers. The loop itself reads only the retry
    /// headers; each client reads its own upstream's headers here (a request
    /// id, say), so this module stays free of any one vendor's names.
    /// Responses that were retried are dropped with their headers.
    pub headers: HeaderMap,
}

/// The retry loop gave up without a response the caller can classify.
///
/// Exhaustive on purpose: a new kind must be mapped by every client, the
/// way [`crate::Error::request_id`] names every variant.
#[derive(Debug)]
pub enum Exhausted {
    /// A transport-level failure the policy stopped retrying.
    Transport {
        /// Total attempts made, including the first.
        attempts: u32,
        /// The last error.
        source: reqwest::Error,
    },
    /// The last response's body was over the policy's
    /// [`max_body_bytes`](RetryPolicy::max_body_bytes): its `Content-Length`
    /// said so before a byte was read, or the read passed the cap. Never
    /// retried, since the next attempt would carry the same body; the
    /// response's status and headers are dropped with it.
    TooLarge {
        /// Total attempts made, including the first.
        attempts: u32,
        /// The cap that was passed, in bytes.
        limit: usize,
    },
}

/// Send `make()` until it yields a response the policy does not retry, or
/// the policy stops.
///
/// `make` is called once per attempt, in order, so it builds a fresh request
/// each time. A status the policy retries
/// ([`is_retryable`](RetryPolicy::is_retryable): in its
/// [`http_statuses`](RetryPolicy::http_statuses), never a 2xx) is retried;
/// a transport failure, or a body that could not be read, is retried as the
/// policy's [`transport`](RetryPolicy::transport) says. The policy stops when the
/// retries are used up or when the next wait would reach its
/// [`budget`](RetryPolicy::budget), measured from the first send.
///
/// Successful and non-retried statuses, and the last retried one when the
/// policy stops, return `Ok(Completed)` so the caller maps them; the last
/// transport failure, or a body over the policy's
/// [`max_body_bytes`](RetryPolicy::max_body_bytes), returns `Err(Exhausted)`.
/// The body is decoded as UTF-8, invalid sequences replaced: every upstream
/// this loop serves answers in JSON, which is UTF-8 by definition. `service`
/// labels every failed attempt reported to the global [`crate::Observer`]
/// (an application passes its own upstream names): the status, `transport`,
/// or `too_large`. Every failed attempt is reported, retried or not: a retry
/// that succeeds hides the failure from the caller, but the attempt was
/// still load on the upstream and still a symptom. The loop is the one place
/// every client passes through, so it is where the count lives.
pub async fn send_with_retries(
    policy: &RetryPolicy,
    service: &'static str,
    make: impl Fn() -> reqwest::RequestBuilder,
) -> Result<Completed, Exhausted> {
    let started = tokio::time::Instant::now();
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match make().send().await {
            Ok(mut resp) => {
                let status = resp.status();
                let retry_after = parse_retry_after(resp.headers());
                if !status.is_success() {
                    crate::observer::global().on_failed_attempt(service, status.as_str());
                }
                let headers = std::mem::take(resp.headers_mut());
                let body = match read_body(resp, policy.max_body_bytes).await {
                    Ok(b) => b,
                    Err(BodyRead::TooLarge) => {
                        crate::observer::global().on_failed_attempt(service, "too_large");
                        tracing::warn!(
                            attempt,
                            status = status.as_u16(),
                            limit = policy.max_body_bytes,
                            "response body over the cap; not retried"
                        );
                        return Err(Exhausted::TooLarge {
                            attempts: attempt,
                            limit: policy.max_body_bytes,
                        });
                    }
                    Err(BodyRead::Transport(source)) => {
                        crate::observer::global().on_failed_attempt(service, "transport");
                        if policy.retries_transport(&source)
                            && let Some(delay) = policy.next_wait(attempt, None, started.elapsed())
                        {
                            tracing::warn!(attempt, ?delay, error = %source, "body read failed; retrying");
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        return Err(Exhausted::Transport {
                            attempts: attempt,
                            source,
                        });
                    }
                };
                if policy.is_retryable(status)
                    && let Some(delay) = policy.next_wait(attempt, retry_after, started.elapsed())
                {
                    tracing::warn!(
                        attempt,
                        status = status.as_u16(),
                        ?delay,
                        "retryable status; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Ok(Completed {
                    status,
                    body,
                    attempts: attempt,
                    retry_after,
                    headers,
                });
            }
            Err(source) => {
                crate::observer::global().on_failed_attempt(service, "transport");
                if policy.retries_transport(&source)
                    && let Some(delay) = policy.next_wait(attempt, None, started.elapsed())
                {
                    tracing::warn!(attempt, ?delay, error = %source, "transport error; retrying");
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(Exhausted::Transport {
                    attempts: attempt,
                    source,
                });
            }
        }
    }
}

/// Why [`read_body`] stopped.
enum BodyRead {
    TooLarge,
    Transport(reqwest::Error),
}

/// Buffer a body of at most `limit` bytes: a `Content-Length` over the cap
/// fails before a read, otherwise chunks are read until they would pass it.
async fn read_body(mut resp: reqwest::Response, limit: usize) -> Result<String, BodyRead> {
    let declared = resp.content_length();
    if declared.is_some_and(|n| n > u64::try_from(limit).unwrap_or(u64::MAX)) {
        return Err(BodyRead::TooLarge);
    }
    let mut buf = Vec::with_capacity(declared.and_then(|n| usize::try_from(n).ok()).unwrap_or(0));
    while let Some(chunk) = resp.chunk().await.map_err(BodyRead::Transport)? {
        if buf.len().saturating_add(chunk.len()) > limit {
            return Err(BodyRead::TooLarge);
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8(buf)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

/// The server's wait, from a response's headers ([`RetryPolicy`], "The
/// server's wait").
///
/// In order: `retry-after-ms` when it is a finite, non-negative number of
/// milliseconds that fits a [`Duration`] (anything else falls through);
/// then `Retry-After` as a number of seconds, where a negative, non-finite
/// or unrepresentable number gives `None`; then `Retry-After` as an HTTP
/// date (`IMF-fixdate`, RFC 850 or asctime), measured against the
/// response's `Date` header when it parses and against this machine's clock
/// otherwise, a past date giving zero. An empty or unreadable value is
/// ignored. No input panics.
pub fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    parse_retry_after_at(headers, SystemTime::now())
}

/// [`parse_retry_after`] with the local clock passed in.
fn parse_retry_after_at(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    if let Some(ms) = header_str(headers, &RETRY_AFTER_MS)
        && let Ok(ms) = ms.parse::<f64>()
        && ms.is_finite()
        && ms >= 0.0
        && let Ok(wait) = Duration::try_from_secs_f64(ms / 1000.0)
    {
        return Some(wait);
    }
    let raw = header_str(headers, &RETRY_AFTER)?;
    if let Ok(secs) = raw.parse::<f64>() {
        // `try_from`, never `from_secs_f64`: `inf` or `1e20` would panic.
        return (secs.is_finite() && secs >= 0.0)
            .then(|| Duration::try_from_secs_f64(secs).ok())
            .flatten();
    }
    let at = httpdate::parse_http_date(raw).ok()?;
    let reference = header_str(headers, &DATE)
        .and_then(|d| httpdate::parse_http_date(d).ok())
        .unwrap_or(now);
    Some(at.duration_since(reference).unwrap_or(Duration::ZERO))
}

/// A header's value, trimmed; `None` when absent, not visible ASCII, or
/// empty.
fn header_str<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    let value = headers.get(name)?.to_str().ok()?.trim();
    (!value.is_empty()).then_some(value)
}

pub use crate::error::retry_after_suffix;

/// Truncate a response body for inclusion in an error message.
pub fn truncate(mut s: String) -> String {
    const MAX: usize = 2_000;
    if s.len() > MAX {
        let cut = s.floor_char_boundary(MAX);
        s.truncate(cut);
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use reqwest::header::HeaderValue;

    fn no_jitter() -> RetryPolicy {
        RetryPolicy {
            backoff_jitter: 0.0,
            ..RetryPolicy::default()
        }
    }

    fn headers(pairs: &[(&HeaderName, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (name, value) in pairs {
            h.insert((*name).clone(), HeaderValue::from_str(value).unwrap());
        }
        h
    }

    /// A whole second, so a date formatted from it parses back exactly.
    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn retry_after_wins_when_within_cap() {
        let p = RetryPolicy::default();
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        // Above the 30 s cap the backoff applies, and jitter only shortens it.
        let d = p.delay(1, Some(Duration::from_secs(600)));
        assert!(d <= Duration::from_millis(500), "{d:?}");
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let p = no_jitter();
        assert_eq!(p.delay(1, None), Duration::from_millis(500));
        assert_eq!(p.delay(2, None), Duration::from_millis(1000));
        assert_eq!(p.delay(3, None), Duration::from_millis(2000));
        assert_eq!(p.delay(10, None), Duration::from_secs(5));
    }

    #[test]
    fn jitter_only_subtracts() {
        let p = RetryPolicy::default();
        assert_eq!(p.delay_with(1, None, 0.0), Duration::from_millis(500));
        assert_eq!(p.delay_with(1, None, 1.0), Duration::from_millis(375));
        for _ in 0..1000 {
            let d = p.delay(1, None);
            assert!(
                (Duration::from_millis(375)..=Duration::from_millis(500)).contains(&d),
                "{d:?}"
            );
        }

        let nan = RetryPolicy {
            backoff_jitter: f64::NAN,
            ..RetryPolicy::default()
        };
        assert_eq!(nan.delay_with(1, None, 1.0), Duration::from_millis(500));
        let negative = RetryPolicy {
            backoff_jitter: -0.5,
            ..RetryPolicy::default()
        };
        assert_eq!(
            negative.delay_with(1, None, 1.0),
            Duration::from_millis(500)
        );
        let seven = RetryPolicy {
            backoff_jitter: 7.0,
            ..RetryPolicy::default()
        };
        assert_eq!(seven.delay_with(1, None, 1.0), Duration::ZERO);

        for jitter in [0.0, 0.25] {
            let huge = RetryPolicy {
                backoff_initial: Duration::MAX,
                backoff_max: Duration::MAX,
                backoff_jitter: jitter,
                ..RetryPolicy::default()
            };
            for unit in [0.0, 0.5, 1.0] {
                assert!(huge.delay_with(3, None, unit) <= Duration::MAX);
            }
            let _ = huge.delay(u32::MAX, None);
        }
    }

    #[test]
    fn statuses_default_and_conservative() {
        let status = |code| StatusCode::from_u16(code).unwrap();
        let default = RetryPolicy::default();
        for code in [408, 429, 500, 503, 529, 599] {
            assert!(default.is_retryable(status(code)), "{code}");
        }
        for code in [400, 401, 403, 404, 422, 600] {
            assert!(!default.is_retryable(status(code)), "{code}");
        }
        let conservative = RetryPolicy::conservative();
        for code in [408, 429] {
            assert!(conservative.is_retryable(status(code)), "{code}");
        }
        for code in [500, 503, 529] {
            assert!(!conservative.is_retryable(status(code)), "{code}");
        }
        assert_eq!(conservative.transport, TransportRetry::BeforeSend);
        assert_eq!(
            RetryPolicy {
                http_statuses: default.http_statuses.clone(),
                transport: TransportRetry::Any,
                ..conservative
            },
            default,
            "conservative() changes only the statuses and the transport level"
        );
    }

    #[test]
    fn a_listed_success_is_never_retried() {
        let status = |code| StatusCode::from_u16(code).unwrap();
        let everything = RetryPolicy {
            http_statuses: (100..=599).collect(),
            ..RetryPolicy::default()
        };
        for code in [200, 201, 204, 299] {
            assert!(!everything.is_retryable(status(code)), "{code}");
        }
        for code in [304, 404, 503] {
            assert!(everything.is_retryable(status(code)), "{code}");
        }
    }

    #[test]
    fn budget_stops_before_a_wait_that_reaches_it() {
        let ms = Duration::from_millis;
        let p = RetryPolicy {
            budget: Some(Duration::from_secs(1)),
            ..no_jitter()
        };
        // First wait is the 500 ms backoff: 400 + 500 stays under 1 s,
        // 500 + 500 reaches it.
        assert_eq!(p.next_wait(1, None, ms(400)), Some(ms(500)));
        assert_eq!(p.next_wait(1, None, ms(500)), None);
        // A server wait longer than the time left is not waited either.
        assert_eq!(p.next_wait(1, Some(Duration::from_secs(2)), ms(0)), None);
        // Retries used up stops with or without a budget.
        assert_eq!(p.next_wait(3, None, ms(0)), None);
        assert_eq!(no_jitter().next_wait(3, None, ms(0)), None);
        // No budget never stops on time.
        assert_eq!(
            no_jitter().next_wait(2, None, Duration::from_secs(3600)),
            Some(ms(1000))
        );
        // A zero budget allows no retry at all.
        let zero = RetryPolicy {
            budget: Some(Duration::ZERO),
            ..no_jitter()
        };
        assert_eq!(zero.next_wait(1, None, ms(0)), None);
        assert_eq!(zero.next_wait(1, Some(Duration::ZERO), ms(0)), None);
    }

    #[test]
    fn retry_after_forms_and_precedence() {
        let now = at(1_700_000_000);
        let parse = |pairs: &[(&HeaderName, &str)]| parse_retry_after_at(&headers(pairs), now);

        assert_eq!(
            parse(&[(&RETRY_AFTER, "2.5")]),
            Some(Duration::from_millis(2500))
        );
        assert_eq!(
            parse(&[(&RETRY_AFTER, " 3 ")]),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            parse(&[(&RETRY_AFTER_MS, "120")]),
            Some(Duration::from_millis(120))
        );
        assert_eq!(
            parse(&[(&RETRY_AFTER_MS, "120"), (&RETRY_AFTER, "7")]),
            Some(Duration::from_millis(120)),
            "retry-after-ms wins"
        );
        for fallthrough in ["-1", "soon", "", "inf", "1e400"] {
            assert_eq!(
                parse(&[(&RETRY_AFTER_MS, fallthrough), (&RETRY_AFTER, "7")]),
                Some(Duration::from_secs(7)),
                "retry-after-ms {fallthrough:?} falls through"
            );
        }
        assert_eq!(parse(&[(&RETRY_AFTER_MS, "-1")]), None);

        let ahead = httpdate::fmt_http_date(now + Duration::from_secs(4));
        assert_eq!(
            parse(&[(&RETRY_AFTER, &ahead)]),
            Some(Duration::from_secs(4))
        );
        assert_eq!(
            parse(&[(&RETRY_AFTER, "Wed, 21 Oct 2015 07:28:00 GMT")]),
            Some(Duration::ZERO),
            "a past date means retry now"
        );

        // The response's own Date header is the reference, whatever the
        // local clock says.
        let retry_at = at(1_600_000_000);
        let pairs = [
            (&RETRY_AFTER, httpdate::fmt_http_date(retry_at)),
            (
                &DATE,
                httpdate::fmt_http_date(retry_at - Duration::from_secs(10)),
            ),
        ];
        let pairs: Vec<(&HeaderName, &str)> = pairs.iter().map(|(n, v)| (*n, v.as_str())).collect();
        for local in [now, at(0), at(1_600_000_005)] {
            assert_eq!(
                parse_retry_after_at(&headers(&pairs), local),
                Some(Duration::from_secs(10))
            );
        }
        // A Date header that does not parse is ignored.
        assert_eq!(
            parse(&[(&RETRY_AFTER, &ahead), (&DATE, "yesterday")]),
            Some(Duration::from_secs(4))
        );

        // RFC 850 and asctime, the two obsolete forms RFC 9110 still accepts.
        let date = [(&DATE, "Sun, 06 Nov 1994 08:49:30 GMT")];
        for form in ["Sunday, 06-Nov-94 08:49:37 GMT", "Sun Nov  6 08:49:37 1994"] {
            assert_eq!(
                parse(&[(&RETRY_AFTER, form), date[0]]),
                Some(Duration::from_secs(7)),
                "{form}"
            );
        }
        assert_eq!(parse(&[]), None);
    }

    #[test]
    fn hostile_retry_after_is_ignored_not_a_panic() {
        // 0.1 called `Duration::from_secs_f64` on these, and `inf` or `1e20`
        // panicked inside the retry loop.
        for value in ["inf", "infinity", "NaN", "1e400", "-1", "", "soon", "1e20"] {
            let h = headers(&[(&RETRY_AFTER, value)]);
            assert_eq!(parse_retry_after(&h), None, "Retry-After {value:?}");
        }
        for value in ["inf", "infinity", "NaN", "1e400", "-1", "", "soon", "1e30"] {
            let h = headers(&[(&RETRY_AFTER_MS, value)]);
            assert_eq!(parse_retry_after(&h), None, "retry-after-ms {value:?}");
        }
        // 1e20 ms is about 3 billion years: representable, so parsed, and
        // then above any `retry_after_max`, so the backoff applies.
        let h = headers(&[(&RETRY_AFTER_MS, "1e20")]);
        let huge = parse_retry_after(&h).unwrap();
        assert!(RetryPolicy::default().delay(1, Some(huge)) <= Duration::from_millis(500));
        // Not visible ASCII: unreadable, so ignored.
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_bytes(b"\xff").unwrap());
        assert_eq!(parse_retry_after(&h), None);
    }
}
