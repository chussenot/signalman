//! HTTP client for the incident.io API subset this integration needs.
//!
//! Authentication is `Authorization: Bearer <API key>` for the management API
//! and the alert source's own token for `POST /v2/alert_events/http/...`, so
//! the key is attached per request rather than as a default header.
//! Retries follow [`crate::http::RetryPolicy`]; incident.io sends
//! `Retry-After` in seconds on 429, which the loop honours.

use std::fmt;

use reqwest::Url;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use serde::de::DeserializeOwned;

use super::error::{Error, Result};
use super::types::{
    Alert, AlertEnvelope, AlertEvent, AlertEventAck, AlertNote, AlertNoteEnvelope, AlertNotesPage,
    AlertsPage, Identity, IdentityEnvelope, Incident, IncidentAlert, IncidentAlertEnvelope,
    IncidentsPage, StatusCategory,
};
use crate::http::{self, Completed, Exhausted, RetryPolicy};

/// Environment variable holding the API key.
pub const API_KEY_ENV: &str = "INCIDENTIO_API_KEY";
/// Environment variable holding the HTTP alert source token used by
/// `triage --forward-to-incidentio`; the source's id is a setting.
pub const ALERT_SOURCE_TOKEN_ENV: &str = "INCIDENTIO_ALERT_SOURCE_TOKEN";
/// Production API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.incident.io";
/// Default per-attempt timeout.
pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// Largest page the incidents endpoint accepts.
pub const MAX_PAGE_SIZE: usize = 250;

/// Builder for [`Client`].
#[derive(Debug, Clone)]
pub struct ClientBuilder {
    api_key: Option<String>,
    base_url: String,
    timeout: std::time::Duration,
    retry: RetryPolicy,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: DEFAULT_BASE_URL.to_owned(),
            timeout: DEFAULT_TIMEOUT,
            retry: RetryPolicy::default(),
        }
    }
}

impl ClientBuilder {
    /// Set the API key explicitly (otherwise read from `INCIDENTIO_API_KEY`).
    #[must_use]
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Override the base URL (tests, proxies).
    #[must_use]
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Per-attempt timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Retry policy.
    #[must_use]
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Build the client. Reads `INCIDENTIO_API_KEY` if no key was set.
    pub fn build(self) -> Result<Client> {
        let api_key = self
            .api_key
            .or_else(|| std::env::var(API_KEY_ENV).ok())
            .filter(|k| !k.trim().is_empty())
            .ok_or(Error::MissingApiKey)?;
        let base_url = Url::parse(&self.base_url).map_err(|e| Error::Url(e.to_string()))?;
        let http = reqwest::Client::builder()
            .timeout(self.timeout)
            .user_agent(concat!("signalman/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Error::Transport {
                attempts: 0,
                source: e,
            })?;
        Ok(Client {
            http,
            base_url,
            auth: bearer(&api_key)?,
            retry: self.retry,
        })
    }
}

fn bearer(token: &str) -> Result<HeaderValue> {
    let mut v = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| Error::Url("token contains characters invalid in a header".into()))?;
    v.set_sensitive(true);
    Ok(v)
}

