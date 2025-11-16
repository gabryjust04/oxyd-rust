// oxyd-core/src/virtual_crud/registry.rs


use sqlx::{postgres::PgRow, Row};

use crate::general::errors::{ApiError, ApiResult};
use super::types::*;
use crate::general::types::AppState;



/// Controlla se esiste la tabella di registry `oxyd_internal._oxyd_tables`.
async fn oxyd_tables_exists(state: &AppState) -> Result<bool, sqlx::Error> {
    let pool = &state.pool;
    let exists: Option<bool> = sqlx::query_scalar(
        r#"
        SELECT TRUE
        FROM information_schema.tables
        WHERE table_schema = 'oxyd_internal' AND table_name = '_oxyd_tables'
        LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?;
    Ok(exists.unwrap_or(false))
}

/// Carica la config dalla `oxyd_internal._oxyd_tables`.
/// Se la tabella registry non esiste → Ok(None) (ambiente dev: tutto esposto).
pub async fn load_table_config(state: &AppState, table: &str) -> ApiResult<Option<OxydTableConfig>> {
    if !oxyd_tables_exists(state).await.map_err(ApiError::from)? {
        return Ok(None);
    }

    let row = sqlx::query(
        r#"
        SELECT table_name, is_exposed, require_auth, allow_insert, allow_update, allow_delete, description
        FROM oxyd_internal._oxyd_tables
        WHERE table_name = $1
        "#,
    )
    .bind(table)
    .fetch_optional(&state.pool)
    .await?;

    if let Some(r) = row {
        Ok(Some(OxydTableConfig {
            table_name: r.get("table_name"),
            is_exposed: r.get("is_exposed"),
            require_auth: r.get("require_auth"),
            allow_insert: r.get("allow_insert"),
            allow_update: r.get("allow_update"),
            allow_delete: r.get("allow_delete"),
            description: r.get::<Option<String>, _>("description"),
        }))
    } else {
        // Registry presente ma tabella non registrata → trattala come non esposta
        Ok(Some(OxydTableConfig {
            table_name: table.to_string(),
            is_exposed: false,
            require_auth: true,
            allow_insert: false,
            allow_update: false,
            allow_delete: false,
            description: None,
        }))
    }
}


/// Legge metadati tabella da information_schema / pg_catalog (nessuna cache).
pub async fn load_table_meta(state: &AppState, schema: &str, table: &str) -> ApiResult<TableMeta> {
    let pool = &state.pool;
    // Verifica esistenza tabella
    let exists: Option<bool> = sqlx::query_scalar(
        r#"
        SELECT TRUE
        FROM information_schema.tables
        WHERE table_schema = $1 AND table_name = $2
        LIMIT 1
        "#,
    )
    .bind(schema)
    .bind(table)
    .fetch_optional(pool)
    .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound(format!("table '{}' not found", table)));
    }

    // Colonne
    let cols = sqlx::query(
        r#"
        SELECT column_name, data_type, is_nullable, column_default
        FROM information_schema.columns
        WHERE table_schema = $1 AND table_name = $2
        ORDER BY ordinal_position
        "#,
    )
    .bind(schema)
    .bind(table)
    .map(|row: PgRow| ColumnMeta {
        name: row.get::<String, _>("column_name"),
        data_type: row.get::<String, _>("data_type"),
        is_nullable: row.get::<String, _>("is_nullable") == "YES",
        has_default: row.get::<Option<String>, _>("column_default").is_some(),
    })
    .fetch_all(pool)
    .await?;

    // Primary key (anche composita)
    let pk_cols = sqlx::query_scalar::<_, String>(
        r#"
        SELECT a.attname AS col
        FROM pg_index i
        JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey)
        JOIN pg_class c ON c.oid = i.indrelid
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE i.indisprimary = TRUE
          AND n.nspname = $1
          AND c.relname = $2
        ORDER BY a.attnum
        "#,
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(TableMeta {
        schema: schema.to_string(),
        name: table.to_string(),
        primary_key: pk_cols,
        columns: cols,
    })
}

/// Verifica esposizione della tabella usando `oxyd_internal._oxyd_tables`.
/// - Se registry NON esiste → consenti.
/// - Se esiste e la tabella non è registrata o non esposta → NotFound.
/// - Ritorna anche il flag `require_auth` per far decidere al caller.
pub async fn ensure_exposed(state: &AppState, table: &str) -> ApiResult<bool> {
    match load_table_config(state, table).await? {
        None => Ok(false), // registry assente → require_auth = false (ambiente dev)
        Some(cfg) => {
            if !cfg.is_exposed {
                return Err(ApiError::NotFound(format!("table '{}' not exposed", table)));
            }
            Ok(cfg.require_auth)
        }
    }
}
