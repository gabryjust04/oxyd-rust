// oxyd-core/src/virtual_crud/insert.rs

use std::collections::{HashMap, HashSet};


use serde_json::{ Value};
use sqlx::{ Row};
use tracing::error;

use crate::{general::errors::{ApiError, ApiResult}, virtual_crud::{query, registry}};
use crate::general::types::AppState;


/// Esegue INSERT (singolo o bulk) e ritorna le righe inserite come array JSON.
/// - body = { ... }  → singolo insert
/// - body = [ {...}, {...} ] → bulk insert
/// - colonne = unione delle chiavi; per chiavi mancanti in una riga → DEFAULT
/// - valori null → NULL; altrimenti bind con cast ::tipo
pub async fn insert_rows(
    state: &AppState,
    schema: &str,
    table: &str,
    body: &serde_json::Value,
) -> ApiResult<Value> {
    let pool = &state.pool;
    let meta = registry::load_table_meta(state, schema, table).await?;

    // mappa tipi colonna
    let colset: HashSet<&str> = meta.columns.iter().map(|c| c.name.as_str()).collect();
    let mut coltypes: HashMap<&str, &str> = HashMap::with_capacity(meta.columns.len());
    for c in &meta.columns {
        coltypes.insert(c.name.as_str(), c.data_type.as_str());
    }

    // normalizza input in Vec<Map>
    let rows_in: Vec<&serde_json::Map<String, serde_json::Value>> = match body {
        serde_json::Value::Object(m) => vec![m],
        serde_json::Value::Array(v) => {
            if v.is_empty() {
                return Err(ApiError::BadRequest("empty array for bulk insert".into()));
            }
            let mut out = Vec::with_capacity(v.len());
            for it in v {
                let m = it.as_object().ok_or_else(|| {
                    ApiError::BadRequest("bulk insert expects array of JSON objects".into())
                })?;
                out.push(m);
            }
            out
        }
        _ => {
            return Err(ApiError::BadRequest(
                "request body must be a JSON object or array of objects".into(),
            ))
        }
    };

    // unione delle colonne
    let  all_cols: Vec<String> = {
        let mut set = HashSet::<String>::new();
        for m in &rows_in {
            for k in m.keys() {
                query::validate_ident(k)?;
                if !colset.contains(k.as_str()) {
                    return Err(ApiError::BadRequest(format!("unknown column '{}'", k)));
                }
                set.insert(k.to_string());
            }
        }
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort(); // ordine deterministico
        v
    };

    if all_cols.is_empty() {
        // nessuna colonna → INSERT DEFAULT VALUES
        let sql = format!(
            r#"
            WITH ins AS (
                INSERT INTO "{schema}"."{table}" DEFAULT VALUES
                RETURNING *
            )
            SELECT row_to_json(ins) AS data FROM ins
            "#,
            schema = meta.schema,
            table = meta.name
        );
        let rows = sqlx::query(&sql).fetch_all(pool).await.map_err(|e| {
            error!(error=%e, sql=%sql, "insert default values failed");
            ApiError::Internal("database error".into())
        })?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let v: serde_json::Value = r.get::<serde_json::Value, _>("data");
            out.push(v);
        }
        return Ok(serde_json::Value::Array(out));
    }

    // prepara cast per ogni colonna (FIX: niente deref su Result; usa ? nel closure)
   let mut casts: Vec<&'static str> = Vec::with_capacity(all_cols.len());
    for c in &all_cols {
        let dt = *coltypes
            .get(c.as_str())
            .ok_or_else(|| ApiError::BadRequest(format!("missing type for column '{}'", c)))?;
        // se pg_cast_of è in query:
        casts.push(query::pg_cast_of(dt));
    }

    // VALUES (...), (...), ...
    let mut binds: Vec<String> = Vec::new();
    let mut values_rows: Vec<String> = Vec::with_capacity(rows_in.len());

    for m in &rows_in {
        let mut tuple_parts: Vec<String> = Vec::with_capacity(all_cols.len());
        for (idx, col) in all_cols.iter().enumerate() {
            let cast = casts[idx];
            match m.get(col) {
                None => {
                    // colonna assente in questa riga → DEFAULT
                    tuple_parts.push("DEFAULT".to_string());
                }
                Some(v) if v.is_null() => {
                    // null esplicito → NULL
                    tuple_parts.push("NULL".to_string());
                }
                Some(v) => {
                    // bind con cast ::tipo
                    let s = if v.is_string() {
                        v.as_str().unwrap().to_string()
                    } else {
                        v.to_string()
                    };
                    binds.push(s);
                    tuple_parts.push(format!("${}::{}", binds.len(), cast));
                }
            }
        }
        values_rows.push(format!("({})", tuple_parts.join(", ")));
    }

    // lista colonne
    let cols_sql = all_cols
        .iter()
        .map(|c| format!(r#""{}""#, c))
        .collect::<Vec<_>>()
        .join(", ");

    let sql = format!(
        r#"
        WITH ins AS (
            INSERT INTO "{schema}"."{table}" ({cols})
            VALUES {values}
            RETURNING *
        )
        SELECT row_to_json(ins) AS data FROM ins
        "#,
        schema = meta.schema,
        table = meta.name,
        cols = cols_sql,
        values = values_rows.join(", ")
    );

    let mut q = sqlx::query(&sql);
    for b in &binds {
        q = q.bind(b.as_str());
    }

    // FIX: niente `?` dopo il match; i rami Err fanno `return Err(...)`
    let rows = match q.fetch_all(pool).await {
        Ok(rows) => rows,
        Err(e) => {
            use sqlx::Error;
            if let Error::Database(db_err) = &e {
                let code_cow = db_err.code();
                let code = code_cow.as_deref().unwrap_or("");
                let msg = db_err.message();
                match code {
                    // violazioni format/cast
                    "22P02" | "22007" | "42804" | "42846" => {
                        return Err(ApiError::BadRequest(msg.to_string()))
                    }
                    // vincoli (unique, not null, fk, check)
                    "23505" | "23502" | "23503" | "23514" => {
                        return Err(ApiError::BadRequest(msg.to_string()))
                    }
                    "42P01" => {
                        return Err(ApiError::NotFound(format!(r#"table "{}"."{}" not found"#, schema, table)))
                    }
                    _ => {
                        error!(%code, db_message=%msg, sql=%sql, ?binds, "unhandled database error (insert)");
                        return Err(ApiError::Internal("database error".into()));
                    }
                }
            }
            error!(error=%e, sql=%sql, ?binds, "non-database error (insert)");
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