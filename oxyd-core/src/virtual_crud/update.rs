// oxyd-core/src/virtual_crud/update.rs

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use sqlx::Row;
use tracing::error;

use crate::auth::types::CurrentUser;
use crate::general::{
    errors::{ApiError, ApiResult},
    types::AppState,
};
use crate::virtual_crud::{query, registry};

use super::types::*;

/// Esegue UPDATE e ritorna le righe aggiornate come array JSON.
///
/// - Richiede almeno un filtro (evita mass update).
/// - Non aggiorna colonne di PK.
/// - I valori vengono bindati come &str con cast ::tipo.
/// - Sicurezza multi-tenant:
///   - se la tabella ha `user_id` e c'è un utente autenticato:
///     - il client NON può aggiornare `user_id` (400 se ci prova)
///   - RLS viene applicato tramite set_config('app.current_user_id', ...) all'interno
///     di una transazione, in modo che le policy Postgres decidano quali righe
///     l'utente può effettivamente aggiornare.
pub async fn update_rows(
    state: &AppState,
    schema: &str,
    table: &str,
    body: &Value,
    opts: &QueryOptions,
    user: Option<CurrentUser>,
) -> ApiResult<Value> {
    if let Some(ref cu) = user {
        println!("update_rows called with authenticated user id={}", cu.id);
    } else {
        println!("update_rows called without authenticated user");
    }

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

    let has_user_id = colset.contains("user_id");
    let user_is_authenticated = user.is_some();

    // Costruisci SET
    let mut set_parts: Vec<String> = Vec::with_capacity(obj.len());
    let mut binds: Vec<String> = Vec::new();

    for (k, v) in obj {
        query::validate_ident(k)?;
        if !colset.contains(k.as_str()) {
            return Err(ApiError::BadRequest(format!("unknown column '{}'", k)));
        }
        if pkset.contains(k.as_str()) {
            return Err(ApiError::BadRequest(format!(
                "cannot update primary key column '{}'",
                k
            )));
        }

        // Convenzione: se la tabella ha user_id e l'utente è autenticato,
        // il client NON può aggiornare user_id.
        if has_user_id && user_is_authenticated && k == "user_id" {
            return Err(ApiError::BadRequest(
                "cannot update user_id; it is bound to the authenticated user".into(),
            ));
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

    // ── RLS: transazione + set_config del contesto utente ───────────────────────
    let mut tx = state.pool.begin().await?;

    if let Some(ref cu) = user {
        sqlx::query("SELECT set_config('app.current_user_id', $1, true)")
            .bind(cu.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                error!(error=%e, "failed to set RLS context via set_config (update)");
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
                let code_cow = db_err.code();
                let code = code_cow.as_deref().unwrap_or("");
                let msg = db_err.message();

                match code {
                    // violazioni formato/cast, operatori, ecc.
                    "22P02" | "22007" | "42804" | "42846" | "42883" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::BadRequest(msg.to_string()));
                    }
                    // vincoli (unique, not null, fk, check)
                    "23505" | "23502" | "23503" | "23514" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::BadRequest(msg.to_string()));
                    }
                    // colonna/espressione inesistente
                    "42703" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::BadRequest(format!(
                            "unknown column/expression: {}",
                            msg
                        )));
                    }
                    // tabella non trovata
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
                            "unhandled database error (update)"
                        );
                        tx.rollback().await.ok();
                        println!(
                            "Unhandled database error (update): code={}, message={}",
                            code, msg
                        );
                        return Err(ApiError::Internal("database error".into()));
                    }
                }
            }

            error!(error=%e, sql=%sql, ?binds, "non-database error (update)");
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
