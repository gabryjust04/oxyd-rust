// oxyd-core/src/virtual_crud/insert.rs

use std::collections::{HashMap, HashSet};

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

/// Perform a single or bulk INSERT and return the inserted rows as a JSON array.
///
/// Behaviour:
/// - `body = { ... }`              → single insert
/// - `body = [ {...}, {...} ]`     → bulk insert
///
/// Column handling:
/// - The final column list is the union of all keys from all input objects.
/// - For columns missing in a given row → `DEFAULT`.
/// - `null` values in JSON → `NULL`.
/// - Non-null values are bound as text and cast to the proper SQL type (`::tipo`).
///
/// Multi-tenant / RLS safety:
/// - If `user` is present:
///   - `app.current_user_id` is set via `set_config(..., true)` inside the transaction.
///   - If the table has a `user_id` column:
///       - The backend ALWAYS enforces `user_id = user.id`.
///       - If the client tries to send a different value for `user_id` → 400.
pub async fn insert_rows(
    state: &AppState,
    schema: &str,
    table: &str,
    body: &Value,
    user: Option<CurrentUser>,
) -> ApiResult<Value> {
    // Basic log to understand whether we are inserting with or without RLS context.
    if let Some(ref cu) = user {
        println!("insert_rows called with authenticated user id={}", cu.id);
    } else {
        println!("insert_rows called without authenticated user");
    }

    // Load full table metadata from the registry (columns, types, PK, ...).
    let meta = registry::load_table_meta(state, schema, table).await?;

    // Build a set of allowed column names, and a type index: "col" -> "data_type".
    let colset: HashSet<&str> = meta.columns.iter().map(|c| c.name.as_str()).collect();
    let mut coltypes: HashMap<&str, &str> = HashMap::with_capacity(meta.columns.len());
    for c in &meta.columns {
        coltypes.insert(c.name.as_str(), c.data_type.as_str());
    }

    // A "user-owned" table is assumed to have a `user_id` column.
    let has_user_id = colset.contains("user_id");
    let user_is_authenticated = user.is_some();

    // ── Normalize input into a Vec<Map> for uniform bulk handling ─────────────
    //
    // - If the body is a single JSON object, we treat it as a single-row bulk insert.
    // - If the body is an array, we ensure every element is an object.
    // - Any other JSON type is rejected.
    let rows_in: Vec<&serde_json::Map<String, Value>> = match body {
        Value::Object(m) => vec![m],
        Value::Array(v) => {
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

    // ── Compute the union of all columns present in the input ─────────────────
    //
    // - Validate each key as a safe identifier.
    // - Reject unknown columns early with a 400.
    // - If the table has `user_id` and the user is authenticated, ensure
    //   `user_id` is part of the union so we can force it.
    let all_cols: Vec<String> = {
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

        // Convention: if the table has `user_id` and the caller is authenticated,
        // the backend ALWAYS manages this column.
        if has_user_id && user_is_authenticated {
            set.insert("user_id".to_string());
        }

        // Sort for deterministic column ordering in the generated SQL.
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort();
        v
    };

    // ── Build SQL + binds (no execution yet) ──────────────────────────────────
    let (sql, binds): (String, Vec<String>) = if all_cols.is_empty() {
        // No columns → `INSERT DEFAULT VALUES`.
        //
        // Notes:
        // - If `user_id` is NOT NULL and has no DEFAULT, this will fail at the DB level.
        //   You can define a DEFAULT in Postgres if you want to support this pattern.
        let sql = format!(
            r#"
            WITH ins AS (
                INSERT INTO "{schema}"."{table}" DEFAULT VALUES
                RETURNING *
            )
            SELECT row_to_json(ins) AS data
            FROM ins
            "#,
            schema = meta.schema,
            table = meta.name
        );
        (sql, Vec::new())
    } else {
        // Prepare casts for each column once (based on information_schema types).
        let mut casts: Vec<&'static str> = Vec::with_capacity(all_cols.len());
        for c in &all_cols {
            let dt = *coltypes
                .get(c.as_str())
                .ok_or_else(|| ApiError::BadRequest(format!("missing type for column '{}'", c)))?;
            casts.push(query::pg_cast_of(dt));
        }

        // We build:
        //   VALUES ($1::type, $2::type, ...),
        //          ($N::type, $N+1::type, ...),
        //   ...
        let mut binds: Vec<String> = Vec::with_capacity(rows_in.len() * all_cols.len());
        let mut values_rows: Vec<String> = Vec::with_capacity(rows_in.len());

        for m in &rows_in {
            let mut tuple_parts: Vec<String> = Vec::with_capacity(all_cols.len());

            for (idx, col) in all_cols.iter().enumerate() {
                let cast = casts[idx];

                // Special handling for `user_id` on user-owned tables:
                //
                // - If table has `user_id` and an authenticated user is present:
                //   - If the client provides a `user_id` that is not null and
                //     does not match the current user id → 400.
                //   - Regardless of the input, we ALWAYS insert `cu.id`.
                if has_user_id && user_is_authenticated && col == "user_id" {
                    let cu = user.as_ref().unwrap();

                    if let Some(v) = m.get(col) {
                        // Client tried to specify `user_id` explicitly.
                        if !v.is_null() {
                            let client_val = if v.is_string() {
                                v.as_str().unwrap().to_string()
                            } else {
                                v.to_string()
                            };
                            let expected = cu.id.to_string();

                            if client_val != expected {
                                return Err(ApiError::BadRequest(
                                    "cannot override user_id; it is bound to the authenticated user"
                                        .into(),
                                ));
                            }
                        }
                    }

                    // Force `user_id` to the authenticated user's id.
                    binds.push(cu.id.to_string());
                    tuple_parts.push(format!("${}::{}", binds.len(), cast));
                    continue;
                }

                // Standard handling for all other columns.
                match m.get(col) {
                    None => {
                        // Column absent in this row → `DEFAULT`.
                        tuple_parts.push("DEFAULT".to_string());
                    }
                    Some(v) if v.is_null() => {
                        // Explicit `null` → `NULL`.
                        tuple_parts.push("NULL".to_string());
                    }
                    Some(v) => {
                        // Bind non-null value and cast it to the right type.
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

        // Build the column list: `"col1", "col2", ...`.
        let cols_sql = all_cols
            .iter()
            .map(|c| format!(r#""{}""#, c))
            .collect::<Vec<_>>()
            .join(", ");

        // Final SQL layout:
        //
        // WITH ins AS (
        //   INSERT INTO "schema"."table" ("col1", "col2", ...)
        //   VALUES (...), (...)
        //   RETURNING *
        // )
        // SELECT row_to_json(ins) AS data FROM ins;
        let sql = format!(
            r#"
            WITH ins AS (
                INSERT INTO "{schema}"."{table}" ({cols})
                VALUES {values}
                RETURNING *
            )
            SELECT row_to_json(ins) AS data
            FROM ins
            "#,
            schema = meta.schema,
            table = meta.name,
            cols = cols_sql,
            values = values_rows.join(", ")
        );

        (sql, binds)
    };

    // ── RLS: transaction + set_config for user context ────────────────────────
    let mut tx = state.pool.begin().await?;

    if let Some(ref cu) = user {
        // `set_config(..., true)` behaves like `SET LOCAL` and supports bind parameters.
        sqlx::query("SELECT set_config('app.current_user_id', $1, true)")
            .bind(cu.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                error!(error=%e, "failed to set RLS context via set_config (insert)");
                ApiError::Internal("failed to set RLS context".into())
            })?;
    }

    // Bind all values collected above and execute the INSERT + RETURNING query.
    let mut q = sqlx::query(&sql);
    for b in &binds {
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
                    // Format/cast violations, invalid input syntax, etc.
                    "22P02" | "22007" | "42804" | "42846" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::BadRequest(msg.to_string()));
                    }
                    // Constraints (unique, not null, FK, check).
                    "23505" | "23502" | "23503" | "23514" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::BadRequest(msg.to_string()));
                    }
                    // Table not found.
                    "42P01" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::NotFound(format!(
                            r#"table "{}"."{}" not found"#,
                            schema, table
                        )));
                    }
                    // Any other database error: log details and return generic 500.
                    _ => {
                        error!(
                            %code,
                            db_message = %msg,
                            sql = %sql,
                            ?binds,
                            "unhandled database error (insert)"
                        );
                        tx.rollback().await.ok();
                        println!(
                            "Unhandled database error (insert): code={}, message={}",
                            code, msg
                        );
                        return Err(ApiError::Internal("database error".into()));
                    }
                }
            }

            // Non-database errors (pool issues, I/O, etc.).
            error!(error=%e, sql=%sql, ?binds, "non-database error (insert)");
            tx.rollback().await.ok();
            return Err(ApiError::Internal("internal error".into()));
        }
    };

    // If we got here, everything went fine; commit the transaction.
    tx.commit().await?;

    // Extract the `data` field (`row_to_json` result) from each row and
    // return them as a JSON array.
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let v: Value = r.get::<Value, _>("data");
        out.push(v);
    }

    Ok(Value::Array(out))
}
