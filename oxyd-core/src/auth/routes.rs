use axum::{
    routing::{post, get},
    Router,
    extract::{State, Json, Extension},
};
use crate::auth::{
    types::{ RegisterBody, LoginBody, RefreshBody, AuthResponse, CurrentUser, PublicUser},
    service,
    middleware,
};

use crate::general::types::AppState;

/// Build and return the router for the `auth` module.
///
/// - mounts public endpoints for register/login/refresh
/// - mounts a protected endpoint (`/auth/me`) guarded by an auth middleware
/// - attaches the shared `AppState` to all routes
pub fn router(state: AppState) -> Router {
    // Public routes (no authentication required)
    let public = Router::new()
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/auth/refresh", post(refresh));

    // Protected routes (demonstration): `/auth/me`
    // `auth_middleware` is applied here to verify the request and inject the user.
    let protected = Router::new()
        .route("/auth/me", get(me))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_middleware,
        ));

    // Merge public and protected routes and attach the shared state
    Router::new()
        .merge(public)
        .merge(protected)
        .with_state(state)
}

/* handlers */

/// Register a new user.
///
/// Expects a JSON body matching `RegisterBody` (email + password).
/// On success returns an `AuthResponse` (typically contains tokens / user info).
/// Service errors are converted into an HTTP status + message pair.
async fn register(
    State(state): State<AppState>,
    Json(body): Json<RegisterBody>,
) -> Result<Json<AuthResponse>, (axum::http::StatusCode, String)> {
    // call the service layer and translate service errors to HTTP responses
    service::register(&state, &body.email, &body.password)
        .await
        .map(Json)
        .map_err(|e| e.into_response())
}

/// Log in an existing user.
///
/// Expects `LoginBody` (email + password) and returns `AuthResponse` on success.
/// Errors from the service are mapped to HTTP responses.
async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginBody>,
) -> Result<Json<AuthResponse>, (axum::http::StatusCode, String)> {
    service::login(&state, &body.email, &body.password)
        .await
        .map(Json)
        .map_err(|e| e.into_response())
}

/// Exchange a refresh token for a new `AuthResponse`.
///
/// Expects `RefreshBody` with the refresh token. On success returns new tokens/user info.
/// Service errors are converted to HTTP status + message.
async fn refresh(
    State(state): State<AppState>,
    Json(body): Json<RefreshBody>,
) -> Result<Json<AuthResponse>, (axum::http::StatusCode, String)> {
    service::refresh(&state, &body.refresh_token)
        .await
        .map(Json)
        .map_err(|e| e.into_response())
}

/// Return the public view of the currently authenticated user.
///
/// The `CurrentUser` is injected into the request extensions by the `auth_middleware`.
/// This handler simply maps `CurrentUser` to `PublicUser` and returns it as JSON.
///
/// Note: because `Extension<CurrentUser>` is used, this route will only be hit if the
/// middleware has already validated the request and inserted the user.
async fn me(
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<PublicUser>, (axum::http::StatusCode, String)> {
    Ok(Json(PublicUser { id: user.id, email: user.email.clone() }))
}
