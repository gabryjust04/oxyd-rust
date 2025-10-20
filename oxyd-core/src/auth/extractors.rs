use axum::{
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
};

use crate::auth::{
    errors::ApiError,
    types::{CurrentUser, JwtClaims},
};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use sqlx::Row;
use crate::general::types::AppState;


/// Estrazione OBBLIGATORIA: la rotta fallisce con 401 se non autenticato.
/// Tenta prima di leggere `CurrentUser` dalle extensions (inserito dal middleware).
/// Se assente, prova a verificare direttamente l'Authorization header.
pub struct RequireUser(pub CurrentUser);


/// Estrazione OPZIONALE: Some(user) se autenticato, altrimenti None.
pub struct OptionalUser(pub Option<CurrentUser>);

//#[async_trait]
impl FromRequestParts<AppState> for RequireUser {
    type Rejection = (StatusCode, String);

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        // 1) Se il middleware ha già inserito CurrentUser
        if let Some(cu) = parts.extensions.get::<CurrentUser>() {
            return Ok(RequireUser(cu.clone()));
        }

        // 2) Fallback: prova a verificare il token direttamente
        let token = bearer_from_headers(&parts.headers)
            .ok_or_else(|| ApiError::Unauthorized("missing Authorization header".into()).into_response())?;

        let mut v = Validation::new(Algorithm::HS256);
        v.validate_exp = true;

        let data = decode::<JwtClaims>(&token, &DecodingKey::from_secret(&state.jwt_secret), &v)
            .map_err(|_| ApiError::Unauthorized("invalid token".into()).into_response())?;

        let user_id: i64 = data
            .claims
            .sub
            .parse()
            .map_err(|_| ApiError::Unauthorized("invalid sub".into()).into_response())?;

        // carica email
        let row = sqlx::query("SELECT email FROM users WHERE id = $1 AND is_active = TRUE")
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()).into_response())?
            .ok_or_else(|| ApiError::Unauthorized("user not found".into()).into_response())?;

        let email: String = row.get("email");
        Ok(RequireUser(CurrentUser { id: user_id, email }))
    }
}

//#[async_trait]
impl FromRequestParts<AppState> for OptionalUser {
    type Rejection = (StatusCode, String);

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        // Già presente?
        if let Some(cu) = parts.extensions.get::<CurrentUser>() {
            return Ok(OptionalUser(Some(cu.clone())));
        }

        // Assente: prova header; se non c'è o invalido, torna None (NON errore)
        let Some(token) = bearer_from_headers(&parts.headers) else {
            return Ok(OptionalUser(None));
        };

        let mut v = Validation::new(Algorithm::HS256);
        v.validate_exp = true;

        let data = match decode::<JwtClaims>(&token, &DecodingKey::from_secret(&state.jwt_secret), &v) {
            Ok(d) => d,
            Err(_) => return Ok(OptionalUser(None)),
        };

        let user_id: i64 = match data.claims.sub.parse() {
            Ok(id) => id,
            Err(_) => return Ok(OptionalUser(None)),
        };

        // carica email
        let row = match sqlx::query("SELECT email FROM users WHERE id = $1 AND is_active = TRUE")
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await
        {
            Ok(Some(r)) => r,
            _ => return Ok(OptionalUser(None)),
        };

        let email: String = row.get("email");
        Ok(OptionalUser(Some(CurrentUser { id: user_id, email })))
    }
}

/* helpers */

fn bearer_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    let h = headers.get(axum::http::header::AUTHORIZATION)?;
    let s = h.to_str().ok()?;
    s.strip_prefix("Bearer ").map(|t| t.to_string())
}
