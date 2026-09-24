//! HTTP plumbing any client over `reqwest` can share: retry policy, backoff,
//! `Retry-After` parsing and a send loop that retries transient failures.
//!
//! It is public so that one loop serves every upstream: the TypeSafe client
//! uses it, and the application this crate was extracted from runs two more
//! clients through it, so the retry rules and the failure counting are
//! written once and every upstream behaves the same way under failure. The
//! loop returns the last response; each client classifies it into its own
//! error type.

use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, RETRY_AFTER};

/// Retry behaviour: how many times, how long between, and how far to trust
/// the server's `Retry-After`.
///
/// The defaults are the TypeSafe Python SDK's `RetryPolicy` (two retries,
/// 0.5 s doubling to 5 s, ±25 % jitter), so a call behaves the same from
/// Rust as from Python. Two retries also bound the worst case: three
/// attempts, each under the client's per-attempt timeout, plus two waits of
/// at most `retry_after_max` each when the server sends a header, or under
/// two seconds of backoff when it does not.
///
/// What each field protects against, at its extremes:
///
/// * `max_retries` at 0 turns every blip into a failed call; set high, it
///   keeps a caller waiting on an upstream that is down and multiplies load on
///   one that is overloaded.
/// * `backoff_initial` and `backoff_max` too small hammer an upstream that
///   asked for room; too large make every recoverable failure slow.
/// * `backoff_jitter` at 0 lets many clients retry in lockstep; the ±25 %
///   default spreads them.
/// * `retry_after_max` is the ceiling on how long the server may ask the
///   client to wait. The server knows better than the client how long to back
///   off, so the header wins when present, but a hostile or misconfigured
///   header must not stall a caller for minutes; above the cap the client's
///   own backoff applies instead.
///
/// Only the delay-seconds form of `Retry-After` is read. The HTTP-date form
/// needs a comparison against a server clock that may not match this one,
/// and the backoff is a safe fallback, so the date is ignored rather than
/// trusted. The Python SDK also reads a `retry-after-ms` header; the HTTP API
/// reference documents neither header, and this crate reads only the
/// standard one.
///
/// There is no overall budget across attempts. The Python SDK has one (30 s
/// by default); here the bound comes from the attempt count and the
/// per-attempt timeout, and a caller that needs a hard deadline wraps the
/// call in its own timeout, which composes with any policy.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryPolicy {
    /// Retries after the first attempt; 0 disables retries.
    pub max_retries: u32,
    /// First backoff delay; doubled each retry.
    pub backoff_initial: Duration,
    /// Cap on the computed backoff.
    pub backoff_max: Duration,
    /// Jitter as a fraction of the delay (0.25 means ±25 %).
    pub backoff_jitter: f64,
    /// Longest `Retry-After` the client will honour; above it the client's
    /// own backoff applies. Prevents a hostile or misconfigured header from
    /// stalling a caller for minutes.
    pub retry_after_max: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            backoff_initial: Duration::from_millis(500),
            backoff_max: Duration::from_secs(5),
            backoff_jitter: 0.25,
            retry_after_max: Duration::from_secs(30),
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

    /// 408, 429 and every 5xx (which includes TypeSafe's 529) are transient
    /// by definition: a later attempt may succeed. 401 and 422 are not, since
    /// a retry cannot fix a key or a request body and would only add load.
    pub fn is_retryable(status: StatusCode) -> bool {
        status == StatusCode::REQUEST_TIMEOUT
            || status == StatusCode::TOO_MANY_REQUESTS
            || status.is_server_error()
    }

    /// Delay before retry number `retry` (1-based), preferring `Retry-After`.
    pub fn delay(&self, retry: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(ra) = retry_after
            && ra <= self.retry_after_max
        {
            return ra;
        }
        let exp = self
            .backoff_initial
            .saturating_mul(2u32.saturating_pow(retry.saturating_sub(1)))
            .min(self.backoff_max);
        if self.backoff_jitter <= 0.0 {
            return exp;
        }
        let factor = 1.0 + (fastrand::f64() * 2.0 - 1.0) * self.backoff_jitter;
        exp.mul_f64(factor.max(0.0))
    }
}

