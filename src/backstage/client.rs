//! HTTP client for the Backstage backend plugins signalman uses.

use std::fmt;
use std::time::Duration;

use reqwest::Url;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use serde::de::DeserializeOwned;

use super::error::{Error, Result};
use super::types::{
    ByRefsResponse, Entity, EntityRef, NotificationPayload, QueryResponse, SearchIndex,
};
use crate::http::{self, Completed, Exhausted, RetryPolicy};

/// Environment variable holding the Backstage backend base URL (e.g.
/// `https://backstage.example.com`, without `/api`).
pub const BASE_URL_ENV: &str = "BACKSTAGE_BASE_URL";
/// Environment variable holding the static external-access token.
pub const TOKEN_ENV: &str = "BACKSTAGE_TOKEN";
/// Default per-attempt timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest page the catalog is asked for.
pub const PAGE_SIZE: usize = 100;

/// One filter set: conditions ANDed together. `None` asserts key existence.
pub type FilterSet<'a> = &'a [(&'a str, Option<&'a str>)];

/// Builder for [`Client`].
#[derive(Debug, Clone, Default)]
pub struct ClientBuilder {
    base_url: Option<String>,
    token: Option<String>,
    timeout: Option<Duration>,
    retry: Option<RetryPolicy>,
}

impl ClientBuilder {
    /// Backend base URL without `/api`.
    #[must_use]
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Static external-access token. Optional for unauthenticated dev setups.
    #[must_use]
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Per-attempt timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Retry policy.
    #[must_use]
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = Some(retry);
        self
    }

    /// Build; falls back to the environment for the base URL and token.
    pub fn build(self) -> Result<Client> {
        let base = self
            .base_url
            .or_else(|| std::env::var(BASE_URL_ENV).ok())
            .filter(|s| !s.trim().is_empty())
            .ok_or(Error::MissingConfig)?;
        let base_url =
            Url::parse(base.trim_end_matches('/')).map_err(|e| Error::Url(e.to_string()))?;
        let token = self
            .token
            .or_else(|| std::env::var(TOKEN_ENV).ok())
            .filter(|s| !s.trim().is_empty());
        let auth = match token {
            Some(t) => {
                let mut v = HeaderValue::from_str(&format!("Bearer {t}")).map_err(|_| {
                    Error::Url("token contains characters invalid in a header".into())
                })?;
                v.set_sensitive(true);
                Some(v)
            }
            None => None,
        };
        let http = reqwest::Client::builder()
            .timeout(self.timeout.unwrap_or(DEFAULT_TIMEOUT))
            .user_agent(concat!("signalman/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Error::Transport {
                attempts: 0,
                source: e,
            })?;
        Ok(Client {
            http,
            base_url,
            auth,
            retry: self.retry.unwrap_or_default(),
        })
    }
}

