// oxyd-core/src/general/errors.rs

use axum::http::StatusCode;

/// A convenience alias used throughout handlers/services:
/// `ApiResult<T>` means "operation that either returns `T` or an `ApiError`".
/// This keeps handler signatures compact: `async fn foo(...) -> ApiResult<Json<T>>`.
pub type ApiResult<T> = Result<T, ApiError>;

/// Domain-level error type for API handlers.
///
/// Each variant corresponds to a common HTTP error class that the service
/// might want to return to the client. Storing a `String` message with
/// each variant keeps the type simple and serializable, but see the notes
/// below about not leaking internal details for `Internal`.
#[derive(Debug)]
pub enum ApiError {
    /// 400 Bad Request — client sent invalid data or parameters.
    /// Use for validation errors, malformed JSON, missing fields, etc.
    BadRequest(String),

    /// 401 Unauthorized — missing/invalid authentication.
    Unauthorized(String),

    /// 403 Forbidden — authenticated but not allowed to access resource.
    Forbidden(String),

    /// 409 Conflict — e.g. resource already exists or version mismatch.
    Conflict(String),

    /// 404 Not Found — resource not present.
    NotFound(String),

    /// 500 Internal Server Error — unexpected failure on the server.
    /// Usually wrap internal errors here; be careful about what message
    /// you send to clients (avoid showing raw DB traces).
    Internal(String),
}

/// Allows converting from library/runtime error types into `ApiError`.
///
/// These `From` impls make it easy to use the `?` operator in your service
/// code and have library errors flow into a consistent HTTP-level error.
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        // Simple conversion: put the `anyhow` error string into Internal.
        // Advantage: easy to use. Drawback: may expose internal details.
        ApiError::Internal(e.to_string())
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        // Converting all sqlx errors to Internal keeps the enum small,
        // but you might want to map certain sqlx errors to more specific
        // HTTP codes (e.g., RowNotFound -> NotFound).
        ApiError::Internal(e.to_string())
    }
}

impl From<argon2::password_hash::Error> for ApiError {
    fn from(e: argon2::password_hash::Error) -> ApiError {
        // Password hashing/verifying errors likely indicate server issue or
        // malformed input; mapping them to Internal is reasonable.
        ApiError::Internal(e.to_string())
    }
}

impl ApiError {
    /// Convert `ApiError` into a tuple `(StatusCode, String)` that can be
    /// used directly by frameworks that accept `(StatusCode, Body)`.
    ///
    /// This method centralizes the HTTP status mapping for each error variant.
    /// You return a raw string body here — in a real API you may prefer to
    /// return JSON with an `error` object and an optional correlation id.
    pub fn into_response(self) -> (StatusCode, String) {
        match self {
            ApiError::BadRequest(m)   => (StatusCode::BAD_REQUEST, m),
            ApiError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m),
            ApiError::Forbidden(m)    => (StatusCode::FORBIDDEN, m),
            ApiError::Conflict(m)     => (StatusCode::CONFLICT, m),
            ApiError::NotFound(m)     => (StatusCode::NOT_FOUND, m),
            ApiError::Internal(m)     => (StatusCode::INTERNAL_SERVER_ERROR, m),
        }
    }
}
