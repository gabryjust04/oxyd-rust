//! Auth services: register/login/refresh based on
//! - Argon2 password hashing (PHC string format)
//! - Short-lived JWT access tokens
//! - Opaque refresh tokens stored as SHA-256 hashes with rotation
//!
//! Expected schema (minimal):
//!   users(id BIGINT PK, email TEXT UNIQUE, password_hash TEXT, is_active BOOL)
//!   refresh_sessions(id BIGINT PK, user_id BIGINT FK, token_hash TEXT UNIQUE,
//!                    revoked BOOL DEFAULT FALSE, replaced_by BIGINT NULL,
//!                    expires_at TIMESTAMPTZ NOT NULL)
//
//! Notes:
//! - We only ever persist a *hash* of the refresh token (never the raw token).
//! - Refresh rotation: using a valid refresh revokes it and issues a new one.
//! - JWT claims kept minimal (sub/iat/exp). Add iss/aud if your system requires it.

use crate::auth::{ types::*};
use crate::general::types::AppState;
use crate::general::errors::*;

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use base64::Engine;
use chrono::{Duration as ChronoDur, Utc};
use jsonwebtoken::{encode, EncodingKey, Header};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, Row};
use std::sync::Arc;

#[derive(Debug, FromRow)]
struct UserRow {
    id: i64,
    email: String,
    password_hash: String,
    is_active: bool,
}

/* ========== PUBLIC API (services) ========== */

/// Register a new user (email must be unique) and immediately issue:
/// - a short-lived access JWT
/// - a long-lived opaque refresh token (stored server-side as hash)
///
/// Returns conflict if the email already exists.
///
/// Security:
/// - Passwords are hashed with Argon2 using a per-user random salt.
/// - Consider tuning Argon2 params (m_cost, t_cost, p) via `Argon2::new(...)`
///   according to your hardware and threat model.
pub async fn register(state: &AppState, email: &str, password: &str) -> ApiResult<AuthResponse> {
    // Basic password policy demo; extend as needed (length, charset, pwned checks, etc.).
    if password.len() < 8 {
        return Err(ApiError::BadRequest("password too short".into()));
    }

    // Hash the password with a fresh random salt; output is a PHC string.
    let salt = SaltString::generate(&mut OsRng);
    let pwd_hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)?
        .to_string();

    // Try to insert the user; ON CONFLICT ensures we don't leak whether the email exists
    // beyond returning a generic conflict error.
    let res = sqlx::query_as::<_, UserRow>(
        "INSERT INTO oxyd_auth.users (email, password_hash)
         VALUES ($1, $2)
         ON CONFLICT (email) DO NOTHING
         RETURNING id, email, password_hash, is_active",
    )
    .bind(email)
    .bind(&pwd_hash)
    .fetch_optional(&state.pool)
    .await?;

    let user = match res {
        Some(u) => u,
        None => return Err(ApiError::Conflict("email already exists".into())),
    };

    // Issue access and refresh tokens.
    let access = issue_access_token(user.id, &state.jwt_secret, state.access_ttl);
    let (refresh_token, token_hash) = generate_refresh(&state.refresh_ttl);

    // Persist only the *hash* of the refresh token with its expiry.
    sqlx::query(
        "INSERT INTO oxyd_auth.refresh_sessions (user_id, token_hash, expires_at)
         VALUES ($1, $2, $3)",
    )
    .bind(user.id)
    .bind(&token_hash)
    .bind(Utc::now() + ChronoDur::from_std(state.refresh_ttl).unwrap())
    .execute(&state.pool)
    .await?;

    Ok(AuthResponse {
        access_token: access,
        refresh_token,
        user: PublicUser { id: user.id, email: user.email },
    })
}

/// Authenticate a user with email/password, returning a fresh access JWT and refresh.
/// Fails with `Unauthorized` on any credential mismatch.
///
/// Notes:
/// - `is_active` gate keeps disabled/suspended accounts.
/// - Password verification uses the hash parameters embedded in the PHC string.
pub async fn login(state: &AppState, email: &str, password: &str) -> ApiResult<AuthResponse> {
    // Fetch by email; don't reveal whether the email exists on failure.
    let user = sqlx::query_as::<_, UserRow>(
        "SELECT id, email, password_hash, is_active FROM oxyd_auth.users WHERE email = $1",
    )
    .bind(email)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::Unauthorized("invalid credentials".into()))?;

    if !user.is_active {
        return Err(ApiError::Unauthorized("inactive account".into()));
    }

    // Verify the supplied password against the stored Argon2 PHC string.
    let parsed = PasswordHash::new(&user.password_hash)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| ApiError::Unauthorized("invalid credentials".into()))?;

    // Issue tokens and persist refresh hash.
    let access = issue_access_token(user.id, &state.jwt_secret, state.access_ttl);
    let (refresh_token, token_hash) = generate_refresh(&state.refresh_ttl);

    sqlx::query(
        "INSERT INTO oxyd_auth.refresh_sessions (user_id, token_hash, expires_at)
         VALUES ($1, $2, $3)",
    )
    .bind(user.id)
    .bind(&token_hash)
    .bind(Utc::now() + ChronoDur::from_std(state.refresh_ttl).unwrap())
    .execute(&state.pool)
    .await?;

    Ok(AuthResponse {
        access_token: access,
        refresh_token,
        user: PublicUser { id: user.id, email: user.email },
    })
}

