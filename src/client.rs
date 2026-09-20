//! HTTP client for `POST /v1/systemone` and `GET /v1/models`.
//!
//! Defaults mirror the official SDKs: `TYPESAFE_API_KEY`, base URL
//! `https://api.typesafe.ai`, model `jev-latest`, 10 s per-request timeout,
//! two retries with exponential backoff and jitter on 408/429/5xx and on
//! transport errors, honouring `Retry-After`.

use std::fmt;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER};
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};

use crate::answer::Response;
use crate::error::{Error, Result};
use crate::question::Questions;

/// Environment variable holding the API key.
pub const API_KEY_ENV: &str = "TYPESAFE_API_KEY";
/// Environment variable overriding the base URL.
pub const BASE_URL_ENV: &str = "TYPESAFE_BASE_URL";
/// Environment variable overriding the default model.
pub const DEFAULT_MODEL_ENV: &str = "TYPESAFE_DEFAULT_MODEL";
/// Production API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";
/// Alias for the current stable release.
pub const DEFAULT_MODEL: &str = "jev-latest";
/// Default per-attempt timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Retry behaviour. Defaults match the Python SDK's `RetryPolicy`.
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
    /// Longest `Retry-After` the client will honour before falling back to
    /// its own backoff. Prevents a hostile or misconfigured header from
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

    fn is_retryable(status: StatusCode) -> bool {
        status == StatusCode::REQUEST_TIMEOUT
            || status == StatusCode::TOO_MANY_REQUESTS
            || status.is_server_error()
    }

    /// Delay before retry number `retry` (1-based), preferring `Retry-After`.
    fn delay(&self, retry: u32, retry_after: Option<Duration>) -> Duration {
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
#[derive(Debug, Clone)]
pub struct ClientBuilder {
    api_key: Option<String>,
    base_url: String,
    model: String,
    timeout: Duration,
    retry: RetryPolicy,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: DEFAULT_MODEL.to_owned(),
            timeout: DEFAULT_TIMEOUT,
            retry: RetryPolicy::default(),
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
            .user_agent(concat!("rustsafe/", env!("CARGO_PKG_VERSION")))
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

    /// Build from environment variables only.
    pub fn from_env() -> Result<Self> {
        let mut b = ClientBuilder::default();
        if let Ok(url) = std::env::var(BASE_URL_ENV) {
            b = b.base_url(url);
        }
        if let Ok(model) = std::env::var(DEFAULT_MODEL_ENV) {
            b = b.model(model);
        }
        b.build()
    }

    /// The model used by [`Self::system_one`].
    pub fn default_model(&self) -> &str {
        &self.model
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
    pub async fn evaluate<S: Serialize + Sync>(
        &self,
        request: &Request<'_, S>,
    ) -> Result<Response> {
        let url = self.url("v1/systemone")?;
        let body = serde_json::to_vec(request)?;
        let text = self
            .send_with_retries(|| self.http.post(url.clone()).body(body.clone()))
            .await?;
        Ok(serde_json::from_str(&text)?)
    }

    /// List the model names this account may send.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let url = self.url("v1/models")?;
        let text = self
            .send_with_retries(|| self.http.get(url.clone()))
            .await?;
        let parsed: ModelsResponse = serde_json::from_str(&text)?;
        Ok(parsed.models)
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.base_url
            .join(path)
            .map_err(|e| Error::Url(e.to_string()))
    }

    async fn send_with_retries(
        &self,
        make: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<String> {
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let outcome = make().send().await;
            let (status, retry_after, text) = match outcome {
                Ok(resp) => {
                    let status = resp.status();
                    let retry_after = parse_retry_after(resp.headers());
                    let text = resp.text().await.map_err(|source| Error::Transport {
                        attempts: attempt,
                        source,
                    })?;
                    (status, retry_after, text)
                }
                Err(source) => {
                    if attempt <= self.retry.max_retries {
                        let delay = self.retry.delay(attempt, None);
                        tracing::warn!(attempt, ?delay, error = %source, "transport error; retrying");
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(Error::Transport {
                        attempts: attempt,
                        source,
                    });
                }
            };

            if status.is_success() {
                return Ok(text);
            }

            if RetryPolicy::is_retryable(status) && attempt <= self.retry.max_retries {
                let delay = self.retry.delay(attempt, retry_after);
                tracing::warn!(
                    attempt,
                    status = status.as_u16(),
                    ?delay,
                    "retryable status; retrying"
                );
                tokio::time::sleep(delay).await;
                continue;
            }

            return Err(classify(status, attempt, retry_after, text));
        }
    }
}

fn classify(
    status: StatusCode,
    attempts: u32,
    retry_after: Option<Duration>,
    body: String,
) -> Error {
    match status.as_u16() {
        401 => Error::Unauthorized,
        422 => Error::InvalidRequest {
            detail: truncate(body),
        },
        429 => Error::RateLimited {
            attempts,
            retry_after,
        },
        529 => Error::Overloaded { attempts },
        code => Error::Http {
            status: code,
            body: truncate(body),
        },
    }
}

fn truncate(mut s: String) -> String {
    const MAX: usize = 2_000;
    if s.len() > MAX {
        let cut = s.floor_char_boundary(MAX);
        s.truncate(cut);
        s.push('…');
    }
    s
}

/// Parse `Retry-After` in delay-seconds form. HTTP-date form is ignored (the
/// backoff policy applies instead).
fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_wins_when_within_cap() {
        let p = RetryPolicy::default();
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        // Beyond the cap the header is ignored and backoff applies (≤ max + jitter).
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
