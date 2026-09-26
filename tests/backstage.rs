//! Backstage client and enricher against a mock backend.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_pass_by_value
)]

use std::time::Duration;

use serde_json::json;
use signalman::RetryPolicy;
use signalman::backstage::types::{EntityRef, NotificationPayload, NotificationSeverity};
use signalman::backstage::{Client, Enricher, Error};
use signalman::triage::NONE_OF_THESE;
use wiremock::matchers::{body_json, body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> Client {
    Client::builder()
        .base_url(server.uri())
        .token("bs-token")
        .retry(RetryPolicy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

fn component(
    name: &str,
    owner: &str,
    relations: serde_json::Value,
    techdocs: bool,
) -> serde_json::Value {
    let mut annotations = json!({ "backstage.io/view-url": format!("https://backstage.example/catalog/default/component/{name}") });
    if techdocs {
        annotations["backstage.io/techdocs-ref"] = json!("dir:.");
    }
    json!({
        "apiVersion": "backstage.io/v1alpha1", "kind": "Component",
        "metadata": { "name": name, "namespace": "default", "title": "Checkout API", "description": "Takes orders and payments.",
                      "tags": ["tier-1"], "annotations": annotations, "links": [{ "url": "https://grafana/x", "title": "Dashboard" }] },
        "spec": { "type": "service", "lifecycle": "production", "owner": owner, "system": "shop" },
        "relations": relations
    })
}

fn group(name: &str, display: &str, description: &str, owns: &[&str]) -> serde_json::Value {
    let mut rel: Vec<serde_json::Value> = owns
        .iter()
        .map(|c| json!({ "type": "ownerOf", "targetRef": format!("component:default/{c}") }))
        .collect();
    rel.push(json!({ "type": "hasMember", "targetRef": "user:default/ada" }));
    json!({
        "apiVersion": "backstage.io/v1alpha1", "kind": "Group",
        "metadata": { "name": name, "namespace": "default", "description": description },
        "spec": { "type": "team", "profile": { "displayName": display }, "children": [] },
        "relations": rel
    })
}

#[tokio::test]
async fn by_name_sends_bearer_and_maps_404_to_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/api/catalog/entities/by-name/component/default/checkout-api",
        ))
        .and(header("authorization", "Bearer bs-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(component(
            "checkout-api",
            "group:default/payments",
            json!([]),
            false,
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/catalog/entities/by-name/component/default/nope"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({ "error": { "name": "NotFoundError" } })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    let e = c
        .get_by_name(&EntityRef::new("component", "default", "checkout-api"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(e.owner().unwrap().to_string(), "group:default/payments");
    assert!(
        c.get_by_name(&EntityRef::new("component", "default", "nope"))
            .await
            .unwrap()
            .is_none()
    );
    let unreachable = Client::builder()
        .base_url("http://127.0.0.1:9")
        .retry(RetryPolicy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    assert!(matches!(
        unreachable
            .get_by_name(&EntityRef::new("component", "default", "x"))
            .await,
        Err(Error::Transport { .. })
    ));
}

#[tokio::test]
async fn query_encodes_filter_sets_fields_and_follows_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/catalog/entities/by-query"))
        .and(query_param("filter", "kind=group,spec.type=team"))
        .and(query_param("fields", "kind,metadata"))
        .and(query_param("orderField", "metadata.name,asc"))
        .and(query_param("limit", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [group("a", "A", "", &[]), group("b", "B", "", &[])],
            "totalItems": 3, "pageInfo": { "nextCursor": "c2" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/catalog/entities/by-query"))
        .and(query_param("cursor", "c2"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [group("c", "C", "", &[])], "totalItems": 3, "pageInfo": {}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let got = client(&server)
        .query(
            &[&[("kind", Some("group")), ("spec.type", Some("team"))]],
            Some("kind,metadata"),
            3,
        )
        .await
        .unwrap();
    assert_eq!(
        got.iter()
            .map(|e| e.metadata.name.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
    // The cursor request must not repeat filter/orderField (the API rejects it).
    let reqs = server.received_requests().await.unwrap();
    let cursor_req = reqs
        .iter()
        .find(|r| r.url.query().unwrap_or("").contains("cursor="))
        .unwrap();
    assert!(!cursor_req.url.query().unwrap().contains("filter="));
    assert!(!cursor_req.url.query().unwrap().contains("orderField="));
}

#[tokio::test]
async fn by_refs_posts_documented_body_and_drops_missing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/catalog/entities/by-refs"))
        .and(body_json(json!({ "entityRefs": ["group:default/payments", "group:default/ghost"], "fields": ["kind", "metadata"] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [group("payments", "Payments", "", &[]), null] })))
        .expect(1)
        .mount(&server)
        .await;
    let got = client(&server)
        .get_by_refs(
            &[
                EntityRef::new("group", "default", "payments"),
                EntityRef::new("group", "default", "ghost"),
            ],
            Some(&["kind", "metadata"]),
        )
        .await
        .unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].metadata.name, "payments");
    assert!(
        client(&server)
            .get_by_refs(&[], None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn techdocs_index_and_notifications_use_documented_paths() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/techdocs/static/docs/default/component/checkout-api/search/search_index.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "config": {}, "docs": [{ "location": "runbooks/x/", "title": "Runbook", "text": "do this" }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/api/techdocs/static/docs/default/component/nodocs/search/search_index.json",
        ))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/notifications/notifications"))
        .and(header("authorization", "Bearer bs-token"))
        .and(body_json(json!({
            "recipients": { "type": "entity", "entityRef": "group:default/payments" },
            "payload": { "title": "Page: X", "description": "d", "link": "https://alerts/1", "severity": "high", "topic": "signalman" }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    let idx = c
        .techdocs_search_index(&EntityRef::new("component", "default", "checkout-api"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(idx.docs.len(), 1);
    assert!(
        c.techdocs_search_index(&EntityRef::new("component", "default", "nodocs"))
            .await
            .unwrap()
            .is_none()
    );
    c.notify_entity(
        &EntityRef::new("group", "default", "payments"),
        &NotificationPayload {
            title: "Page: X".into(),
            description: Some("d".into()),
            link: Some("https://alerts/1".into()),
            severity: Some(NotificationSeverity::High),
            topic: Some("signalman".into()),
            scope: None,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn unauthorized_is_reported_as_such() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({ "error": { "name": "AuthenticationError" } })),
        )
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server)
            .get_by_name(&EntityRef::new("component", "default", "x"))
            .await,
        Err(Error::Unauthorized { status: 401 })
    ));
}

/// The enricher end to end against a small catalog.
async fn catalog(server: &MockServer) {
    let relations = json!([
        { "type": "ownedBy", "targetRef": "group:default/payments" },
        { "type": "dependsOn", "targetRef": "resource:default/orders-db" },
        { "type": "dependencyOf", "targetRef": "component:default/web" }
    ]);
    Mock::given(method("GET"))
        .and(path(
            "/api/catalog/entities/by-name/component/default/checkout-api",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(component(
            "checkout-api",
            "group:default/payments",
            relations,
            true,
        )))
        .mount(server)
        .await;
    // Neighbours.
    Mock::given(method("POST"))
        .and(path("/api/catalog/entities/by-refs"))
        .and(body_string_contains("resource:default/orders-db"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [
            { "kind": "Resource", "metadata": { "name": "orders-db", "namespace": "default" }, "spec": {},
              "relations": [{ "type": "ownedBy", "targetRef": "group:default/data-platform" }] },
            { "kind": "Component", "metadata": { "name": "web", "namespace": "default" }, "spec": {},
              "relations": [{ "type": "ownedBy", "targetRef": "group:default/payments" }] }
        ] })))
        .with_priority(1)
        .mount(server)
        .await;
    // Groups.
    Mock::given(method("POST"))
        .and(path("/api/catalog/entities/by-refs"))
        .and(body_string_contains("group:default/payments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [
            group("payments", "Payments", "Checkout, payments and refunds", &["checkout-api", "web"]),
            group("data-platform", "Data Platform", "Databases and pipelines", &[])
        ] })))
        .with_priority(1)
        .mount(server)
        .await;
    // Any other by-refs (components mentioned in text): nothing found.
    Mock::given(method("POST"))
        .and(path("/api/catalog/entities/by-refs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [] })))
        .with_priority(9)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/techdocs/static/docs/default/component/checkout-api/search/search_index.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "docs": [
            { "location": "", "title": "Checkout API", "text": "Overview." },
            { "location": "runbooks/high-error-rate/", "title": "High error rate", "text": "Check payments-gateway, then roll back the last deploy." }
        ] })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn enricher_builds_context_candidates_and_runbook() {
    let server = MockServer::start().await;
    catalog(&server).await;
    let enricher = Enricher::new(client(&server));
    let e = enricher
        .enrich(
            &["checkout-api".into()],
            "HighErrorRate checkout-api 5xx ratio 12%",
        )
        .await
        .unwrap();

    let c = e.component.unwrap();
    assert_eq!(c.name, "checkout-api");
    assert_eq!(c.owner.as_deref(), Some("Payments"));
    assert_eq!(
        c.owner_description.as_deref(),
        Some("Checkout, payments and refunds")
    );
    assert_eq!(c.lifecycle.as_deref(), Some("production"));
    assert_eq!(c.system.as_deref(), Some("shop"));
    assert_eq!(c.depends_on, vec!["resource orders-db"]);
    assert_eq!(c.dependents, vec!["component web"]);
    assert_eq!(c.links, vec!["Dashboard"]);
    assert_eq!(e.matched_by.as_deref(), Some("name checkout-api"));

    let keys: Vec<&str> = e.candidates.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, vec!["data-platform", "payments", NONE_OF_THESE]);
    let payments = e.candidates.get("payments").unwrap();
    assert!(
        payments.description.contains("Owns: checkout-api, web"),
        "{}",
        payments.description
    );
    assert_eq!(
        payments.entity_ref.as_deref(),
        Some("group:default/payments")
    );

    assert!(
        e.runbook
            .unwrap()
            .starts_with("High error rate (runbooks/high-error-rate): Check payments-gateway")
    );
}

#[tokio::test]
async fn unknown_component_falls_back_to_all_teams() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/api/catalog/entities/by-name/component/default/mystery",
        ))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/catalog/entities/by-query"))
        .and(query_param(
            "filter",
            "kind=component,metadata.title=mystery",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "items": [], "totalItems": 0, "pageInfo": {} })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/catalog/entities/by-query"))
        .and(query_param("filter", "kind=group,spec.type=team"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [group("sre", "SRE", "Reliability", &[]), group("payments", "Payments", "", &[])], "totalItems": 2, "pageInfo": {}
        })))
        .mount(&server)
        .await;
    let e = Enricher::new(client(&server))
        .enrich(&["mystery".into()], "x")
        .await
        .unwrap();
    assert!(e.component.is_none());
    assert!(e.runbook.is_none());
    let keys: Vec<&str> = e.candidates.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, vec!["payments", "sre", NONE_OF_THESE]);
}

#[tokio::test]
async fn group_types_become_one_filter_set_each() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/api/catalog/entities/by-name/component/default/mystery",
        ))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/catalog/entities/by-query"))
        .and(query_param(
            "filter",
            "kind=component,metadata.title=mystery",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "items": [], "totalItems": 0, "pageInfo": {} })),
        )
        .mount(&server)
        .await;
    // Two `filter` parameters, ORed by the catalog.
    Mock::given(method("GET"))
        .and(path("/api/catalog/entities/by-query"))
        .and(query_param("filter", "kind=group,spec.type=squad"))
        .and(query_param("filter", "kind=group,spec.type=tribe"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [group("fcp", "FCP", "", &[])], "totalItems": 1, "pageInfo": {}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let e = Enricher::new(client(&server))
        .with_group_types(vec!["squad".into(), "tribe".into()])
        .enrich(&["mystery".into()], "x")
        .await
        .unwrap();
    let keys: Vec<&str> = e.candidates.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, vec!["fcp", NONE_OF_THESE]);
}

