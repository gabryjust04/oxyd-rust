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

/// REQUIRED extraction: the route fails with 401 if the user is not authenticated.
///
/// Behaviour:
/// 1. Try to read `CurrentUser` from request `extensions` (populated by middleware).
/// 2. If not present, fall back to verifying the Authorization header token directly
///    and loading the user row from the DB.
/// This wrapper type is used as an extractor in handlers: `RequireUser` forces auth.
pub struct RequireUser(pub CurrentUser);

/// OPTIONAL extraction: returns `Some(user)` if authenticated, otherwise `None`.
///
/// Behaviour:
/// - If CurrentUser is already in `extensions`, return it.
/// - Otherwise, try to parse & verify Authorization header; if that fails, return `None`
///   (do NOT fail the request).
pub struct OptionalUser(pub Option<CurrentUser>);

// Note: the #[async_trait] lines are commented out — not needed for `FromRequestParts`
// because the trait uses `async fn` directly (no external async_trait crate required).

impl FromRequestParts<AppState> for RequireUser {
    // The rejection type is a raw (StatusCode, String) tuple; you could instead
    // use a dedicated error type that implements `IntoResponse`.
    type Rejection = (StatusCode, String);

    // This function runs when Axum tries to create `RequireUser` from an incoming request.
    // `parts` is the request *parts* (headers, extensions, etc) and `state` is the app state.
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        // 1) If middleware already inserted CurrentUser into extensions, use it.
        //    This avoids an extra DB call and double-parsing the token.
        if let Some(cu) = parts.extensions.get::<CurrentUser>() {
            // clone required: `extensions.get` yields a reference; the extractor holds owned data.
            return Ok(RequireUser(cu.clone()));
        }

        // 2) Fallback: verify the token directly from the Authorization header.
        //    `bearer_from_headers` returns `Option<String>`, so convert absent -> Unauthorized.
        let token = bearer_from_headers(&parts.headers)
            .ok_or_else(|| ApiError::Unauthorized("missing Authorization header".into()).into_response())?;

        // Configure JWT validation:
        // - Use HS256 algorithm (this should match how tokens are issued).
        // - validate_exp = true enforces `exp` claim checking.
        let mut v = Validation::new(Algorithm::HS256);
        v.validate_exp = true;

        // Decode and verify token -> map any decode error to Unauthorized.
        let data = decode::<JwtClaims>(&token, &DecodingKey::from_secret(&state.jwt_secret), &v)
            .map_err(|_| ApiError::Unauthorized("invalid token".into()).into_response())?;

        // `sub` claim should contain the user id as string; parse to i64.
        let user_id: i64 = data
            .claims
            .sub
            .parse()
            .map_err(|_| ApiError::Unauthorized("invalid sub".into()).into_response())?;

        // Load user email from DB and verify the user is active.
        // If DB fails -> Internal, if user not found -> Unauthorized.
        let row = sqlx::query("SELECT email FROM users WHERE id = $1 AND is_active = TRUE")
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()).into_response())? // DB failure
            .ok_or_else(|| ApiError::Unauthorized("user not found".into()).into_response())?; // no row

        let email: String = row.get("email");
        Ok(RequireUser(CurrentUser { id: user_id, email }))
    }
}

impl FromRequestParts<AppState> for OptionalUser {
    type Rejection = (StatusCode, String);

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        // If middleware already set CurrentUser, return it.
        if let Some(cu) = parts.extensions.get::<CurrentUser>() {
            return Ok(OptionalUser(Some(cu.clone())));
        }

        // If there's no Authorization header, return `None` (optional extractor mustn't error).
        let Some(token) = bearer_from_headers(&parts.headers) else {
            return Ok(OptionalUser(None));
        };

        let mut v = Validation::new(Algorithm::HS256);
        v.validate_exp = true;

        // Try decoding the token; on any error, treat as unauthenticated (return None).
        let data = match decode::<JwtClaims>(&token, &DecodingKey::from_secret(&state.jwt_secret), &v) {
            Ok(d) => d,
            Err(_) => return Ok(OptionalUser(None)),
        };

        // Parse sub -> user_id; if invalid, return None (don't treat as route error).
        let user_id: i64 = match data.claims.sub.parse() {
            Ok(id) => id,
            Err(_) => return Ok(OptionalUser(None)),
        };

        // Try to fetch the user's email. Any DB error or missing user -> return None.
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
    // Get the Authorization header, validate it's UTF-8, and strip the "Bearer " prefix.
    let h = headers.get(axum::http::header::AUTHORIZATION)?;
    let s = h.to_str().ok()?;
    s.strip_prefix("Bearer ").map(|t| t.to_string())
}
