// oxyd-core/src/virtual_crud/delete.rs

use serde_json::Value;
use sqlx::Row;
use tracing::error;

use crate::{
    auth::types::CurrentUser,
    general::{
        errors::{ApiError, ApiResult},
        types::AppState,
    },
    virtual_crud::{query, registry},
};

use super::types::*;

/// Execute a filtered DELETE and return the deleted rows as a JSON array.
///
/// Behaviour:
/// - Requires at least one filter in `QueryOptions`:
///     - this explicitly disallows "DELETE everything" without a WHERE clause.
/// - Relies on database-side RLS policies:
///   - If `user` is present:
///       - opens a transaction,
///       - runs `set_config('app.current_user_id', user.id, true)` inside it,
///       - performs the DELETE within the same transaction.
///   - RLS policies decide which rows are actually deletable for that user.
/// - Deleted rows are returned as `row_to_json` objects in a JSON array
///   (PostgREST-style response).
pub async fn delete_rows(
    state: &AppState,
    schema: &str,
    table: &str,
    opts: &QueryOptions,
    user: Option<CurrentUser>,
) -> ApiResult<Value> {
    // Load table metadata (columns, types, etc.).
    let meta = registry::load_table_meta(state, schema, table).await?;

    // Safety guard: never allow a DELETE without filters.
    if opts.filters.is_empty() {
        return Err(ApiError::BadRequest(
            "refuse DELETE without filters".into(),
        ));
    }

    // Small debug log to see if RLS context will be set.
    if let Some(ref cu) = user {
        println!(
            r#"delete_rows called with authenticated user id={}"#,
            cu.id
        );
    } else {
        println!("delete_rows called without authenticated user");
    }

    // ── Build WHERE clause + bind values ──────────────────────────────────────
    //
    // `build_where_sql` validates columns, types, and operators and pushes
    // all filter values into `binds` in the proper order.
    let mut binds: Vec<String> = Vec::new();
    let where_sql = query::build_where_sql(&meta, opts, &mut binds)?;

    // Final SQL shape:
    //
    // WITH deleted AS (
    //   DELETE FROM "schema"."table"
    //   WHERE ...
    //   RETURNING *
    // )
    // SELECT row_to_json(deleted) AS data FROM deleted;
    let sql = format!(
        r#"
        WITH deleted AS (
            DELETE FROM "{schema}"."{table}"
            {where_sql}
            RETURNING *
        )
        SELECT row_to_json(deleted) AS data
        FROM deleted
        "#,
        schema = meta.schema,
        table = meta.name,
        where_sql = where_sql,
    );

    // ── RLS: transaction + set_config for user context ────────────────────────
    let mut tx = state.pool.begin().await?;

    if let Some(ref cu) = user {
        // `set_config(..., true)` behaves like `SET LOCAL`:
        // - it is scoped to the current transaction only,
        // - it supports bind parameters.
        sqlx::query("SELECT set_config('app.current_user_id', $1, true)")
            .bind(cu.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                error!(
                    error = %e,
                    "failed to set RLS context via set_config (delete)"
                );
                ApiError::Internal("failed to set RLS context".into())
            })?;
    }

    // Bind all filter values into the DELETE query.
    let mut q = sqlx::query(&sql);
    for b in &binds {
        q = q.bind(b.as_str());
    }

    // ── Execute DELETE and map errors to API-level responses ──────────────────
    let rows = match q.fetch_all(&mut *tx).await {
        Ok(rows) => rows,
        Err(e) => {
            use sqlx::Error;

            if let Error::Database(db_err) = &e {
                let code_opt = db_err.code();
                let code = code_opt.as_deref().unwrap_or("");
                let msg = db_err.message();

                match code {
                    // Type / cast / function errors (bad input, type mismatch, etc.).
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
                    // Target table not found.
                    "42P01" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::NotFound(format!(
                            r#"table "{}"."{}" not found"#,
                            schema, table
                        )));
                    }
                    // Any other database error: log and return a generic 500.
                    _ => {
                        error!(
                            %code,
                            db_message = %msg,
                            sql = %sql,
                            ?binds,
                            "unhandled database error (delete)"
                        );
                        tx.rollback().await.ok();
                        return Err(ApiError::Internal("database error".into()));
                    }
                }
            }

            // Non-database errors (pool issues, IO, etc.).
            error!(error = %e, sql=%sql, ?binds, "non-database error (delete)");
            tx.rollback().await.ok();
            return Err(ApiError::Internal("internal error".into()));
        }
    };

    // If we reach this point, the DELETE succeeded. Commit the transaction.
    tx.commit().await?;

    // Extract the `data` field (`row_to_json` result) for each deleted row and
    // return them as a JSON array.
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let v: Value = r.get::<Value, _>("data");
        out.push(v);
    }

    Ok(Value::Array(out))
}
