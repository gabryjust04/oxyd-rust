use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use crate::virtual_crud::registry::RegistryCache;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt_secret: Arc<[u8]>,
    pub access_ttl: Duration,
    pub refresh_ttl: Duration,
    pub registry_cache: RegistryCache,
}

impl AppState {
    pub fn new(pool: PgPool, jwt_secret: Arc<[u8]>, access_ttl: Duration, refresh_ttl: Duration, registry_cache: RegistryCache) -> Self {
        Self { pool, jwt_secret, access_ttl, refresh_ttl, registry_cache }
    }
}