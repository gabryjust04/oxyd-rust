// oxyd-core/src/virtual_crud/registry/mod.rs

mod cache;
mod load;

pub use cache::RegistryCache;

use crate::general::errors::{ApiError, ApiResult};
use crate::general::types::AppState;
use crate::virtual_crud::types::{OxydTableConfig, TableMeta};

use load::{
    db_check_table_exists,
    db_fetch_table_config,
    db_load_pk_columns,
    db_load_table_columns,
    db_oxyd_tables_exists,
};

/// Chiave fissa per cache di esistenza del registry.
const OXYD_TABLES_EXISTS_KEY: &str = "oxyd_internal._oxyd_tables";

/// Versione cachata di "esiste oxyd_internal._oxyd_tables?".
///
/// - usa Moka con TTL 60s (configurato in RegistryCache::new)
/// - se non è in cache, va sul DB e poi inserisce in cache
async fn oxyd_tables_exists(state: &AppState) -> Result<bool, sqlx::Error> {
    if let Some(v) = state
        .registry_cache
        .table_exists
        .get(OXYD_TABLES_EXISTS_KEY)
        .await
    {
        return Ok(v);
    }

    let exists = db_oxyd_tables_exists(&state.pool).await?;
    state
        .registry_cache
        .table_exists
        .insert(OXYD_TABLES_EXISTS_KEY.to_string(), exists)
        .await;

    Ok(exists)
}

/// Carica la configurazione per una tabella dal registry, con cache.
///
/// Semantica come prima:
/// - Se la tabella `_oxyd_tables` **non esiste** → Ok(None)
/// - Se `_oxyd_tables` esiste:
///     - se la riga per `table` esiste → Some(cfg)
///     - se non esiste → Some(cfg "locked down", is_exposed=false, allow_*=false)
pub async fn load_table_config(
    state: &AppState,
    table: &str,
) -> ApiResult<Option<OxydTableConfig>> {
    // Se il registry non esiste → DEV mode, nessuna config centrale.
    if !oxyd_tables_exists(state)
        .await
        .map_err(ApiError::from)?
    {
        return Ok(None);
    }

    // Registry esiste → controlla la cache della singola tabella.
    if let Some(cached) = state.registry_cache.table_config.get(table).await {
        return Ok(cached);
    }

    // Non in cache → vai sul DB.
    let from_db = db_fetch_table_config(&state.pool, table)
        .await
        .map_err(ApiError::from)?;

    let cfg = match from_db {
        Some(cfg) => Some(cfg),
        None => {
            // Registry esiste ma tabella non registrata:
            // fabbrichiamo una config "locked down" come prima.
            Some(OxydTableConfig {
                table_name:   table.to_string(),
                is_exposed:   false,
                require_auth: true,
                allow_insert: false,
                allow_update: false,
                allow_delete: false,
                description:  None,
            })
        }
    };

    // Salva in cache (anche la variante "fabbricata").
    state
        .registry_cache
        .table_config
        .insert(table.to_string(), cfg.clone())
        .await;

    Ok(cfg)
}

/// Carica i metadati strutturali della tabella (schema, colonne, PK) con cache.
///
/// Semantica come prima:
/// - Se la tabella non esiste → ApiError::NotFound("table '...' not found")
pub async fn load_table_meta(
    state: &AppState,
    schema: &str,
    table: &str,
) -> ApiResult<TableMeta> {
    let key = format!("{}.{}", schema, table);

    if let Some(meta) = state.registry_cache.table_meta.get(&key).await {
        return Ok(meta);
    }

    // 1) Esistenza tabella
    let exists = db_check_table_exists(&state.pool, schema, table)
        .await
        .map_err(ApiError::from)?;

    if !exists {
        return Err(ApiError::NotFound(format!(
            "table '{}' not found",
            table
        )));
    }

    // 2) Colonne
    let cols = db_load_table_columns(&state.pool, schema, table)
        .await
        .map_err(ApiError::from)?;

    // 3) Primary key (anche composite)
    let pk_cols = db_load_pk_columns(&state.pool, schema, table)
        .await
        .map_err(ApiError::from)?;

    let meta = TableMeta {
        schema:      schema.to_string(),
        name:        table.to_string(),
        primary_key: pk_cols,
        columns:     cols,
    };

    state
        .registry_cache
        .table_meta
        .insert(key, meta.clone())
        .await;

    Ok(meta)
}

/// Garantisce che la tabella sia esposta dal registry e ritorna il flag `require_auth`.
///
/// Semantica identica alla versione precedente:
/// - Se il registry **non esiste** → DEV mode: tabella esposta, require_auth = false
/// - Se il registry esiste:
///     - se `is_exposed = false` → ApiError::NotFound (come se la tabella non esistesse)
///     - se esposta          → Ok(require_auth)
pub async fn ensure_exposed(state: &AppState, table: &str) -> ApiResult<bool> {
    match load_table_config(state, table).await? {
        // Registry assente → nessuna auth obbligatoria (dev mode aperto)
        None => Ok(false),

        Some(cfg) => {
            if !cfg.is_exposed {
                return Err(ApiError::NotFound(format!(
                    "table '{}' not exposed",
                    table
                )));
            }
            Ok(cfg.require_auth)
        }
    }
}
