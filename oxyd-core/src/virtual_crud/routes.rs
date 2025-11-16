// oxyd-core/src/virtual_crud/routes.rs

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, patch, delete},
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

use super::query::parse_query_options;
use super::registry::ensure_exposed;
use super::select::select_rows;
use super::types::QueryOptions;
use super::insert::insert_rows;
use super::update::update_rows;
use super::delete::delete_rows;

/// Monta le rotte del CRUD virtuale (SELECT + INSERT + UPDATE + DELETE).
///
/// Esempi:
///   GET     /api/posts
///   GET     /api/posts?select=id,title&order=created_at.desc&limit=50
///   GET     /api/posts?id=eq.42
///
///   POST    /api/posts
///     body = { ... }              → singolo insert
///     body = [ {...}, {...} ]     → bulk insert
///
///   PATCH   /api/posts?id=eq.42
///     body = { title: "new title", ... } → update parziale con filtri
///
///   DELETE  /api/posts?id=eq.42
///     → delete con filtri obbligatori (no mass delete)
pub fn router(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/{table}",
            get(list_handler)
                .post(insert_handler)
                .patch(update_handler)
                .delete(delete_handler),
        )
        .with_state(state)
}

/// Handler lista/selezione righe.
///
/// - legge sempre i metadati al volo
/// - verifica esposizione via `_oxyd_tables` (se presente)
/// - se `require_auth = true` e `user` è None → 401
async fn list_handler(
    State(state): State<AppState>,
    Path(table): Path<String>,
    Query(qs): Query<HashMap<String, String>>,
    OptionalUser(user): OptionalUser,
) -> Result<Json<Value>, (StatusCode, String)> {
    let require_auth = ensure_exposed(&state, &table)
        .await
        .map_err(|e| e.into_response())?;

    if require_auth && user.is_none() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Authentication required".to_string(),
        ));
    }

    let schema = "public";
    let opts: QueryOptions = parse_query_options(&qs)
        .map_err(|e| e.into_response())?;

    let data = select_rows(&state, schema, &table, &opts, user)
        .await
        .map_err(|e| e.into_response())?;

    Ok(Json(data))
}

/// Handler insert (singolo o bulk).
///
/// - body = { ... }              → singolo insert
/// - body = [ {...}, {...} ]     → bulk insert
/// - delega a `insert_rows`, che:
///   - costruisce la INSERT dinamica
///   - setta il contesto RLS via `set_config('app.current_user_id', $1, true)`
///   - se la tabella ha `user_id` e c'è un utente:
///       - forza sempre `user_id = user.id` (errore 400 se il client prova a cambiarlo)
///   - ritorna le righe inserite come array JSON
async fn insert_handler(
    State(state): State<AppState>,
    Path(table): Path<String>,
    OptionalUser(user): OptionalUser,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let require_auth = ensure_exposed(&state, &table)
        .await
        .map_err(|e| e.into_response())?;

    if require_auth && user.is_none() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Authentication required".to_string(),
        ));
    }

    let schema = "public";

    let data = insert_rows(&state, schema, &table, &body, user)
        .await
        .map_err(|e| e.into_response())?;

    Ok(Json(data))
}

/// Handler update.
///
/// - body = { col1: val1, col2: val2, ... }
/// - richiede almeno un filtro nei query param (es: `id=eq.42`)
/// - delega a `update_rows`, che:
///   - costruisce l'UPDATE dinamico
///   - applica RLS via `set_config('app.current_user_id', $1, true)`
///   - vieta update di PK
///   - se la tabella ha `user_id` e c'è un utente autenticato:
///       - il client NON può aggiornare `user_id` (400 se ci prova)
async fn update_handler(
    State(state): State<AppState>,
    Path(table): Path<String>,
    Query(qs): Query<HashMap<String, String>>,
    OptionalUser(user): OptionalUser,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let require_auth = ensure_exposed(&state, &table)
        .await
        .map_err(|e| e.into_response())?;

    if require_auth && user.is_none() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Authentication required".to_string(),
        ));
    }

    let schema = "public";
    let opts: QueryOptions = parse_query_options(&qs)
        .map_err(|e| e.into_response())?;

    let data = update_rows(&state, schema, &table, &body, &opts, user)
        .await
        .map_err(|e| e.into_response())?;

    Ok(Json(data))
}

/// Handler delete.
///
/// - richiede almeno un filtro (es: `id=eq.42`) → niente mass delete
/// - delega a `delete_rows`, che:
///   - costruisce la DELETE dinamica
///   - applica RLS via `set_config('app.current_user_id', $1, true)` dentro transazione
///   - ritorna le righe eliminate come array JSON (compatibile con PostgREST-style)
async fn delete_handler(
    State(state): State<AppState>,
    Path(table): Path<String>,
    Query(qs): Query<HashMap<String, String>>,
    OptionalUser(user): OptionalUser,
) -> Result<Json<Value>, (StatusCode, String)> {
    let require_auth = ensure_exposed(&state, &table)
        .await
        .map_err(|e| e.into_response())?;

    if require_auth && user.is_none() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Authentication required".to_string(),
        ));
    }

    let schema = "public";
    let opts: QueryOptions = parse_query_options(&qs)
        .map_err(|e| e.into_response())?;

    let data = delete_rows(&state, schema, &table, &opts, user)
        .await
        .map_err(|e| e.into_response())?;

    Ok(Json(data))
}
