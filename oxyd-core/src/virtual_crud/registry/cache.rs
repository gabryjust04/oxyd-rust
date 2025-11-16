// oxyd-core/src/virtual_crud/registry/cache.rs

use std::time::Duration;

use moka::future::Cache;

use crate::virtual_crud::types::{OxydTableConfig, TableMeta};

/// In-memory cache per il registry, condivisa via AppState.
#[derive(Clone)]
pub struct RegistryCache {
    /// Esistenza della tabella oxyd_internal._oxyd_tables (true/false).
    pub table_exists: Cache<String, bool>,

    /// Config per ogni tabella (valore di ritorno di `load_table_config`).
    /// - None  = registro non esiste (DEV mode)
    /// - Some( OxydTableConfig ) = registro esiste, tabella configurata/fabbricata
    pub table_config: Cache<String, Option<OxydTableConfig>>,

    /// Metadati strutturali delle tabelle (schema + name → TableMeta).
    pub table_meta: Cache<String, TableMeta>,
}

impl RegistryCache {
    pub fn new() -> Self {
        let ttl = Duration::from_secs(60);

        RegistryCache {
            table_exists: Cache::builder()
                .max_capacity(100)
                .time_to_live(ttl)
                .build(),

            table_config: Cache::builder()
                .max_capacity(1_000)
                .time_to_live(ttl)
                .build(),

            table_meta: Cache::builder()
                .max_capacity(1_000)
                .time_to_live(ttl)
                .build(),
        }
    }
}
