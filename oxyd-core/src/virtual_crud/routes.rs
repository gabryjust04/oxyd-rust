use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde_json::Value;
use axum::http::StatusCode;

use crate::general::errors::{ApiError, ApiResult};
use crate::general::types::AppState; // assume che contenga `pub db: PgPool`

use super::services::{ensure_exposed, parse_query_options, select_rows};
use super::types::QueryOptions;

/// Monta le rotte del CRUD virtuale (solo SELECT).
/// Esempi:
///   GET /api/posts
///   GET /api/posts?select=id,title&order=created_at.desc&limit=50
///   GET /api/posts?id=eq.42
pub fn router(state: AppState) -> Router {
    Router::new().route("/api/{table}", get(list_handler).with_state(state.clone()))
}

/// Handler lista/selezione righe.
/// - legge sempre i metadati al volo
/// - verifica esposizione via `_oxyd_tables` (se presente)
/// - (nota) se `require_auth = true`, qui NON forziamo auth: lascia la responsabilità
///         a un middleware `RequireUser` montato su questa rotta o ad un guard esterno.
///         In alternativa, puoi leggere l'utente dalle extensions e restituire 401 qui.
async fn list_handler(
    State(state): State<AppState>,
    Path(table): Path<String>,
    Query(qs): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let require_auth = ensure_exposed(&state, &table)
        .await
        .map_err(|e| e.into_response())?; // oppure mappa qui a (StatusCode, String)

    let schema = "public";
    let opts: QueryOptions = parse_query_options(&qs)
        .map_err(|e| e.into_response())?;

    let data = select_rows(&state, schema, &table, &opts)
        .await
        .map_err(|e| e.into_response())?;

    Ok(Json(data))
}
