//! The MCP server over Streamable HTTP, end to end on a real TCP listener:
//! the bearer token in front of the protocol handler, an rmcp client driving
//! tool calls over HTTP, and the change feed shared with `POST /changes`
//! when the same router serves both. No wiremock: the tools exercised here
//! (`recent_changes` and the tool list) never call TypeSafe or incident.io.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use common::{DummyClient, call};
use rmcp::ServiceExt;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::json;
use signalman::changes::{ChangeLog, FeedToken};
use signalman::incidentio::Triager;
use signalman::mcp::Server;
use signalman::mcp::http::{McpToken, PATH, router as mcp_router};
use signalman::serve::{AppState, router as serve_router};

const MCP_TOKEN: &str = "mcp-token-for-tests";
const FEED_TOKEN: &str = "feed-token-for-tests";

/// A triager whose clients point at a closed port: nothing here calls them.
fn triager() -> Triager {
    Triager::new(
        common::typesafe_client("http://127.0.0.1:9"),
        common::incidentio_client("http://127.0.0.1:9"),
    )
}

fn mcp_token() -> McpToken {
    McpToken::new(MCP_TOKEN).unwrap()
}

/// Serve `app` on an ephemeral loopback port for the rest of the test.
async fn listen(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// An rmcp client over real HTTP, presenting `token` as a bearer.
async fn connect(
    addr: SocketAddr,
    token: &str,
) -> rmcp::service::RunningService<rmcp::RoleClient, DummyClient> {
    // `StreamableHttpClientTransportConfig` is `#[non_exhaustive]`: set the
    // fields on a default rather than build it as a literal. `auth_header`
    // is the bare token; rmcp's reqwest client adds the `Bearer ` scheme.
    let mut config = StreamableHttpClientTransportConfig::default();
    config.uri = format!("http://{addr}{PATH}").into();
    config.auth_header = Some(token.to_owned());
    let transport = StreamableHttpClientTransport::from_config(config);
    DummyClient
        .serve(transport)
        .await
        .expect("client connects over HTTP")
}

fn initialize_body() -> String {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": { "name": "raw-test", "version": "0" }
        }
    })
    .to_string()
}

/// A raw `POST /mcp` with the given `Authorization` header, if any.
async fn raw_post(addr: SocketAddr, authorization: Option<&str>) -> reqwest::Response {
    let mut req = reqwest::Client::new()
        .post(format!("http://{addr}{PATH}"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(initialize_body());
    if let Some(a) = authorization {
        req = req.header("authorization", a);
    }
    req.send().await.unwrap()
}

/// The token is checked before rmcp sees the request: no header, a bare
/// token, a wrong token and the change feed's token are all refused with
/// 401 and a `WWW-Authenticate: Bearer` challenge.
#[tokio::test]
async fn a_request_without_the_token_never_reaches_the_protocol_handler() {
    let app = mcp_router(Server::new(triager(), false), mcp_token(), &[]);
    let addr = listen(app).await;

    for authorization in [
        None,
        Some(MCP_TOKEN),
        Some("Bearer not-the-token"),
        Some(&format!("Bearer {FEED_TOKEN}")),
    ] {
        let res = raw_post(addr, authorization).await;
        assert_eq!(res.status(), 401, "{authorization:?}");
        assert_eq!(
            res.headers().get("www-authenticate").unwrap(),
            "Bearer",
            "{authorization:?}"
        );
    }

    let res = raw_post(addr, Some(&format!("Bearer {MCP_TOKEN}"))).await;
    assert!(
        res.status().is_success(),
        "the right token reaches rmcp: {}",
        res.status()
    );
}

/// The full protocol over HTTP: initialize, list tools, call one. Stateless
/// on the server, so the client needs no session either.
#[tokio::test]
async fn an_authenticated_client_lists_the_tools_and_calls_one_over_http() {
    let app = mcp_router(Server::new(triager(), false), mcp_token(), &[]);
    let addr = listen(app).await;
    let client = connect(addr, MCP_TOKEN).await;

    let info = client.peer_info().expect("initialize completed");
    assert_eq!(
        info.server_info.as_ref().expect("server_info").name,
        "signalman"
    );

    let mut names: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "lookup_owner",
            "open_incidents",
            "qualify_alert",
            "recent_changes",
            "related_alerts",
        ]
    );

    let result = client
        .call_tool(call("recent_changes", json!({})))
        .await
        .unwrap();
    let body = result.structured_content.expect("structured content");
    assert_eq!(body["configured"], json!(false));
    assert_eq!(body["changes"], json!([]));
}

/// `allow_write` is honoured over HTTP exactly as over stdio.
#[tokio::test]
async fn allow_write_registers_the_write_tool_over_http_too() {
    let app = mcp_router(Server::new(triager(), true), mcp_token(), &[]);
    let addr = listen(app).await;
    let client = connect(addr, MCP_TOKEN).await;

    let tools = client.list_all_tools().await.unwrap();
    assert!(
        tools.iter().any(|t| t.name == "apply_qualification"),
        "{tools:?}"
    );
}

/// The reason HTTP exists next to stdio: mounted on the router `serve`
/// runs, `recent_changes` reads the change log `POST /changes` writes, in
/// the same process. The two endpoints keep separate tokens: the feed's
/// token does not open `/mcp`, and the MCP token does not open `/changes`.
#[tokio::test]
async fn mounted_on_the_serve_router_recent_changes_sees_what_was_posted_to_changes() {
    let mut triager = triager();
    triager.changes = Some(ChangeLog::default());
    let mut state = AppState::new(None, triager.clone());
    state.changes_token = FeedToken::new(FEED_TOKEN);
    let app = serve_router(Arc::new(state)).merge(mcp_router(
        Server::new(triager, false),
        mcp_token(),
        &[],
    ));
    let addr = listen(app).await;
    let http = reqwest::Client::new();

    let refused = http
        .post(format!("http://{addr}/changes"))
        .bearer_auth(MCP_TOKEN)
        .json(&json!({ "kind": "deploy", "summary": "nope" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        401,
        "the MCP token must not open the feed"
    );

    let posted = http
        .post(format!("http://{addr}/changes"))
        .bearer_auth(FEED_TOKEN)
        .json(&json!({
            "kind": "deploy",
            "component": "checkout-api",
            "summary": "checkout-api v42 rolled out",
            "source": "argocd"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(posted.status(), 202);

    let client = connect(addr, MCP_TOKEN).await;
    let result = client
        .call_tool(call(
            "recent_changes",
            json!({ "component": "checkout-api" }),
        ))
        .await
        .unwrap();
    let body = result.structured_content.expect("structured content");
    assert_eq!(body["configured"], json!(true));
    assert_eq!(body["changes"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(
        body["changes"][0]["summary"],
        json!("checkout-api v42 rolled out")
    );
    assert_eq!(body["changes"][0]["source"], json!("argocd"));
}

/// `mcp.allowed_hosts` pins the `Host` header: a request to the loopback
/// address is refused when only another hostname is allowed, even with the
/// right token.
#[tokio::test]
async fn allowed_hosts_refuses_a_host_that_is_not_listed() {
    let app = mcp_router(
        Server::new(triager(), false),
        mcp_token(),
        &["signalman.example.com".to_owned()],
    );
    let addr = listen(app).await;

    let res = raw_post(addr, Some(&format!("Bearer {MCP_TOKEN}"))).await;
    assert!(
        res.status().is_client_error() && res.status() != 401,
        "expected rmcp's host check to refuse 127.0.0.1, got {}",
        res.status()
    );
}
