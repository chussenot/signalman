//! The MCP server over Streamable HTTP: the same [`Server`] `signalman mcp`
//! serves on stdio, mounted at [`PATH`] on an axum router behind a bearer
//! token. Mounted on the router `serve` runs, it shares that process's
//! change feed, which is what makes `recent_changes` live over HTTP.
//!
//! Stateless by construction: no `Mcp-Session-Id`, every `POST` is one
//! request and one JSON response (rmcp falls back to an SSE stream for a
//! request only if the handler emits a notification before its result,
//! which no tool here does). Two replicas behind one Service therefore need
//! no session affinity, and a graceful shutdown has no long-lived streams
//! to drain.
//!
//! The token is checked by a layer in front of rmcp's service, so a request
//! without it never reaches the protocol handler. It is its own secret,
//! `SIGNALMAN_MCP_TOKEN`: not the webhook signing secret (incident.io
//! authenticating to signalman) and not the change-feed token (delivery
//! tooling authenticating to signalman). An agent is a third party with its
//! own credential, revocable on its own.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};

use super::Server;
use crate::changes::FeedToken;

/// Where the MCP endpoint is mounted.
pub const PATH: &str = "/mcp";

/// The environment variable holding the bearer token.
pub const TOKEN_ENV: &str = "SIGNALMAN_MCP_TOKEN";

/// The bearer token an MCP client must present. Same shape and comparison
/// as the change feed's [`FeedToken`]; a different secret.
#[derive(Clone)]
pub struct McpToken(FeedToken);

impl std::fmt::Debug for McpToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("McpToken(<redacted>)")
    }
}

impl McpToken {
    /// From an explicit value; empty is `None`.
    pub fn new(token: impl Into<String>) -> Option<Self> {
        FeedToken::new(token).map(Self)
    }

    /// From [`TOKEN_ENV`]; `None` when unset or empty.
    pub fn from_env() -> Option<Self> {
        std::env::var(TOKEN_ENV).ok().and_then(Self::new)
    }

    /// Constant-time comparison against an `Authorization` header value.
    pub fn accepts(&self, authorization: Option<&str>) -> bool {
        self.0.accepts(authorization)
    }
}

/// A router serving `server` at [`PATH`], every request checked against
/// `token` first. Merge it into the router `serve` builds, or serve it on
/// its own listener.
///
/// `allowed_hosts` is rmcp's DNS-rebinding guard on the `Host` header.
/// Empty disables it: the guard exists for local servers a browser could be
/// tricked into reaching, and a browser cannot attach this bearer token, so
/// the token already closes that door. Set it (`mcp.allowed_hosts`) to pin
/// the hostnames the endpoint answers to anyway.
pub fn router(server: Server, token: McpToken, allowed_hosts: &[String]) -> Router {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true);
    let config = if allowed_hosts.is_empty() {
        config.disable_allowed_hosts()
    } else {
        config.with_allowed_hosts(allowed_hosts.iter().cloned())
    };
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    Router::new()
        .route_service(PATH, service)
        .layer(middleware::from_fn_with_state(token, require_bearer))
}

async fn require_bearer(State(token): State<McpToken>, request: Request, next: Next) -> Response {
    let presented = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if token.accepts(presented) {
        return next.run(request).await;
    }
    tracing::warn!(path = %request.uri().path(), "MCP request refused: missing or wrong bearer token");
    (
        StatusCode::UNAUTHORIZED,
        [(WWW_AUTHENTICATE, "Bearer")],
        "missing or wrong bearer token",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn token_is_bearer_only_and_never_printed() {
        let token = McpToken::new("  s3cret ").expect("non-empty");
        assert!(token.accepts(Some("Bearer s3cret")));
        assert!(!token.accepts(Some("s3cret")));
        assert!(!token.accepts(Some("Bearer other")));
        assert!(!token.accepts(None));
        assert!(McpToken::new("   ").is_none());
        assert_eq!(format!("{token:?}"), "McpToken(<redacted>)");
    }
}
