use sqlx::PgPool;
use std::{sync::Arc, time::Duration};


#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt_secret: Arc<[u8]>,
    pub access_ttl: Duration,
    pub refresh_ttl: Duration,
}

impl AppState {
    pub fn new(pool: PgPool, jwt_secret: Arc<[u8]>, access_ttl: Duration, refresh_ttl: Duration) -> Self {
        Self { pool, jwt_secret, access_ttl, refresh_ttl }
    }
}