// oxyd-core/src/virtual_crud/registry/load.rs

use sqlx::{postgres::PgRow, PgPool, Row};

use crate::virtual_crud::types::{ColumnMeta, OxydTableConfig, TableMeta};

/// Controlla se esiste oxyd_internal._oxyd_tables.
pub async fn db_oxyd_tables_exists(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let exists: Option<bool> = sqlx::query_scalar(
        r#"
        SELECT TRUE
        FROM information_schema.tables
        WHERE table_schema = 'oxyd_internal'
          AND table_name   = '_oxyd_tables'
        LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?;

    Ok(exists.unwrap_or(false))
}

/// Controlla se esiste una tabella generica (schema, name).
pub async fn db_check_table_exists(
    pool: &PgPool,
    schema: &str,
    table: &str,
) -> Result<bool, sqlx::Error> {
    let exists: Option<bool> = sqlx::query_scalar(
        r#"
        SELECT TRUE
        FROM information_schema.tables
        WHERE table_schema = $1
          AND table_name   = $2
        LIMIT 1
        "#,
    )
    .bind(schema)
    .bind(table)
    .fetch_optional(pool)
    .await?;

    Ok(exists.unwrap_or(false))
}

/// Carica le colonne di una tabella da information_schema.
pub async fn db_load_table_columns(
    pool: &PgPool,
    schema: &str,
    table: &str,
) -> Result<Vec<ColumnMeta>, sqlx::Error> {
    let cols = sqlx::query(
        r#"
        SELECT column_name,
               data_type,
               is_nullable,
               column_default
        FROM information_schema.columns
        WHERE table_schema = $1
          AND table_name   = $2
        ORDER BY ordinal_position
        "#,
    )
    .bind(schema)
    .bind(table)
    .map(|row: PgRow| ColumnMeta {
        name:        row.get::<String, _>("column_name"),
        data_type:   row.get::<String, _>("data_type"),
        is_nullable: row.get::<String, _>("is_nullable") == "YES",
        has_default: row
            .get::<Option<String>, _>("column_default")
            .is_some(),
    })
    .fetch_all(pool)
    .await?;

    Ok(cols)
}

/// Carica le colonne della primary key (anche PK composte).
pub async fn db_load_pk_columns(
    pool: &PgPool,
    schema: &str,
    table: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let pk_cols = sqlx::query_scalar::<_, String>(
        r#"
        SELECT a.attname AS col
        FROM pg_index i
        JOIN pg_attribute a
          ON a.attrelid = i.indrelid
         AND a.attnum   = ANY(i.indkey)
        JOIN pg_class c
          ON c.oid = i.indrelid
        JOIN pg_namespace n
          ON n.oid = c.relnamespace
        WHERE i.indisprimary = TRUE
          AND n.nspname      = $1
          AND c.relname      = $2
        ORDER BY a.attnum
        "#,
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(pk_cols)
}

/// Carica la configurazione di una tabella dal registro.
/// ATTENZIONE: qui assumiamo che oxyd_internal._oxyd_tables esista già.
/// - Some(cfg) se la riga esiste
/// - None se non c'è riga per quella tabella
pub async fn db_fetch_table_config(
    pool: &PgPool,
    table: &str,
) -> Result<Option<OxydTableConfig>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT table_name,
               is_exposed,
               require_auth,
               allow_insert,
               allow_update,
               allow_delete,
               description
        FROM oxyd_internal._oxyd_tables
        WHERE table_name = $1
        "#,
    )
    .bind(table)
    .fetch_optional(pool)
    .await?;

    if let Some(r) = row {
        Ok(Some(OxydTableConfig {
            table_name:   r.get("table_name"),
            is_exposed:   r.get("is_exposed"),
            require_auth: r.get("require_auth"),
            allow_insert: r.get("allow_insert"),
            allow_update: r.get("allow_update"),
            allow_delete: r.get("allow_delete"),
            description:  r.get::<Option<String>, _>("description"),
        }))
    } else {
        Ok(None)
    }
}
