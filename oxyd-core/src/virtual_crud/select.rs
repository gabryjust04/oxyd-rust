// oxyd-core/src/virtual_crud/select.rs

use std::collections::{ HashSet};



use crate::{auth::{extractors::OptionalUser, types::CurrentUser}, general::{errors::{ApiError, ApiResult}, types::AppState}, virtual_crud::registry};
use super::types::*;
use crate::virtual_crud::{query};
use serde_json::{ Value};
use sqlx::{ Row};
use tracing::error;


fn build_select_sql(meta: &TableMeta, opts: &QueryOptions) -> ApiResult<(String, Vec<String>)> {
    // set colonne consentite
    let colset: HashSet<&str> = meta.columns.iter().map(|c| c.name.as_str()).collect();

    // SELECT
    let select_cols = if let Some(cols) = &opts.select {
        if cols.is_empty() {
            "*".to_string()
        } else {
            let mut acc = Vec::with_capacity(cols.len());
            for c in cols {
                query::validate_ident(c)?;
                if !colset.contains(c.as_str()) {
                    return Err(ApiError::BadRequest(format!("unknown column '{}'", c)));
                }
                acc.push(format!(r#""{}""#, c));
            }
            acc.join(", ")
        }
    } else {
        "*".to_string()
    };

    // WHERE tramite helper (riempie binds)
    let mut binds: Vec<String> = Vec::new();
    let where_sql = query::build_where_sql(meta, opts, &mut binds)?;

    // ORDER BY
    let order_sql = if let Some((col, desc)) = &opts.order_by {
        query::validate_ident(col)?;
        if !colset.contains(col.as_str()) {
            return Err(ApiError::BadRequest(format!("unknown column '{}'", col)));
        }
        format!(r#" ORDER BY "{}" {}"#, col, if *desc { "DESC" } else { "ASC" })
    } else {
        String::new()
    };

    // LIMIT/OFFSET
    let limit_sql = opts.limit.map(|_| " LIMIT $LIM".to_string()).unwrap_or_default();
    let offset_sql = opts.offset.map(|_| " OFFSET $OFF".to_string()).unwrap_or_default();

    // SQL finale
    let mut sql = format!(
        r#"
        SELECT row_to_json(t) AS data
        FROM (
            SELECT {select_cols}
            FROM "{schema}"."{table}"
            {where}{order}{limit}{offset}
        ) t
        "#,
        select_cols = select_cols,
        schema = meta.schema,
        table = meta.name,
        where = where_sql,
        order = order_sql,
        limit = limit_sql,
        offset = offset_sql
    );

    // Rimpiazza sentinelle lim/off con placeholder numerati
    let mut extra_binds: Vec<i64> = Vec::new();
    if let Some(l) = opts.limit {
        sql = sql.replace("$LIM", &format!("${}", binds.len() + 1));
        extra_binds.push(l);
    }
    if let Some(o) = opts.offset {
        sql = sql.replace("$OFF", &format!("${}", binds.len() + extra_binds.len() + 1));
        extra_binds.push(o);
    }

    for v in extra_binds {
        binds.push(v.to_string());
    }

    Ok((sql, binds))
}


pub async fn select_rows(
    state: &AppState,
    schema: &str,
    table: &str,
    opts: &QueryOptions,
    user: Option<CurrentUser>,
) -> ApiResult<Value> {
    
    if let Some(ref cu) = user {
        println!("select_rows called with authenticated user id={}", cu.id);
    } else {
        println!("select_rows called without authenticated user");
    }

    let pool = &state.pool;
    let meta = registry::load_table_meta(state, schema, table).await?;

    let (sql, binds) = build_select_sql(&meta, opts)?;

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
                        error!(%code, db_message=%msg, sql=%sql, ?binds, "unhandled database error");
                        return Err(ApiError::Internal("database error".into()));
                    }
                }
            }
            error!(error=%e, sql=%sql, ?binds, "non-database error");
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
