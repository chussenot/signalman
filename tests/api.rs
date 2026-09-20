//! End-to-end tests through the axum router, no network.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use rustsafe::{Store, router};
use serde_json::{Value, json};
use tower::ServiceExt;

fn app() -> Router {
    router(Store::new())
}

async fn send(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(json) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(req.body(body).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, json)
}

async fn create_user(app: &Router, username: &str, email: &str) -> Value {
    let (status, body) = send(
        app,
        "POST",
        "/users",
        Some(json!({ "username": username, "email": email })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

#[tokio::test]
async fn creates_and_fetches_a_user_with_normalised_email() {
    let app = app();
    let created = create_user(&app, "ada", "  Ada@Example.ORG ").await;
    assert_eq!(created["email"], "ada@example.org");

    let id = created["id"].as_str().expect("id");
    let (status, fetched) = send(&app, "GET", &format!("/users/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched, created);
}

#[tokio::test]
async fn invalid_domain_value_is_422_problem_details() {
    let app = app();
    let (status, body) = send(
        &app,
        "POST",
        "/users",
        Some(json!({ "username": "ada", "email": "not-an-email" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["status"], 422);
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .contains("invalid email address"),
        "{body}"
    );
}

#[tokio::test]
async fn malformed_json_is_400() {
    let app = app();
    let req = Request::builder()
        .method("POST")
        .uri("/users")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{ not json"))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
}

#[tokio::test]
async fn duplicate_username_is_409() {
    let app = app();
    create_user(&app, "ada", "ada@example.org").await;
    let (status, body) = send(
        &app,
        "POST",
        "/users",
        Some(json!({ "username": "ada", "email": "other@example.org" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn unknown_user_is_404_and_bad_uuid_is_400() {
    let app = app();
    let (status, _) = send(
        &app,
        "GET",
        "/users/00000000-0000-0000-0000-000000000000",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(&app, "GET", "/users/not-a-uuid", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn order_lifecycle_enforces_transitions() {
    let app = app();
    let user = create_user(&app, "ada", "ada@example.org").await;
    let customer_id = user["id"].clone();

    // Draft with zero total.
    let (status, order) = send(
        &app,
        "POST",
        "/orders",
        Some(json!({ "customer_id": customer_id, "total": 0 })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    assert_eq!(order["status"], "draft");
    assert!(order.get("submitted_at").is_none());
    let id = order["id"].as_str().unwrap().to_owned();

    // Paying a draft is a 409 naming both states.
    let (status, body) = send(&app, "POST", &format!("/orders/{id}/pay"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["detail"], "order is draft, expected submitted");

    // Submitting an empty order is a 422.
    let (status, _) = send(&app, "POST", &format!("/orders/{id}/submit"), None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Add an amount, then submit.
    let (status, body) = send(
        &app,
        "POST",
        &format!("/orders/{id}/items"),
        Some(json!({ "amount": 1999 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 1999);

    let (status, body) = send(&app, "POST", &format!("/orders/{id}/submit"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "submitted");
    assert!(body["submitted_at"].is_string());

    // Submitted orders are frozen.
    let (status, _) = send(
        &app,
        "POST",
        &format!("/orders/{id}/items"),
        Some(json!({ "amount": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Pay, then paying again is a conflict.
    let (status, body) = send(&app, "POST", &format!("/orders/{id}/pay"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "paid");
    assert!(body["paid_at"].is_string());

    let (status, _) = send(&app, "POST", &format!("/orders/{id}/pay"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn negative_amount_never_reaches_the_domain() {
    let app = app();
    let user = create_user(&app, "ada", "ada@example.org").await;
    let (status, body) = send(
        &app,
        "POST",
        "/orders",
        Some(json!({ "customer_id": user["id"], "total": -1 })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

#[tokio::test]
async fn order_for_unknown_customer_is_404() {
    let app = app();
    let (status, _) = send(
        &app,
        "POST",
        "/orders",
        Some(json!({ "customer_id": "00000000-0000-0000-0000-000000000000", "total": 10 })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn openapi_document_covers_every_route() {
    let app = app();
    let (status, doc) = send(&app, "GET", "/openapi.json", None).await;
    assert_eq!(status, StatusCode::OK);
    let paths = doc["paths"].as_object().expect("paths");
    for expected in [
        "/users",
        "/users/{id}",
        "/orders",
        "/orders/{id}",
        "/orders/{id}/items",
        "/orders/{id}/submit",
        "/orders/{id}/pay",
    ] {
        assert!(paths.contains_key(expected), "missing {expected}");
    }
    let schemas = &doc["components"]["schemas"];
    assert_eq!(schemas["Money"]["minimum"], 0);
    assert_eq!(schemas["Username"]["minLength"], 3);
    assert_eq!(schemas["Email"]["format"], "email");
}

#[tokio::test]
async fn healthz_is_ok() {
    let (status, body) = send(&app(), "GET", "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");
}