/// Rotate a refresh token:
/// - Checks the presented refresh (by comparing its SHA-256 hash) is valid, not revoked, and not expired.
/// - Revokes the old session record.
/// - Creates a new refresh session and returns a fresh raw refresh token + new access JWT.
///
/// This prevents replay: the moment a refresh is used, it is invalidated.
pub async fn refresh(state: &AppState, presented_refresh: &str) -> ApiResult<AuthResponse> {
    // Hash the presented refresh token to compare against the DB.
    let token_hash = sha256_hex(presented_refresh);

    #[derive(FromRow)]
    struct SessRow {
        id: i64,
        user_id: i64,
    }

    // Look up an active, non-revoked, non-expired session for this hash.
    let sess = sqlx::query_as::<_, SessRow>(
        "SELECT id, user_id
         FROM oxyd_auth.refresh_sessions
         WHERE token_hash = $1
           AND revoked = FALSE
           AND expires_at > NOW()",
    )
    .bind(&token_hash)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::Unauthorized("invalid or expired refresh".into()))?;

    // Generate the new refresh before mutating state, so we can persist it atomically.
    let (new_refresh, new_hash) = generate_refresh(&state.refresh_ttl);

    // Rotate within a transaction:
    // 1) revoke the old session
    // 2) insert the new session (linking via replaced_by for auditability)
    let mut tx = state.pool.begin().await?;

    sqlx::query("UPDATE oxyd_auth.refresh_sessions SET revoked = TRUE WHERE id = $1")
        .bind(sess.id)
        .execute(&mut *tx)
        .await?;

    let new_id: i64 = sqlx::query(
        "INSERT INTO oxyd_auth.refresh_sessions (user_id, token_hash, expires_at)
         VALUES ($1, $2, $3)
         RETURNING id",
    )
    .bind(sess.user_id)
    .bind(&new_hash)
    .bind(Utc::now() + ChronoDur::from_std(state.refresh_ttl).unwrap())
    .fetch_one(&mut *tx)
    .await?
    .get(0);

    sqlx::query("UPDATE oxyd_auth.refresh_sessions SET replaced_by = $1 WHERE id = $2")
        .bind(new_id)
        .bind(sess.id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    // Mint a new access JWT; load user's email for the response payload.
    let access = issue_access_token(sess.user_id, &state.jwt_secret, state.access_ttl);
    let email = load_email(sess.user_id, state).await?;

    Ok(AuthResponse {
        access_token: access,
        refresh_token: new_refresh,
        user: PublicUser { id: sess.user_id, email },
    })
}

/* ========== Shared helpers ========== */

/// Load the public shape of a user (id/email) ensuring the account is active.
pub async fn load_public_user(user_id: i64, state: &AppState) -> ApiResult<PublicUser> {
    let row = sqlx::query("SELECT id, email FROM oxyd_auth.users WHERE id = $1 AND is_active = TRUE")
        .bind(user_id)
        .fetch_one(&state.pool)
        .await?;
    Ok(PublicUser { id: row.get("id"), email: row.get("email") })
}

/// Load only the email for a given user id (no active check).
async fn load_email(user_id: i64, state: &AppState) -> ApiResult<String> {
    let row = sqlx::query("SELECT email FROM oxyd_auth.users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&state.pool)
        .await?;
    Ok(row.get::<String, _>("email"))
}

/// Issue a signed JWT access token for `user_id` with the given TTL.
///
/// Claims:
/// - `sub`: user id as string
/// - `iat`: issued-at (seconds since epoch)
/// - `exp`: expiration (seconds since epoch)
///
/// Consider adding `iss`/`aud`, key rotation (kid header), and stronger Header if required.
pub fn issue_access_token(user_id: i64, secret: &Arc<[u8]>, ttl: std::time::Duration) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock went backwards")
        .as_secs() as i64;

    let exp = now + ttl.as_secs() as i64;
    let claims = JwtClaims { sub: user_id.to_string(), iat: now, exp };

    // HS256 by default; ensure `secret` has enough entropy and manage rotation out of band.
    encode(&Header::default(), &claims, &EncodingKey::from_secret(secret)).expect("jwt")
}

/// Generate a new opaque refresh token and its SHA-256 hex hash.
///
/// - The raw token is a URL-safe base64 string of 32 random bytes.
/// - Only the hash should be persisted.
/// - `_ttl` is not embedded in the token; expiry is enforced in the DB (`expires_at`).
pub fn generate_refresh(_ttl: &std::time::Duration) -> (String, String) {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);

    // URL-safe, no padding → comfortable for cookies/headers.
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let hash = sha256_hex(&token);
    (token, hash)
}

/// Hex-encoded SHA-256 of a string (used for refresh token storage & lookup).
fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hex::encode(hasher.finalize())
}
