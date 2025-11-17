// tests/utils.rs
//! Shared helpers for integration tests (auth + virtual_crud).

use oxyd_core as app;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
    Router,
};
use http_body_util::BodyExt as _;
use serde::{de::DeserializeOwned, Deserialize};
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

use app::virtual_crud::registry::RegistryCache;

#[derive(Debug, Deserialize)]
pub struct PublicUser {
    pub id: i64,
    pub email: String,
}

#[derive(Debug, Deserialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub user: PublicUser,
}

/// Build a full app that exposes both auth routes and virtual_crud routes
/// sharing the same AppState (DB pool + JWT secret + TTL + registry cache).
pub fn build_full_app(pool: PgPool) -> Router {
    // Secret hardcoded per i test, va benissimo.
    let secret: Arc<[u8]> = Arc::from(b"segreto".to_vec().into_boxed_slice());

    // TTL di default: 15 min access, 30 giorni refresh
    let state = app::general::types::AppState::new(
        pool,
        secret,
        Duration::from_secs(15 * 60),
        Duration::from_secs(30 * 24 * 3600),
        RegistryCache::new(),
    );

    let auth_router = app::auth::routes::router(state.clone());
    let crud_router = app::virtual_crud::routes::router(state);

    // Axum 0.7: merge unisce le route dei due Router
    Router::new().merge(auth_router).merge(crud_router)
}

/// POST JSON, opzionale header Bearer.
pub async fn post_json(
    app: &Router,
    uri: &str,
    json: serde_json::Value,
    bearer: Option<&str>,
) -> Response {
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");

    if let Some(tk) = bearer {
        req = req.header("authorization", format!("Bearer {tk}"));
    }

    app.clone()
        .oneshot(
            req.body(Body::from(
                serde_json::to_vec(&json).expect("serialize json body"),
            ))
            .unwrap(),
        )
        .await
        .unwrap()
}

/// PATCH JSON, opzionale header Bearer.
pub async fn patch_json(
    app: &Router,
    uri: &str,
    json: serde_json::Value,
    bearer: Option<&str>,
) -> Response {
    let mut req = Request::builder()
        .method("PATCH")
        .uri(uri)
        .header("content-type", "application/json");

    if let Some(tk) = bearer {
        req = req.header("authorization", format!("Bearer {tk}"));
    }

    app.clone()
        .oneshot(
            req.body(Body::from(
                serde_json::to_vec(&json).expect("serialize json body"),
            ))
            .unwrap(),
        )
        .await
        .unwrap()
}

/// GET, opzionale header Bearer.
pub async fn get_json(app: &Router, uri: &str, bearer: Option<&str>) -> Response {
    let mut req = Request::builder().method("GET").uri(uri);
    if let Some(tk) = bearer {
        req = req.header("authorization", format!("Bearer {tk}"));
    }

    app.clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// DELETE, opzionale header Bearer.
pub async fn delete_json(app: &Router, uri: &str, bearer: Option<&str>) -> Response {
    let mut req = Request::builder().method("DELETE").uri(uri);
    if let Some(tk) = bearer {
        req = req.header("authorization", format!("Bearer {tk}"));
    }

    app.clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// Deserialize JSON response into `T`, keeping the HTTP status.
pub async fn read_json<T: DeserializeOwned>(res: Response) -> (StatusCode, T) {
    let status = res.status();
    let bytes = res
        .into_body()
        .collect()
        .await
        .expect("body collect")
        .to_bytes();
    let val = serde_json::from_slice::<T>(&bytes).unwrap_or_else(|e| {
        panic!(
            "failed to parse JSON: {e}\nBody: {}",
            String::from_utf8_lossy(&bytes)
        )
    });
    (status, val)
}