#[tokio::test]
async fn techdocs_forbidden_leaves_runbook_empty() {
    let server = MockServer::start().await;
    catalog(&server).await;
    // A token restricted to the catalog plugin: TechDocs answers 403.
    Mock::given(method("GET"))
        .and(path("/api/techdocs/static/docs/default/component/checkout-api/search/search_index.json"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": { "name": "NotAllowedError", "message": "This token's access is restricted to plugin(s) 'catalog'" }
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    let e = Enricher::new(client(&server))
        .enrich(&["checkout-api".into()], "HighErrorRate checkout-api")
        .await
        .unwrap();
    assert_eq!(e.component.unwrap().name, "checkout-api");
    assert!(e.runbook.is_none());
    assert!(e.runbook_url.is_none());
}

#[tokio::test]
async fn a_body_over_the_cap_is_refused_and_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/catalog/entities/by-name/component/default/big"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'{'; 33]))
        .expect(1)
        .mount(&server)
        .await;
    let c = Client::builder()
        .base_url(server.uri())
        .token("bs-token")
        .retry(RetryPolicy {
            max_body_bytes: 32,
            ..RetryPolicy::default()
        })
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let err = c
        .get_by_name(&EntityRef::new("component", "default", "big"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::ResponseTooLarge { limit: 32 }),
        "{err:?}"
    );
    server.verify().await;
}
