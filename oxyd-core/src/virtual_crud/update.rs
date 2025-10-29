// oxyd-core/src/virtual_crud/update.rs 









use std::collections::{HashMap, HashSet};

use serde_json::{ Value};
use sqlx::{Row};
use tracing::error;

use crate::general::errors::{ApiError, ApiResult};
use crate::virtual_crud::{registry,query};
use super::types::*;
use crate::general::types::AppState;











/// Esegue UPDATE e ritorna le righe aggiornate come array JSON.
/// - Richiede almeno un filtro (evita mass update).
/// - Non aggiorna colonne di PK.
/// - I valori vengono bindati come &str con cast ::tipo.
pub async fn update_rows(
    state: &AppState,
    schema: &str,
    table: &str,
    body: &serde_json::Value,
    opts: &QueryOptions,
) -> ApiResult<Value> {
    let pool = &state.pool;
    let meta = registry::load_table_meta(state, schema, table).await?;

    // body deve essere un oggetto
    let obj = body.as_object().ok_or_else(|| {
        ApiError::BadRequest("request body must be a JSON object".into())
    })?;
    if obj.is_empty() {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    if opts.filters.is_empty() {
        return Err(ApiError::BadRequest("refuse UPDATE without filters".into()));
    }

    // Set colonne consentite / tipi
    let colset: HashSet<&str> = meta.columns.iter().map(|c| c.name.as_str()).collect();
    let mut coltypes: HashMap<&str, &str> = HashMap::with_capacity(meta.columns.len());
    for c in &meta.columns {
        coltypes.insert(c.name.as_str(), c.data_type.as_str());
    }
    let pkset: HashSet<&str> = meta.primary_key.iter().map(|s| s.as_str()).collect();

    // Costruisci SET
    let mut set_parts: Vec<String> = Vec::with_capacity(obj.len());
    let mut binds: Vec<String> = Vec::new();

    for (k, v) in obj {
        query::validate_ident(k)?;
        if !colset.contains(k.as_str()) {
            return Err(ApiError::BadRequest(format!("unknown column '{}'", k)));
        }
        if pkset.contains(k.as_str()) {
            return Err(ApiError::BadRequest(format!("cannot update primary key column '{}'", k)));
        }

        let dt = *coltypes
            .get(k.as_str())
            .ok_or_else(|| ApiError::BadRequest(format!("missing type for column '{}'", k)))?;
        let cast = query::pg_cast_of(dt);

        let col = format!(r#""{}""#, k);
        if v.is_null() {
            set_parts.push(format!("{} = NULL", col));
        } else {
            // serializza il valore JSON in stringa; lato DB castiamo a ::tipo
            // NB: per i tipi testo è sicuro; per numerici/date dev'essere una stringa rappresentabile.
            let s = if v.is_string() {
                v.as_str().unwrap().to_string()
            } else {
                v.to_string()
            };
            binds.push(s);
            set_parts.push(format!("{} = ${}::{}", col, binds.len(), cast));
        }
    }

    // WHERE
    let where_sql = query::build_where_sql(&meta, opts, &mut binds)?;

    // SQL (CTE + RETURNING) → array di json come per SELECT
    let sql = format!(
        r#"
        WITH updated AS (
            UPDATE "{schema}"."{table}"
            SET {set_clause}
            {where_sql}
            RETURNING *
        )
        SELECT row_to_json(updated) AS data FROM updated
        "#,
        schema = meta.schema,
        table = meta.name,
        set_clause = set_parts.join(", "),
        where_sql = where_sql
    );

    // Esecuzione + mappatura errori come select_rows
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
                        error!(%code, db_message=%msg, sql=%sql, ?binds, "unhandled database error (update)");
                        return Err(ApiError::Internal("database error".into()));
                    }
                }
            }
            error!(error=%e, sql=%sql, ?binds, "non-database error (update)");
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
