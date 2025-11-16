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

/// Esegue DELETE e ritorna le righe eliminate come array JSON.
///
/// - Richiede almeno un filtro (evita mass delete).
/// - Usa le policy RLS definite sul DB:
///   - se `user` è presente:
///       - apre una transazione
///       - esegue `set_config('app.current_user_id', user.id, true)`
///       - esegue la DELETE all'interno della stessa transazione
///   - le policy RLS decidono quali righe sono effettivamente cancellabili.
pub async fn delete_rows(
    state: &AppState,
    schema: &str,
    table: &str,
    opts: &QueryOptions,
    user: Option<CurrentUser>,
) -> ApiResult<Value> {
    let meta = registry::load_table_meta(state, schema, table).await?;

    if opts.filters.is_empty() {
        return Err(ApiError::BadRequest(
            "refuse DELETE without filters".into(),
        ));
    }

    if let Some(ref cu) = user {
        println!(
            r#"delete_rows called with authenticated user id={}"#,
            cu.id
        );
    } else {
        println!("delete_rows called without authenticated user");
    }

    // Costruzione WHERE + bind
    let mut binds: Vec<String> = Vec::new();
    let where_sql = query::build_where_sql(&meta, opts, &mut binds)?;

    let sql = format!(
        r#"
        WITH deleted AS (
            DELETE FROM "{schema}"."{table}"
            {where_sql}
            RETURNING *
        )
        SELECT row_to_json(deleted) AS data FROM deleted
        "#,
        schema = meta.schema,
        table = meta.name,
        where_sql = where_sql,
    );

    // ── RLS: transazione + set_config del contesto utente ───────────────────────
    let mut tx = state.pool.begin().await?;

    if let Some(ref cu) = user {
        // set_config(..., true) = "SET LOCAL", limitato alla transazione
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

    let mut q = sqlx::query(&sql);
    for b in &binds {
        q = q.bind(b.as_str());
    }

    let rows = match q.fetch_all(&mut *tx).await {
        Ok(rows) => rows,
        Err(e) => {
            use sqlx::Error;

            if let Error::Database(db_err) = &e {
                let code_opt = db_err.code();
                let code = code_opt.as_deref().unwrap_or("");
                let msg = db_err.message();

                match code {
                    // errori di tipo/cast/funzione
                    "22P02" | "22007" | "42804" | "42846" | "42883" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::BadRequest(msg.to_string()));
                    }
                    // colonna o espressione sconosciuta
                    "42703" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::BadRequest(format!(
                            "unknown column/expression: {}",
                            msg
                        )));
                    }
                    // tabella non esiste
                    "42P01" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::NotFound(format!(
                            r#"table "{}"."{}" not found"#,
                            schema, table
                        )));
                    }
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

            error!(error = %e, sql=%sql, ?binds, "non-database error (delete)");
            tx.rollback().await.ok();
            return Err(ApiError::Internal("internal error".into()));
        }
    };

    tx.commit().await?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let v: Value = r.get::<Value, _>("data");
        out.push(v);
    }

    Ok(Value::Array(out))
}
