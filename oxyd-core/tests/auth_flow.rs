// tests/auth_flow.rs
//! Integration tests for the Axum/SQLx auth module.
//!
//! This suite verifies the full user-side flow:
//! - Register (issue access + refresh)
//! - Login (issue access + refresh)
//! - Protected /auth/me (middleware injects CurrentUser)
//! - Refresh with rotation (old refresh becomes invalid)
//! - Duplicate register, wrong credentials, missing/invalid bearer
//! - Expired access token behavior
//!
//! Requirements:
//! - A running Postgres reachable via DATABASE_URL
//! - Migrations in ./migrations (relative to the crate root)
//!
//! 

use oxyd_core as app;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
    Router,
};
use http_body_util::BodyExt as _;
use serde::Deserialize;
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;



#[derive(Debug, Deserialize)]
struct PublicUser {
    id: i64,
    email: String,
}

#[derive(Debug, Deserialize)]
struct AuthResponse {
    access_token: String,
    refresh_token: String,
    user: PublicUser,
}

/* -------------------------- Test helpers -------------------------- */

fn build_app_with_ttls(pool: PgPool, access_ttl: Duration, refresh_ttl: Duration) -> Router {
    let secret: Arc<[u8]> = Arc::from(b"segreto".to_vec().into_boxed_slice());
    let state = app::general::types::AppState::new(pool, secret, access_ttl, refresh_ttl);
    app::auth::routes::router(state)
}

fn build_app(pool: PgPool) -> Router {
    // Defaults good enough for most tests (15 min access, 30d refresh)
    build_app_with_ttls(
        pool,
        Duration::from_secs(15 * 60),
        Duration::from_secs(30 * 24 * 3600),
    )
}

