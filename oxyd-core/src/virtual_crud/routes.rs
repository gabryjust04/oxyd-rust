// oxyd-core/src/virtual_crud/routes.rs

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, patch, post},
    Json,
    Router,
};
use serde_json::Value;

use crate::{
    auth::extractors::OptionalUser,
    general::{
        errors::{ApiError, ApiResult},
        types::AppState,
    },
};

use super::delete::delete_rows;
use super::insert::insert_rows;
use super::query::parse_query_options;
use super::registry::ensure_exposed;
use super::select::select_rows;
use super::types::QueryOptions;
use super::update::update_rows;

/// Mounts the "virtual CRUD" (SELECT + INSERT + UPDATE + DELETE) routes.
///
/// Supported patterns:
///
///   GET     /api/posts
///   GET     /api/posts?select=id,title&order=created_at.desc&limit=50
///   GET     /api/posts?id=eq.42
///
///   POST    /api/posts
///     body = { ... }              → single insert
///     body = [ {...}, {...} ]     → bulk insert
///
///   PATCH   /api/posts?id=eq.42
///     body = { title: "new title", ... } → partial update with filters
///
///   DELETE  /api/posts?id=eq.42
///     → delete with mandatory filters (no "mass delete" without WHERE)
pub fn router(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/{table}",
            get(list_handler)
                .post(insert_handler)
                .patch(update_handler)
                .delete(delete_handler),
        )
        // Store shared application state in the router so every handler can access it.
        .with_state(state)
}

/// List / select handler.
///
/// Responsibilities:
/// - Load table metadata on every request.
/// - Check if the table is exposed via `_oxyd_tables` (if present).
/// - Enforce authentication if `require_auth = true`.
/// - Parse query string into [`QueryOptions`] and delegate the actual SELECT
///   to [`select_rows`].
async fn list_handler(
    State(state): State<AppState>,
    Path(table): Path<String>,
    // Raw query string is represented as a `HashMap<key, value>`.
    Query(query_params): Query<HashMap<String, String>>,
    // `OptionalUser` wraps `Option<User>` so handlers can be reused for
    // both public and authenticated endpoints.
    OptionalUser(user): OptionalUser,
) -> Result<Json<Value>, (StatusCode, String)> {
    // Check if the table is exposed and whether it requires authentication.
    let require_auth = ensure_exposed(&state, &table)
        .await
        .map_err(|e: ApiError| e.into_response())?;

    // If the table is marked as "auth-only", reject anonymous callers.
    if require_auth && user.is_none() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Authentication required".to_string(),
        ));
    }

    // For now we hard-code the schema; this can later be made dynamic
    // (e.g. multi-tenant schemas, per-user schemas, etc.).
    let schema = "public";

    // Convert the raw query params into strongly typed `QueryOptions`.
    let opts: QueryOptions = parse_query_options(&query_params)
        .map_err(|e: ApiError| e.into_response())?;

    // Execute the dynamic SELECT, taking RLS and user context into account.
    let data = select_rows(&state, schema, &table, &opts, user)
        .await
        .map_err(|e: ApiError| e.into_response())?;

    Ok(Json(data))
}

/// Insert handler (single or bulk).
///
/// - `body = { ... }`              → single insert
/// - `body = [ {...}, {...} ]`     → bulk insert
///
/// Delegates core logic to [`insert_rows`], which:
/// - Builds the dynamic `INSERT` statement for the target table.
/// - Sets the RLS context via `set_config('app.current_user_id', $1, true)`.
/// - If the table has a `user_id` column and there is an authenticated user:
///     - Always forces `user_id = user.id`.
///     - Returns `400` if the client tries to override `user_id`.
/// - Returns the inserted rows as a JSON array (PostgREST-style).
async fn insert_handler(
    State(state): State<AppState>,
    Path(table): Path<String>,
    OptionalUser(user): OptionalUser,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    // Check exposure and auth requirements for the target table.
    let require_auth = ensure_exposed(&state, &table)
        .await
        .map_err(|e: ApiError| e.into_response())?;

    if require_auth && user.is_none() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Authentication required".to_string(),
        ));
    }

    let schema = "public";

    // Delegate the heavy lifting (validation, SQL generation, RLS) to `insert_rows`.
    let data = insert_rows(&state, schema, &table, &body, user)
        .await
        .map_err(|e: ApiError| e.into_response())?;

    Ok(Json(data))
}

/// Update handler.
///
/// - `body = { col1: val1, col2: val2, ... }`
/// - Requires at least one filter in the query string (e.g. `id=eq.42`),
///   so that we never run a "mass update" without a WHERE clause.
/// - Delegates to [`update_rows`], which:
///   - Builds a dynamic `UPDATE` statement.
///   - Applies RLS by setting `app.current_user_id`.
///   - Forbids primary key updates.
///   - If the table has `user_id` and the caller is authenticated:
///       - The client cannot update `user_id` (returns `400` if it tries).
async fn update_handler(
    State(state): State<AppState>,
    Path(table): Path<String>,
    Query(query_params): Query<HashMap<String, String>>,
    OptionalUser(user): OptionalUser,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let require_auth = ensure_exposed(&state, &table)
        .await
        .map_err(|e: ApiError| e.into_response())?;

    if require_auth && user.is_none() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Authentication required".to_string(),
        ));
    }

    let schema = "public";

    // Decode query string (filters, ordering, pagination...) into `QueryOptions`.
    let opts: QueryOptions = parse_query_options(&query_params)
        .map_err(|e: ApiError| e.into_response())?;

    // Delegate to the dynamic UPDATE engine.
    let data = update_rows(&state, schema, &table, &body, &opts, user)
        .await
        .map_err(|e: ApiError| e.into_response())?;

    Ok(Json(data))
}

/// Delete handler.
///
/// - Requires at least one filter in the query string (e.g. `id=eq.42`);
///   by design we never allow a "DELETE *" without any WHERE condition.
/// - Delegates to [`delete_rows`], which:
///   - Builds the dynamic `DELETE` statement.
///   - Applies RLS via `set_config('app.current_user_id', $1, true)` inside a transaction.
///   - Returns the deleted rows as a JSON array (PostgREST-style).
async fn delete_handler(
    State(state): State<AppState>,
    Path(table): Path<String>,
    Query(query_params): Query<HashMap<String, String>>,
    OptionalUser(user): OptionalUser,
) -> Result<Json<Value>, (StatusCode, String)> {
    let require_auth = ensure_exposed(&state, &table)
        .await
        .map_err(|e: ApiError| e.into_response())?;

    if require_auth && user.is_none() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Authentication required".to_string(),
        ));
    }

    let schema = "public";

    // `QueryOptions` is also used here so that filters, RLS and deleted rows
    // selection stay consistent with the SELECT/UPDATE behaviour.
    let opts: QueryOptions = parse_query_options(&query_params)
        .map_err(|e: ApiError| e.into_response())?;

    let data = delete_rows(&state, schema, &table, &opts, user)
        .await
        .map_err(|e: ApiError| e.into_response())?;

    Ok(Json(data))
}
