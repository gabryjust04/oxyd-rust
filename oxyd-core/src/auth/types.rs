use serde::{Deserialize, Serialize};




#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtClaims {
    pub sub: String, // user_id
    pub iat: i64,
    pub exp: i64,
}

#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub id: i64,
    pub email: String,
}

/* DTO */

#[derive(Deserialize)]
pub struct RegisterBody {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct LoginBody {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct RefreshBody {
    pub refresh_token: String,
}

#[derive(Serialize)]
pub struct PublicUser {
    pub id: i64,
    pub email: String,
}

#[derive(Serialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub user: PublicUser,
}
