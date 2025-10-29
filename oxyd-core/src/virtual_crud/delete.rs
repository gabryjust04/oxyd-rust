// oxyd-core/src/virtual_crud/delete.rs


use serde_json::{Value};
use sqlx::{ Row};
use tracing::error;

use crate::{general::errors::{ApiError, ApiResult}, virtual_crud::{query, registry}};
use super::types::*;
use crate::general::types::AppState;


/// Esegue DELETE e ritorna le righe eliminate come array JSON.
/// - Richiede almeno un filtro (evita mass delete).
pub async fn delete_rows(
    state: &AppState,
    schema: &str,
    table: &str,
    opts: &QueryOptions,
) -> ApiResult<Value> {
    let pool = &state.pool;
    let meta = registry::load_table_meta(state, schema, table).await?;

    if opts.filters.is_empty() {
        return Err(ApiError::BadRequest("refuse DELETE without filters".into()));
    }

    let mut binds: Vec<String> = Vec::new();
    let where_sql = query::build_where_sql(&meta, opts, &mut binds)?;

    let sql = format!(
        r#"
        WITH deleted AS (
            DELETE FROM "{schema}"."{table}"
            {where_sql}
            RETURNING *\
        )
        SELECT row_to_json(deleted) AS data FROM deleted
        "#,
        schema = meta.schema,
        table = meta.name,
        where_sql = where_sql
    );

    let mut q = sqlx::query(&sql);
    for b in &binds {
        q = q.bind(b.as_str());
    }

    let rows = match q.fetch_all(pool).await {
        Ok(rows) => rows,
        Err(e) => {
            use sqlx::Error;
            if let Error::Database(db_err) = &e {
                let code_opt = db_err.code();
                let code = code_opt.as_deref().unwrap_or("");
                let msg = db_err.message();
                match code {
                    "22P02" | "22007" | "42804" | "42846" | "42883" => {
                        return Err(ApiError::BadRequest(msg.to_string()))
                    }
                    "42703" => return Err(ApiError::BadRequest(format!("unknown column/expression: {}", msg))),
                    "42P01" => {
                        return Err(ApiError::NotFound(format!(r#"table "{}"."{}" not found"#, schema, table)))
                    }
                    _ => {
                        error!(%code, db_message=%msg, sql=%sql, ?binds, "unhandled database error (delete)");
                        return Err(ApiError::Internal("database error".into()));
                    }
                }
            }
            error!(error=%e, sql=%sql, ?binds, "non-database error (delete)");
            return Err(ApiError::Internal("internal error".into()));
        }
    };

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let v: serde_json::Value = r.get::<serde_json::Value, _>("data");
        out.push(v);
    }
    Ok(serde_json::Value::Array(out))
}