/// The last response of a retry loop, ready for the caller to classify.
#[derive(Debug)]
pub struct Completed {
    /// HTTP status.
    pub status: StatusCode,
    /// Response body as text.
    pub body: String,
    /// Total attempts made, including the first.
    pub attempts: u32,
    /// `Retry-After` from the last response, when present.
    pub retry_after: Option<Duration>,
}

/// The retry loop gave up on a transport-level failure.
#[derive(Debug)]
pub struct Exhausted {
    /// Total attempts made, including the first.
    pub attempts: u32,
    /// The last error.
    pub source: reqwest::Error,
}

/// Send `make()` until it yields a non-retryable response or retries run out.
///
/// Successful and non-retryable statuses return `Ok(Completed)` so the caller
/// maps them; transport errors after the last retry return `Err(Exhausted)`.
/// `service` labels every failed attempt reported to the global
/// [`crate::Observer`] (an application passes its own upstream names). Every
/// failed attempt is reported, retried or not: a retry that succeeds hides
/// the failure from the caller, but the attempt was still load on the
/// upstream and still a symptom. The loop is the one place every client
/// passes through, so it is where the count lives.
pub async fn send_with_retries(
    policy: &RetryPolicy,
    service: &'static str,
    make: impl Fn() -> reqwest::RequestBuilder,
) -> Result<Completed, Exhausted> {
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match make().send().await {
            Ok(resp) => {
                let status = resp.status();
                let retry_after = parse_retry_after(resp.headers());
                if !status.is_success() {
                    crate::observer::global().on_failed_attempt(service, status.as_str());
                }
                let body = match resp.text().await {
                    Ok(b) => b,
                    Err(source) => {
                        crate::observer::global().on_failed_attempt(service, "transport");
                        if attempt <= policy.max_retries {
                            let delay = policy.delay(attempt, None);
                            tracing::warn!(attempt, ?delay, error = %source, "body read failed; retrying");
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        return Err(Exhausted {
                            attempts: attempt,
                            source,
                        });
                    }
                };
                if RetryPolicy::is_retryable(status) && attempt <= policy.max_retries {
                    let delay = policy.delay(attempt, retry_after);
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
                });
            }
            Err(source) => {
                crate::observer::global().on_failed_attempt(service, "transport");
                if attempt <= policy.max_retries {
                    let delay = policy.delay(attempt, None);
                    tracing::warn!(attempt, ?delay, error = %source, "transport error; retrying");
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(Exhausted {
                    attempts: attempt,
                    source,
                });
            }
        }
    }
}

/// Parse `Retry-After` in delay-seconds form. The HTTP-date form is ignored
/// and the backoff applies instead, since reading it needs a clock the
/// client cannot trust to match the server's.
pub fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|s| *s >= 0.0)
        .map(Duration::from_secs_f64)
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
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn retry_after_wins_when_within_cap() {
        let p = RetryPolicy::default();
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        let d = p.delay(1, Some(Duration::from_secs(600)));
        assert!(d <= Duration::from_millis(625), "{d:?}");
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let p = RetryPolicy {
            backoff_jitter: 0.0,
            ..RetryPolicy::default()
        };
        assert_eq!(p.delay(1, None), Duration::from_millis(500));
        assert_eq!(p.delay(2, None), Duration::from_millis(1000));
        assert_eq!(p.delay(3, None), Duration::from_millis(2000));
        assert_eq!(p.delay(10, None), Duration::from_secs(5));
    }

    #[test]
    fn retry_after_header_parses_seconds_only() {
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("2.5"));
        assert_eq!(parse_retry_after(&h), Some(Duration::from_millis(2500)));
        h.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(parse_retry_after(&h), None);
    }
}
