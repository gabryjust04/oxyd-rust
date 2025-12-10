use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
    body::Body,
};
// jsonwebtoken is used to decode and validate JWTs
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use crate::auth::{types::{ CurrentUser, JwtClaims}};
use sqlx::Row;
use crate::general::types::AppState;
use crate::general::errors::ApiError;

/// Middleware that authenticates requests using a Bearer JWT.
///
/// - Expects `Authorization: Bearer <token>` header.
/// - Validates the token (including `exp`) using the HS256 secret found in `AppState`.
/// - Loads a minimal user record (email) from the database and injects a `CurrentUser`
///   into the request extensions for downstream handlers to consume.
/// - On success forwards the request to the next middleware/handler, otherwise returns an ApiError-converted response.
pub async fn auth_middleware(
    // extract `AppState` from the axum request state and bind it to `state`
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Result<Response, (StatusCode, String)> {
    // Read the Authorization header: return Unauthorized if missing
    let header = match req.headers().get(axum::http::header::AUTHORIZATION) {
        Some(h) => h,
        // ApiError::Unauthorized constructs an error that is then converted to a Response.
        // We return early with that response here.
        None => return Err(ApiError::Unauthorized("missing Authorization header".into()).into_response()),
    };

    // Convert header bytes to &str, handling invalid header encoding
    let header = match header.to_str() {
        Ok(h) => h,
        Err(_) => return Err(ApiError::Unauthorized("bad header".into()).into_response()),
    };

    // Expect the header to start with "Bearer " and strip the prefix to obtain the token
    let token = match header.strip_prefix("Bearer ") {
        Some(t) => t,
        None => return Err(ApiError::Unauthorized("no bearer".into()).into_response()),
    };



    // Configure JWT validation:
    // - HS256 algorithm
    // - validate_exp ensures tokens are checked for expiration (`exp` claim)
    // - leeway allows clock skew (in seconds)
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    // `leeway` is seconds of allowed clock skew. Tune or remove in production if not needed.
    validation.leeway = 10;

    // Attempt to decode and validate the token. Any error => unauthorized.
    let data = match decode::<JwtClaims>(
        token,
        &DecodingKey::from_secret(&state.jwt_secret),
        &validation,
    ) {
        Ok(d) => d,
        Err(_) => return Err(ApiError::Unauthorized("invalid token".into()).into_response()),
    };

   
    // `sub` claim is expected to contain a user id as a string. Parse to i64.
    let user_id: i64 = match data.claims.sub.parse() {
        Ok(id) => id,
        // If `sub` is not a valid integer => unauthorized
        Err(_) => return Err(ApiError::Unauthorized("invalid sub".into()).into_response()),
    };

    // Load minimal user info from the database to ensure user exists and is active.
    // We use `fetch_optional` so we can return a 401 if the row is not found.
    let row = sqlx::query("SELECT email FROM oxyd_auth.users WHERE id = $1 AND is_active = TRUE")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        // Map any database error to an internal server error response
        .map_err(|e| ApiError::Internal(e.to_string()).into_response())?
        // If no row is returned, the user doesn't exist or is inactive -> unauthorized
        .ok_or_else(|| ApiError::Unauthorized("user not found".into()).into_response())?;

    // Extract the email column from the row and build CurrentUser
    let email: String = row.get("email");
    // Store the authenticated user into request extensions so downstream handlers can access it.
    // axum's extension map is typically used for per-request data like the current user.
    req.extensions_mut().insert(CurrentUser { id: user_id, email });

    // Forward the request to the next middleware/handler and return its response.
    Ok(next.run(req).await)
}
