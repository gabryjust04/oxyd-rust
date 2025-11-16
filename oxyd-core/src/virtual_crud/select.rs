// oxyd-core/src/virtual_crud/select.rs

use std::collections::HashSet;

use crate::{
    auth::types::CurrentUser,
    general::{
        errors::{ApiError, ApiResult},
        types::AppState,
    },
    virtual_crud::registry,
};
use super::types::*;
use crate::virtual_crud::query;
use serde_json::Value;
use sqlx::Row;
use tracing::error;

/// Build the dynamic SELECT statement for a given table and query options.
///
/// Returns:
/// - The SQL string with numbered placeholders (`$1`, `$2`, ...).
/// - A vector of **stringified** bind values (in the right order).
fn build_select_sql(meta: &TableMeta, opts: &QueryOptions) -> ApiResult<(String, Vec<String>)> {
    // Build a set with all allowed column names for fast membership checks.
    let colset: HashSet<&str> = meta.columns.iter().map(|c| c.name.as_str()).collect();

    // ── SELECT clause ──────────────────────────────────────────────────────────
    let select_cols = if let Some(cols) = &opts.select {
        if cols.is_empty() {
            // Explicit `select=` with no fields → keep PostgREST-like semantics: `*`.
            "*".to_string()
        } else {
            let mut acc = Vec::with_capacity(cols.len());

            for c in cols {
                // Make sure the identifier is syntactically safe (no SQL injection).
                query::validate_ident(c)?;

                // Reject unknown columns early with a clear 400 error.
                if !colset.contains(c.as_str()) {
                    return Err(ApiError::BadRequest(format!("unknown column '{}'", c)));
                }

                // Quote the identifier to be safe with reserved words / mixed case.
                acc.push(format!(r#""{}""#, c));
            }

            acc.join(", ")
        }
    } else {
        // No explicit `select` → default to `*`.
        "*".to_string()
    };

    // ── WHERE clause via helper (this also fills `binds`) ─────────────────────
    let mut binds: Vec<String> = Vec::new();
    let where_sql = query::build_where_sql(meta, opts, &mut binds)?;

    // ── ORDER BY clause ────────────────────────────────────────────────────────
    let order_sql = if let Some((col, desc)) = &opts.order_by {
        query::validate_ident(col)?;

        if !colset.contains(col.as_str()) {
            return Err(ApiError::BadRequest(format!("unknown column '{}'", col)));
        }

        let direction = if *desc { "DESC" } else { "ASC" };
        format!(r#" ORDER BY "{}" {}"#, col, direction)
    } else {
        String::new()
    };

    // ── LIMIT / OFFSET placeholders ────────────────────────────────────────────
    //
    // We first put fake markers `$LIM` / `$OFF` into the SQL and later replace
    // them with the proper `$N` placeholders once we know how many binds
    // `WHERE` already used.
    let limit_sql = opts
        .limit
        .map(|_| " LIMIT $LIM".to_string())
        .unwrap_or_default();

    let offset_sql = opts
        .offset
        .map(|_| " OFFSET $OFF".to_string())
        .unwrap_or_default();

    // ── Final SQL layout ──────────────────────────────────────────────────────
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
        offset = offset_sql,
    );

    // ── Replace LIMIT/OFFSET markers with numbered placeholders ───────────────
    //
    // We append LIMIT/OFFSET values **after** the WHERE parameters, so the
    // indices keep increasing: `$1..$n` for WHERE, then `$n+1` for LIMIT,
    // `$n+2` for OFFSET, in that order.
    let mut extra_binds: Vec<i64> = Vec::new();

    if let Some(l) = opts.limit {
        sql = sql.replace("$LIM", &format!("${}", binds.len() + 1));
        extra_binds.push(l);
    }

    if let Some(o) = opts.offset {
        sql = sql.replace("$OFF", &format!("${}", binds.len() + extra_binds.len() + 1));
        extra_binds.push(o);
    }

    // Convert LIMIT / OFFSET to strings and append to the bind list.
    for v in extra_binds {
        binds.push(v.to_string());
    }

    Ok((sql, binds))
}

/// Execute a dynamic SELECT on a registered table, with optional RLS context.
///
/// Behaviour:
/// - Loads `TableMeta` from the registry (schema + table).
/// - Builds the dynamic SQL and bind list via [`build_select_sql`].
/// - Starts a transaction and, if a user is present, sets the RLS context
///   using `set_config('app.current_user_id', ...)`.
/// - Executes the query, mapping known Postgres errors to API-friendly responses.
/// - Returns the resulting rows as a JSON array of `row_to_json(t)` objects.
pub async fn select_rows(
    state: &AppState,
    schema: &str,
    table: &str,
    opts: &QueryOptions,
    user: Option<CurrentUser>,
) -> ApiResult<Value> {
    // Basic debug logging: useful to verify RLS is getting a user id.
    if let Some(ref cu) = user {
        println!("select_rows called with authenticated user id={}", cu.id);
    } else {
        println!("select_rows called without authenticated user");
    }

    // Load dynamic table metadata from the registry (columns, PKs, etc.).
    let meta = registry::load_table_meta(state, schema, table).await?;

    // Generate SQL and bind values according to the incoming QueryOptions.
    let (sql, binds) = build_select_sql(&meta, opts)?;

    // ── RLS: run inside a transaction + SET LOCAL user context ────────────────
    let mut tx = state.pool.begin().await?;

    if let Some(ref cu) = user {
        // Set a session-local variable that RLS policies can read via
        // `current_setting('app.current_user_id', true)`.
        //
        // NOTE: we always bind it as text; policies are free to cast
        // (e.g. `::int`, `::uuid`, etc.).
        sqlx::query("SELECT set_config('app.current_user_id', $1, true)")
            .bind(cu.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                error!(error=%e, "failed to set RLS context via set_config");
                ApiError::Internal("failed to set RLS context".into())
            })?;
        // No `else`: for anonymous users we simply do not set any context.
        // `SET LOCAL` automatically goes away at transaction end.
    }

    // ── Bind all parameters in order and execute the query ────────────────────
    let mut q = sqlx::query(&sql);
    for b in &binds {
        // We bound everything as text; Postgres will cast when needed.
        q = q.bind(b.as_str());
    }

    let rows = match q.fetch_all(&mut *tx).await {
        Ok(rows) => rows,
        Err(e) => {
            use sqlx::Error;

            if let Error::Database(db_err) = &e {
                let code_cow = db_err.code();
                let code = code_cow.as_deref().unwrap_or("");
                let msg = db_err.message();

                match code {
                    // Cast / parse errors, operator mismatch, invalid input syntax, etc.
                    "22P02" | "22007" | "42804" | "42846" | "42883" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::BadRequest(msg.to_string()));
                    }
                    // Unknown column or expression in the query.
                    "42703" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::BadRequest(format!(
                            "unknown column/expression: {}",
                            msg
                        )));
                    }
                    // Table not found (probably schema drift vs registry).
                    "42P01" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::NotFound(format!(
                            r#"table "{}"."{}" not found"#,
                            schema, table
                        )));
                    }
                    // Any other database error → generic 500 + structured log.
                    _ => {
                        error!(%code, db_message=%msg, sql=%sql, ?binds, "unhandled database error");
                        tx.rollback().await.ok();
                        return Err(ApiError::Internal("database error".into()));
                    }
                }
            }

            // Non-database errors (e.g. pool issues, IO errors, etc.).
            error!(error=%e, sql=%sql, ?binds, "non-database error");
            tx.rollback().await.ok();
            return Err(ApiError::Internal("internal error".into()));
        }
    };

    // If we got here the query was successful; commit the transaction.
    tx.commit().await?;

    // ── Extract the `data` field (row_to_json result) from each row ───────────
    let mut out = Vec::with_capacity(rows.len());

    for r in rows {
        let v: serde_json::Value = r.get::<serde_json::Value, _>("data");
        out.push(v);
    }

    Ok(serde_json::Value::Array(out))
}
