use std::collections::HashMap;

use axum::{
    Extension, Json, Router, extract::{Path, Query, State}, routing::get
};
use serde_json::Value;
use axum::http::StatusCode;
use sha2::digest::crypto_common::IvSizeUser;

use crate::{auth::types::CurrentUser, general::errors::{ApiError, ApiResult}};
use crate::general::types::AppState; // assume che contenga `pub db: PgPool`

use super::query::{parse_query_options};
use super::select::select_rows;
use super::registry::{ensure_exposed};
use super::types::QueryOptions;
use crate::auth::extractors::OptionalUser;

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
    OptionalUser(user): OptionalUser,
) -> Result<Json<Value>, (StatusCode, String)> {
    let require_auth = ensure_exposed(&state, &table)
        .await
        .map_err(|e| e.into_response())?;

    // Ora puoi verificare se l'auth è richiesta e se l'utente è presente
    if require_auth && user.is_none() {
        return Err((StatusCode::UNAUTHORIZED, "Authentication required".to_string()));
    }

    let schema = "public";
    let opts: QueryOptions = parse_query_options(&qs)
        .map_err(|e| e.into_response())?;

    let data = select_rows(&state, schema, &table, &opts,user)
        .await
        .map_err(|e| e.into_response())?;

    Ok(Json(data))
}