/// A configured incident.io API client. Cheap to clone.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: Url,
    auth: HeaderValue,
    retry: RetryPolicy,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("incidentio::Client")
            .field("base_url", &self.base_url.as_str())
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

    /// Production defaults with the API key from `INCIDENTIO_API_KEY`.
    pub fn from_env() -> Result<Self> {
        ClientBuilder::default().build()
    }

    /// `GET /v1/identity`: which key this is and what it may do.
    #[tracing::instrument(name = "incidentio.identity", skip_all)]
    pub async fn identity(&self) -> Result<Identity> {
        let url = self.url("v1/identity")?;
        let env: IdentityEnvelope = self
            .call(|| {
                self.http
                    .get(url.clone())
                    .header(AUTHORIZATION, self.auth.clone())
            })
            .await?;
        Ok(env.identity)
    }

    /// Incidents in the given status categories, following pagination until
    /// `max` are collected. Test, tutorial and retrospective incidents are
    /// filtered out client-side.
    ///
    /// Note the list endpoint's own rate limit (60 requests/minute).
    #[tracing::instrument(name = "incidentio.list_incidents", skip_all, fields(max))]
    pub async fn list_incidents_in(
        &self,
        categories: &[StatusCategory],
        max: usize,
    ) -> Result<Vec<Incident>> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let page_size = (max - out.len()).clamp(1, MAX_PAGE_SIZE);
            let mut url = self.url("v2/incidents")?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("page_size", &page_size.to_string());
                for c in categories {
                    q.append_pair("status_category[one_of]", c.as_str());
                }
                if let Some(a) = &after {
                    q.append_pair("after", a);
                }
            }
            let page: IncidentsPage = self
                .call(|| {
                    self.http
                        .get(url.clone())
                        .header(AUTHORIZATION, self.auth.clone())
                })
                .await?;
            let got = page.incidents.len();
            out.extend(
                page.incidents
                    .into_iter()
                    .filter(Incident::is_open_and_real),
            );
            after = page.pagination_meta.and_then(|m| m.after);
            if got < page_size || after.is_none() || out.len() >= max {
                break;
            }
        }
        out.truncate(max);
        Ok(out)
    }

    /// Triage, live and paused incidents: the dedup candidates.
    pub async fn list_open_incidents(&self, max: usize) -> Result<Vec<Incident>> {
        self.list_incidents_in(
            &[
                StatusCategory::Triage,
                StatusCategory::Live,
                StatusCategory::Paused,
            ],
            max,
        )
        .await
    }

    /// `GET /v2/alerts/{id}`.
    #[tracing::instrument(name = "incidentio.get_alert", skip(self))]
    pub async fn get_alert(&self, id: &str) -> Result<Alert> {
        let url = self.url(&format!("v2/alerts/{id}"))?;
        let env: AlertEnvelope = self
            .call(|| {
                self.http
                    .get(url.clone())
                    .header(AUTHORIZATION, self.auth.clone())
            })
            .await?;
        Ok(env.alert)
    }

    /// `POST /v2/alerts/{id}/actions/add_tags`. Existing tags are kept; unknown
    /// names are created.
    #[tracing::instrument(name = "incidentio.add_alert_tags", skip(self))]
    pub async fn add_alert_tags(&self, alert_id: &str, tags: &[String]) -> Result<Alert> {
        let url = self.url(&format!("v2/alerts/{alert_id}/actions/add_tags"))?;
        let body = serde_json::to_vec(&serde_json::json!({ "tags": tags }))?;
        let env: AlertEnvelope = self
            .call(|| {
                self.http
                    .post(url.clone())
                    .header(AUTHORIZATION, self.auth.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
            })
            .await?;
        Ok(env.alert)
    }

    /// `POST /v2/incident_alerts`: attach an alert to an incident. Idempotent
    /// on the server side when the connection already exists.
    #[tracing::instrument(name = "incidentio.attach_alert", skip(self))]
    pub async fn attach_alert_to_incident(
        &self,
        alert_id: &str,
        incident_id: &str,
    ) -> Result<IncidentAlert> {
        let url = self.url("v2/incident_alerts")?;
        let body = serde_json::to_vec(&serde_json::json!({
            "alert_id": alert_id,
            "incident_id": incident_id,
        }))?;
        let env: IncidentAlertEnvelope = self
            .call(|| {
                self.http
                    .post(url.clone())
                    .header(AUTHORIZATION, self.auth.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
            })
            .await?;
        Ok(env.incident_alert)
    }

    /// The HTTP alert source token from [`ALERT_SOURCE_TOKEN_ENV`]: a secret,
    /// so this module reads it, not the configuration layer.
    pub fn alert_source_token() -> Option<String> {
        std::env::var(ALERT_SOURCE_TOKEN_ENV)
            .ok()
            .filter(|t| !t.trim().is_empty())
    }

    /// `POST /v2/alert_events/http/{alert_source_config_id}`.
    ///
    /// Authenticates with the alert source's secret token, not the API key.
    /// The endpoint accepts and returns 202; processing is asynchronous.
    #[tracing::instrument(name = "incidentio.send_alert_event", skip_all)]
    pub async fn send_alert_event(
        &self,
        alert_source_config_id: &str,
        source_token: &str,
        event: &AlertEvent,
    ) -> Result<AlertEventAck> {
        let url = self.url(&format!("v2/alert_events/http/{alert_source_config_id}"))?;
        let auth = bearer(source_token)?;
        let body = serde_json::to_vec(event)?;
        self.call(|| {
            self.http
                .post(url.clone())
                .header(AUTHORIZATION, auth.clone())
                .header(CONTENT_TYPE, "application/json")
                .body(body.clone())
        })
        .await
    }

    /// Firing alerts created at or after `since` (RFC 3339), in the API's
    /// order. One page, capped at [`MAX_PAGE_SIZE`]: this is blast-radius
    /// context, where a bounded sample is enough and a second call is not.
    #[tracing::instrument(name = "incidentio.list_firing_alerts", skip(self))]
    pub async fn list_firing_alerts_since(&self, since: &str, max: usize) -> Result<Vec<Alert>> {
        let mut url = self.url("v2/alerts")?;
        url.query_pairs_mut()
            .append_pair("status[one_of]", "firing")
            .append_pair("created_at[gte]", since)
            .append_pair("page_size", &max.clamp(1, MAX_PAGE_SIZE).to_string());
        let page: AlertsPage = self
            .call(|| {
                self.http
                    .get(url.clone())
                    .header(AUTHORIZATION, self.auth.clone())
            })
            .await?;
        Ok(page.alerts)
    }

    /// `GET /v1/alert_notes?alert_id=…`: the notes on one alert.
    #[tracing::instrument(name = "incidentio.list_alert_notes", skip(self))]
    pub async fn list_alert_notes(&self, alert_id: &str) -> Result<Vec<AlertNote>> {
        let mut url = self.url("v1/alert_notes")?;
        url.query_pairs_mut()
            .append_pair("alert_id", alert_id)
            .append_pair("page_size", "50");
        let page: AlertNotesPage = self
            .call(|| {
                self.http
                    .get(url.clone())
                    .header(AUTHORIZATION, self.auth.clone())
            })
            .await?;
        Ok(page.alert_notes)
    }

    /// `POST /v1/alert_notes`: add a markdown note to an alert.
    #[tracing::instrument(name = "incidentio.create_alert_note", skip(self, content))]
    pub async fn create_alert_note(&self, alert_id: &str, content: &str) -> Result<AlertNote> {
        let url = self.url("v1/alert_notes")?;
        let body = serde_json::to_vec(&serde_json::json!({
            "alert_id": alert_id,
            "content": content,
        }))?;
        let env: AlertNoteEnvelope = self
            .call(|| {
                self.http
                    .post(url.clone())
                    .header(AUTHORIZATION, self.auth.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
            })
            .await?;
        Ok(env.alert_note)
    }

    /// `PUT /v1/alert_notes/{id}`: replace a note's content.
    #[tracing::instrument(name = "incidentio.update_alert_note", skip(self, content))]
    pub async fn update_alert_note(&self, note_id: &str, content: &str) -> Result<AlertNote> {
        let url = self.url(&format!("v1/alert_notes/{note_id}"))?;
        let body = serde_json::to_vec(&serde_json::json!({ "content": content }))?;
        let env: AlertNoteEnvelope = self
            .call(|| {
                self.http
                    .put(url.clone())
                    .header(AUTHORIZATION, self.auth.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
            })
            .await?;
        Ok(env.alert_note)
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.base_url
            .join(path)
            .map_err(|e| Error::Url(e.to_string()))
    }

    async fn call<T: DeserializeOwned>(
        &self,
        make: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<T> {
        match http::send_with_retries(&self.retry, "incidentio", make).await {
            Ok(Completed { status, body, .. }) if status.is_success() => {
                Ok(serde_json::from_str(&body)?)
            }
            Ok(Completed {
                status,
                body,
                attempts,
                retry_after,
                ..
            }) => Err(Error::from_response(
                status.as_u16(),
                body,
                attempts,
                retry_after,
            )),
            Err(Exhausted { attempts, source }) => Err(Error::Transport { attempts, source }),
        }
    }
}
