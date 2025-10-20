use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
    body::Body,
};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use crate::auth::{types::{ CurrentUser, JwtClaims}, errors::ApiError};
use sqlx::Row;
use crate::general::types::AppState;



pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Result<Response, (StatusCode, String)> {
    // Leggi Authorization: Bearer <token>
    let header = match req.headers().get(axum::http::header::AUTHORIZATION) {
        Some(h) => h,
        None => return Err(ApiError::Unauthorized("missing Authorization header".into()).into_response()),
    };
    let header = match header.to_str() {
        Ok(h) => h,
        Err(_) => return Err(ApiError::Unauthorized("bad header".into()).into_response()),
    };
    let token = match header.strip_prefix("Bearer ") {
        Some(t) => t,
        None => return Err(ApiError::Unauthorized("no bearer".into()).into_response()),
    };

    // Verifica JWT
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    let data = match decode::<JwtClaims>(
        token,
        &DecodingKey::from_secret(&state.jwt_secret),
        &validation,
    ) {
        Ok(d) => d,
        Err(_) => return Err(ApiError::Unauthorized("invalid token".into()).into_response()),
    };

    let user_id: i64 = match data.claims.sub.parse() {
        Ok(id) => id,
        Err(_) => return Err(ApiError::Unauthorized("invalid sub".into()).into_response()),
        };

    // Carica info essenziali dell'utente (email) e inietta
    let row = sqlx::query("SELECT email FROM users WHERE id = $1 AND is_active = TRUE")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()).into_response())?
        .ok_or_else(|| ApiError::Unauthorized("user not found".into()).into_response())?;

    let email: String = row.get("email");
    req.extensions_mut().insert(CurrentUser { id: user_id, email });

   Ok(next.run(req).await)
}