/// A configured Backstage client. Cheap to clone.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: Url,
    auth: Option<HeaderValue>,
    retry: RetryPolicy,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("backstage::Client")
            .field("base_url", &self.base_url.as_str())
            .field("token", &self.auth.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Start building a client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Build from `BACKSTAGE_BASE_URL` and `BACKSTAGE_TOKEN`.
    pub fn from_env() -> Result<Self> {
        ClientBuilder::default().build()
    }

    /// True when `BACKSTAGE_BASE_URL` is set.
    pub fn is_configured() -> bool {
        std::env::var(BASE_URL_ENV).is_ok_and(|v| !v.trim().is_empty())
    }

    /// `GET /catalog/entities/by-name/{kind}/{namespace}/{name}`; `None` on 404.
    pub async fn get_by_name(&self, entity_ref: &EntityRef) -> Result<Option<Entity>> {
        let path = format!(
            "api/catalog/entities/by-name/{}/{}/{}",
            entity_ref.kind, entity_ref.namespace, entity_ref.name
        );
        let url = self.url(&path);
        match self.call::<Entity>(|| self.http.get(url.clone())).await {
            Ok(e) => Ok(Some(e)),
            Err(Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `GET /catalog/entities/by-query`. Each filter set is ANDed internally;
    /// sets are ORed. Follows `pageInfo.nextCursor` until `max` entities.
    pub async fn query(
        &self,
        filter_sets: &[FilterSet<'_>],
        fields: Option<&str>,
        max: usize,
    ) -> Result<Vec<Entity>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut url = self.url("api/catalog/entities/by-query");
            {
                let mut q = url.query_pairs_mut();
                // The API rejects filter/orderField alongside cursor; the
                // cursor encodes them.
                if let Some(c) = &cursor {
                    q.append_pair("cursor", c);
                } else {
                    for set in filter_sets {
                        let cond = set
                            .iter()
                            .map(|(k, v)| match v {
                                Some(v) => format!("{k}={v}"),
                                None => (*k).to_owned(),
                            })
                            .collect::<Vec<_>>()
                            .join(",");
                        q.append_pair("filter", &cond);
                    }
                    if let Some(f) = fields {
                        q.append_pair("fields", f);
                    }
                    q.append_pair("orderField", "metadata.name,asc");
                }
                q.append_pair("limit", &(max - out.len()).clamp(1, PAGE_SIZE).to_string());
            }
            let page: QueryResponse = self.call(|| self.http.get(url.clone())).await?;
            let got = page.items.len();
            out.extend(page.items);
            cursor = page.page_info.and_then(|p| p.next_cursor);
            if got == 0 || cursor.is_none() || out.len() >= max {
                break;
            }
        }
        out.truncate(max);
        Ok(out)
    }

    /// `POST /catalog/entities/by-refs`. Missing entities are dropped.
    pub async fn get_by_refs(
        &self,
        refs: &[EntityRef],
        fields: Option<&[&str]>,
    ) -> Result<Vec<Entity>> {
        if refs.is_empty() {
            return Ok(Vec::new());
        }
        let url = self.url("api/catalog/entities/by-refs");
        let mut body = serde_json::json!({
            "entityRefs": refs.iter().map(ToString::to_string).collect::<Vec<_>>(),
        });
        if let Some(f) = fields {
            body["fields"] = serde_json::json!(f);
        }
        let bytes = serde_json::to_vec(&body)?;
        let resp: ByRefsResponse = self
            .call(|| {
                self.http
                    .post(url.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .body(bytes.clone())
            })
            .await?;
        Ok(resp.items.into_iter().flatten().collect())
    }

    /// The mkdocs search index TechDocs publishes for an entity; `None` when
    /// the site or the index does not exist.
    pub async fn techdocs_search_index(
        &self,
        entity_ref: &EntityRef,
    ) -> Result<Option<SearchIndex>> {
        let path = format!(
            "api/techdocs/static/docs/{}/{}/{}/search/search_index.json",
            entity_ref.namespace, entity_ref.kind, entity_ref.name
        );
        let url = self.url(&path);
        match self
            .call::<SearchIndex>(|| self.http.get(url.clone()))
            .await
        {
            Ok(i) => Ok(Some(i)),
            Err(Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `POST /notifications/notifications` to an entity (group or user).
    pub async fn notify_entity(
        &self,
        recipient: &EntityRef,
        payload: &NotificationPayload,
    ) -> Result<()> {
        let url = self.url("api/notifications/notifications");
        let bytes = serde_json::to_vec(&serde_json::json!({
            "recipients": { "type": "entity", "entityRef": recipient.to_string() },
            "payload": payload,
        }))?;
        self.call_raw(|| {
            self.http
                .post(url.clone())
                .header(CONTENT_TYPE, "application/json")
                .body(bytes.clone())
        })
        .await
        .map(|_| ())
    }

    fn url(&self, path: &str) -> Url {
        // Keep any path prefix on the base URL (e.g. https://host/backstage).
        let mut base = self.base_url.clone();
        let joined = format!("{}/{}", base.path().trim_end_matches('/'), path);
        base.set_path(&joined);
        base.set_query(None);
        base
    }

    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            Some(a) => rb.header(AUTHORIZATION, a.clone()),
            None => rb,
        }
    }

    async fn call<T: DeserializeOwned>(
        &self,
        make: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<T> {
        let body = self.call_raw(make).await?;
        Ok(serde_json::from_str(&body)?)
    }

    async fn call_raw(&self, make: impl Fn() -> reqwest::RequestBuilder) -> Result<String> {
        let make = || self.authed(make());
        match http::send_with_retries(&self.retry, make).await {
            Ok(Completed { status, body, .. }) if status.is_success() => Ok(body),
            Ok(Completed {
                status,
                body,
                attempts,
                retry_after,
                ..
            }) => Err(match status.as_u16() {
                401 | 403 => Error::Unauthorized {
                    status: status.as_u16(),
                },
                404 => Error::NotFound {
                    path: make()
                        .build()
                        .map(|r| r.url().path().to_owned())
                        .unwrap_or_default(),
                },
                429 => Error::RateLimited {
                    attempts,
                    retry_after,
                },
                code => Error::Http {
                    status: code,
                    body: http::truncate(body),
                },
            }),
            Err(Exhausted { attempts, source }) => Err(Error::Transport { attempts, source }),
        }
    }
}
