use axum::http::StatusCode;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    Unauthorized(String),
    Forbidden(String),
    Conflict(String),
    NotFound(String),
    Internal(String),
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self { ApiError::Internal(e.to_string()) }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self { ApiError::Internal(e.to_string()) }
}

impl From<argon2::password_hash::Error> for ApiError {
    fn from(e: argon2::password_hash::Error) -> Self { ApiError::Internal(e.to_string()) }
}

impl ApiError {
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
