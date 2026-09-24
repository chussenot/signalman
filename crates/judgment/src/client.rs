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
//! non-success status is [`Error::Http`]; and a transport failure after the
//! retries is [`Error::Transport`]. Each carries the attempt count where one
//! applies. The key is marked sensitive and redacted from `Debug` output, so a
//! client or builder printed with `{:?}` cannot leak it.
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

use reqwest::Url;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
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
    #[tracing::instrument(
        name = "typesafe.evaluate",
        skip_all,
        fields(model = request.model, input_tokens = tracing::field::Empty)
    )]
    pub async fn evaluate<S: Serialize + Sync>(
        &self,
        request: &Request<'_, S>,
    ) -> Result<Response> {
        let url = self.url("v1/systemone")?;
        let body = serde_json::to_vec(request)?;
        let text = self
            .send_with_retries(|| self.http.post(url.clone()).body(body.clone()))
            .await?;
        let response: Response = serde_json::from_str(&text)?;
        tracing::Span::current().record("input_tokens", response.usage.input_tokens);
        self.observer().on_usage(&response.model, &response.usage);
        Ok(response)
    }

    /// List the models this account may send. Observed rather than
    /// documented: the SDKs expose it, the HTTP API reference does not.
    #[tracing::instrument(name = "typesafe.list_models", skip_all)]
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let url = self.url("v1/models")?;
        let text = self
            .send_with_retries(|| self.http.get(url.clone()))
            .await?;
        let parsed: ModelsResponse = serde_json::from_str(&text)?;
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

    async fn send_with_retries(
        &self,
        make: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<String> {
        match http::send_with_retries(&self.retry, "typesafe", make).await {
            Ok(Completed { status, body, .. }) if status.is_success() => Ok(body),
            Ok(Completed {
                status,
                body,
                attempts,
                retry_after,
                ..
            }) => Err(match status.as_u16() {
                401 => Error::Unauthorized,
                422 => Error::InvalidRequest {
                    detail: http::truncate(body),
                },
                429 => Error::RateLimited {
                    attempts,
                    retry_after,
                },
                529 => Error::Overloaded { attempts },
                code => Error::Http {
                    status: code,
                    body: http::truncate(body),
                },
            }),
            Err(Exhausted { attempts, source }) => Err(Error::Transport { attempts, source }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