/// Send a JSON POST to the given `uri` and return the `Response`.
async fn post_json(app: &Router, uri: &str, json: serde_json::Value) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&json).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Send a GET with an optional Bearer token.
async fn get_bearer(app: &Router, uri: &str, access_token: Option<&str>) -> Response {
    let mut req = Request::builder().method("GET").uri(uri);
    if let Some(tk) = access_token {
        req = req.header("authorization", format!("Bearer {tk}"));
    }
    app.clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// Deserialize a JSON response body into `T`, preserving the HTTP status.
async fn read_json<T: serde::de::DeserializeOwned>(res: Response) -> (StatusCode, T) {
    let status = res.status();
    let bytes = res
        .into_body()
        .collect()
        .await
        .expect("body collect")
        .to_bytes();
    let val = serde_json::from_slice::<T>(&bytes)
        .unwrap_or_else(|e| panic!("failed to parse JSON: {e}\nBody: {}", String::from_utf8_lossy(&bytes)));
    (status, val)
}

/* -------------------------- Test cases --------------------------- */

/// Full happy-path: register → /me → refresh (rotation) → /me with new access.
#[sqlx::test(migrations = "../oxyd-core/migrations")]
async fn auth_flow_happy_path(pool: PgPool) {
    let app = build_app(pool);

    // 1) Register a new user
    let res = post_json(
        &app,
        "/auth/register",
        serde_json::json!({ "email": "u1@example.com", "password": "verystrong" }),
    )
    .await;
    assert!(res.status().is_success(), "register should succeed");
    let (_, reg): (StatusCode, AuthResponse) = read_json(res).await;

    // 2) /auth/me with the access token
    let res = get_bearer(&app, "/auth/me", Some(&reg.access_token)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let (_, me): (StatusCode, PublicUser) = read_json(res).await;
    assert_eq!(me.email, "u1@example.com");
    assert_eq!(me.id, reg.user.id);

    // 3) Refresh: should return a new access and a NEW refresh token
    let res = post_json(
        &app,
        "/auth/refresh",
        serde_json::json!({ "refresh_token": reg.refresh_token }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let (_, refreshed): (StatusCode, AuthResponse) = read_json(res).await;
    assert_ne!(refreshed.refresh_token, reg.refresh_token, "refresh token must be rotated");

    // 4) /auth/me with the new access token still works
    let res = get_bearer(&app, "/auth/me", Some(&refreshed.access_token)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let (_, me2): (StatusCode, PublicUser) = read_json(res).await;
    assert_eq!(me2.email, "u1@example.com");
    assert_eq!(me2.id, reg.user.id);
}

/// Duplicate register should yield 409 Conflict (idempotency on email).
#[sqlx::test(migrations = "../oxyd-core/migrations")]
async fn register_duplicate_email_conflict(pool: PgPool) {
    let app = build_app(pool);

    let first = post_json(
        &app,
        "/auth/register",
        serde_json::json!({ "email": "dup@example.com", "password": "abcdefgh" }),
    )
    .await;
    assert!(first.status().is_success());

    let second = post_json(
        &app,
        "/auth/register",
        serde_json::json!({ "email": "dup@example.com", "password": "abcdefgh" }),
    )
    .await;
    assert_eq!(second.status(), StatusCode::CONFLICT, "duplicate must be 409");
}

/// Login with correct credentials yields 200 + tokens; wrong password yields 401.
#[sqlx::test(migrations = "../oxyd-core/migrations")]
async fn login_success_and_wrong_password(pool: PgPool) {
    let app = build_app(pool);

    // Prepare: register
    let _ = post_json(
        &app,
        "/auth/register",
        serde_json::json!({ "email": "login@example.com", "password": "goodpass123" }),
    )
    .await;

    // Wrong password → 401
    let wrong = post_json(
        &app,
        "/auth/login",
        serde_json::json!({ "email": "login@example.com", "password": "WRONG" }),
    )
    .await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    // Correct password → 200 + valid tokens
    let ok = post_json(
        &app,
        "/auth/login",
        serde_json::json!({ "email": "login@example.com", "password": "goodpass123" }),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
    let (_, auth): (StatusCode, AuthResponse) = read_json(ok).await;

    // /me must work with the issued access token
    let me = get_bearer(&app, "/auth/me", Some(&auth.access_token)).await;
    assert_eq!(me.status(), StatusCode::OK);
    let (_, user): (StatusCode, PublicUser) = read_json(me).await;
    assert_eq!(user.email, "login@example.com");
}

/// /auth/me must reject missing or malformed bearer tokens.
#[sqlx::test(migrations = "../oxyd-core/migrations")]
async fn me_requires_bearer(pool: PgPool) {
    let app = build_app(pool);

    // No Authorization header
    let res = get_bearer(&app, "/auth/me", None).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Garbage bearer
    let res = get_bearer(&app, "/auth/me", Some("this-is-not-a-jwt")).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// Refresh must rotate tokens and revoke the old refresh immediately.
#[sqlx::test(migrations = "../oxyd-core/migrations")]
async fn refresh_rotates_and_revokes_old(pool: PgPool) {
    let app = build_app(pool);

    let res = post_json(
        &app,
        "/auth/register",
        serde_json::json!({ "email": "rot@example.com", "password": "verystrong" }),
    )
    .await;
    let (_, reg): (StatusCode, AuthResponse) = read_json(res).await;

    // First refresh → OK
    let res = post_json(
        &app,
        "/auth/refresh",
        serde_json::json!({ "refresh_token": reg.refresh_token }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let (_, r1): (StatusCode, AuthResponse) = read_json(res).await;

    // Using the OLD refresh again → must be 401 (revoked)
    let res = post_json(
        &app,
        "/auth/refresh",
        serde_json::json!({ "refresh_token": reg.refresh_token }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Using the NEW refresh works
    let res = post_json(
        &app,
        "/auth/refresh",
        serde_json::json!({ "refresh_token": r1.refresh_token }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
}

/// Refresh with an arbitrary random string must be rejected.
#[sqlx::test(migrations = "../oxyd-core/migrations")]
async fn refresh_rejects_random_token(pool: PgPool) {
    let app = build_app(pool);

    // No user/refresh exists; any random string must fail
    let res = post_json(
        &app,
        "/auth/refresh",
        serde_json::json!({ "refresh_token": "totally-random-invalid" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// An access token with (very) short TTL should expire and be rejected by /auth/me.
#[sqlx::test(migrations = "../oxyd-core/migrations")]
async fn expired_access_token_is_rejected(pool: PgPool) {
    // Build app with 1-second access TTL to make expiration deterministic.
    let app = build_app_with_ttls(pool, Duration::from_secs(1), Duration::from_secs(3600));

    // Register to get a short-lived access token
    let res = post_json(
        &app,
        "/auth/register",
        serde_json::json!({ "email": "exp@example.com", "password": "verystrong" }),
    )
    .await;
    let (_, reg): (StatusCode, AuthResponse) = read_json(res).await;


    println!("Access token: {}", reg.access_token);
    // Wait for expiration (sleep > TTL)
    tokio::time::sleep(Duration::from_secs(2)).await;

    // /auth/me must now reject the expired token
    let res = get_bearer(&app, "/auth/me", Some(&reg.access_token)).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
