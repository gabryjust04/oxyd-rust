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

/// Esegue INSERT (singolo o bulk) e ritorna le righe inserite come array JSON.
///
/// - body = { ... }              → singolo insert
/// - body = [ {...}, {...} ]     → bulk insert
/// - colonne = unione delle chiavi; per chiavi mancanti in una riga → DEFAULT
/// - valori null → NULL; altrimenti bind con cast ::tipo
///
/// Sicurezza multi-tenant:
/// - se user è presente:
///   - setta app.current_user_id via set_config(..., true) all'interno della transazione
///   - se la tabella ha colonna `user_id`, il backend la forza SEMPRE a `user.id`
///     ignorando ciò che arriva dal client (e se prova a metterla diversa → 400).
pub async fn insert_rows(
    state: &AppState,
    schema: &str,
    table: &str,
    body: &Value,
    user: Option<CurrentUser>,
) -> ApiResult<Value> {
    if let Some(ref cu) = user {
        println!("insert_rows called with authenticated user id={}", cu.id);
    } else {
        println!("insert_rows called without authenticated user");
    }

    let meta = registry::load_table_meta(state, schema, table).await?;

    // mappa tipi colonna
    let colset: HashSet<&str> = meta.columns.iter().map(|c| c.name.as_str()).collect();
    let mut coltypes: HashMap<&str, &str> = HashMap::with_capacity(meta.columns.len());
    for c in &meta.columns {
        coltypes.insert(c.name.as_str(), c.data_type.as_str());
    }

    // se la tabella è "user-owned" avrà una colonna `user_id`
    let has_user_id = colset.contains("user_id");
    let user_is_authenticated = user.is_some();

    // normalizza input in Vec<Map>
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

    // unione delle colonne presenti nell'input
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

        // Convenzione: se la tabella ha `user_id` e c'è un utente autenticato,
        // lo gestisce SEMPRE il backend.
        if has_user_id && user_is_authenticated {
            set.insert("user_id".to_string());
        }

        let mut v: Vec<String> = set.into_iter().collect();
        v.sort(); // ordine deterministico
        v
    };

    // Costruzione SQL + binds (senza ancora eseguire nulla)
    let (sql, binds): (String, Vec<String>) = if all_cols.is_empty() {
        // nessuna colonna → INSERT DEFAULT VALUES
        //
        // Nota:
        // - se la tabella ha user_id NOT NULL senza DEFAULT, questo fallirà lato DB
        //   (puoi aggiungere un DEFAULT a livello Postgres se vuoi supportare
        //   anche questo caso).
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
        (sql, Vec::new())
    } else {
        // prepara cast per ogni colonna
        let mut casts: Vec<&'static str> = Vec::with_capacity(all_cols.len());
        for c in &all_cols {
            let dt = *coltypes
                .get(c.as_str())
                .ok_or_else(|| ApiError::BadRequest(format!("missing type for column '{}'", c)))?;
            casts.push(query::pg_cast_of(dt));
        }

        // VALUES (...), (...), ...
        let mut binds: Vec<String> = Vec::with_capacity(rows_in.len() * all_cols.len());
        let mut values_rows: Vec<String> = Vec::with_capacity(rows_in.len());

        for m in &rows_in {
            let mut tuple_parts: Vec<String> = Vec::with_capacity(all_cols.len());

            for (idx, col) in all_cols.iter().enumerate() {
                let cast = casts[idx];

                // Gestione speciale per `user_id`:
                // - se la tabella ha user_id e c'è un CurrentUser:
                //   - se il client lo passa e non coincide con cu.id → 400
                //   - in ogni caso, in INSERT usiamo SEMPRE cu.id
                if has_user_id && user_is_authenticated && col == "user_id" {
                    let cu = user.as_ref().unwrap();

                    if let Some(v) = m.get(col) {
                        // se il client prova a mettere user_id esplicito diverso dal proprio → errore chiaro
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

                    // forziamo SEMPRE user_id = cu.id
                    binds.push(cu.id.to_string());
                    tuple_parts.push(format!("${}::{}", binds.len(), cast));
                    continue;
                }

                // comportamento standard per tutte le altre colonne
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

        (sql, binds)
    };

    // ── RLS: transazione + set_config del contesto utente ───────────────────────
    let mut tx = state.pool.begin().await?;

    if let Some(ref cu) = user {
        // set_config(..., true) equivale a SET LOCAL e supporta i bind
        sqlx::query("SELECT set_config('app.current_user_id', $1, true)")
            .bind(cu.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                error!(error=%e, "failed to set RLS context via set_config (insert)");
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
                    // violazioni formato/cast
                    "22P02" | "22007" | "42804" | "42846" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::BadRequest(msg.to_string()));
                    }
                    // vincoli (unique, not null, fk, check)
                    "23505" | "23502" | "23503" | "23514" => {
                        tx.rollback().await.ok();
                        return Err(ApiError::BadRequest(msg.to_string()));
                    }
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
            error!(error=%e, sql=%sql, ?binds, "non-database error (insert)");
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
