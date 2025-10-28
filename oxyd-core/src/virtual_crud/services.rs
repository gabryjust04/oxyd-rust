use std::collections::{HashMap, HashSet};

use axum::http::StatusCode;
use regex::Regex;
use serde_json::{json, Value};
use sqlx::{postgres::PgRow, PgPool, Row};
use std::sync::LazyLock;
use tracing::error;

use crate::general::errors::{ApiError, ApiResult};
use super::types::*;
use crate::general::types::AppState;

/// Controlla se esiste la tabella di registry `_oxyd_tables`.
async fn oxyd_tables_exists(state: &AppState) -> Result<bool, sqlx::Error> {
    let pool = &state.pool;
    let exists: Option<bool> = sqlx::query_scalar(
        r#"
        SELECT TRUE
        FROM information_schema.tables
        WHERE table_schema = 'public' AND table_name = '_oxyd_tables'
        LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?;
    Ok(exists.unwrap_or(false))
}

/// Carica la config dalla `_oxyd_tables`.
/// Se la tabella registry non esiste → Ok(None) (ambiente dev: tutto esposto).
pub async fn load_table_config(state: &AppState, table: &str) -> ApiResult<Option<OxydTableConfig>> {
    if !oxyd_tables_exists(state).await.map_err(ApiError::from)? {
        return Ok(None);
    }

    let row = sqlx::query(
        r#"
        SELECT table_name, is_exposed, require_auth, allow_insert, allow_update, allow_delete, description
        FROM _oxyd_tables
        WHERE table_name = $1
        "#,
    )
    .bind(table)
    .fetch_optional(&state.pool)
    .await?;

    if let Some(r) = row {
        Ok(Some(OxydTableConfig {
            table_name: r.get("table_name"),
            is_exposed: r.get("is_exposed"),
            require_auth: r.get("require_auth"),
            allow_insert: r.get("allow_insert"),
            allow_update: r.get("allow_update"),
            allow_delete: r.get("allow_delete"),
            description: r.get::<Option<String>, _>("description"),
        }))
    } else {
        // Registry presente ma tabella non registrata → trattala come non esposta
        Ok(Some(OxydTableConfig {
            table_name: table.to_string(),
            is_exposed: false,
            require_auth: true,
            allow_insert: false,
            allow_update: false,
            allow_delete: false,
            description: None,
        }))
    }
}


/// Legge metadati tabella da information_schema / pg_catalog (nessuna cache).
pub async fn load_table_meta(state: &AppState, schema: &str, table: &str) -> ApiResult<TableMeta> {
    let pool = &state.pool;
    // Verifica esistenza tabella
    let exists: Option<bool> = sqlx::query_scalar(
        r#"
        SELECT TRUE
        FROM information_schema.tables
        WHERE table_schema = $1 AND table_name = $2
        LIMIT 1
        "#,
    )
    .bind(schema)
    .bind(table)
    .fetch_optional(pool)
    .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound(format!("table '{}' not found", table)));
    }

    // Colonne
    let cols = sqlx::query(
        r#"
        SELECT column_name, data_type, is_nullable, column_default
        FROM information_schema.columns
        WHERE table_schema = $1 AND table_name = $2
        ORDER BY ordinal_position
        "#,
    )
    .bind(schema)
    .bind(table)
    .map(|row: PgRow| ColumnMeta {
        name: row.get::<String, _>("column_name"),
        data_type: row.get::<String, _>("data_type"),
        is_nullable: row.get::<String, _>("is_nullable") == "YES",
        has_default: row.get::<Option<String>, _>("column_default").is_some(),
    })
    .fetch_all(pool)
    .await?;

    // Primary key (anche composita)
    let pk_cols = sqlx::query_scalar::<_, String>(
        r#"
        SELECT a.attname AS col
        FROM pg_index i
        JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey)
        JOIN pg_class c ON c.oid = i.indrelid
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE i.indisprimary = TRUE
          AND n.nspname = $1
          AND c.relname = $2
        ORDER BY a.attnum
        "#,
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(TableMeta {
        schema: schema.to_string(),
        name: table.to_string(),
        primary_key: pk_cols,
        columns: cols,
    })
}


/// Parsing basilare querystring in stile PostgREST-like.
pub fn parse_query_options(qs: &HashMap<String, String>) -> ApiResult<QueryOptions> {
    let mut opts = QueryOptions::default();

   if let Some(sel) = qs.get("select") {
        let cols = sel
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        if !cols.is_empty() {
            opts.select = Some(cols);
        }
    }

    if let Some(order) = qs.get("order") {
        // order=col.asc | col.desc | col
        let mut parts = order.split('.').map(|s| s.trim());
        let col = parts.next().unwrap_or("").to_string();
        let dir = parts.next().unwrap_or("asc");
        if !col.is_empty() {
            opts.order_by = Some((col, dir.eq_ignore_ascii_case("desc")));
        }
    }

    if let Some(lim) = qs.get("limit") {
        if let Ok(v) = lim.parse::<i64>() {
            opts.limit = Some(v.clamp(1, 1000));
        }
    }
    if let Some(off) = qs.get("offset") {
        if let Ok(v) = off.parse::<i64>() {
            opts.offset = Some(v.max(0));
        }
    }

    // Filtri: qualunque chiave non riservata viene interpretata come filtro
    let reserved = HashSet::from([
        "select".to_string(),
        "order".to_string(),
        "limit".to_string(),
        "offset".to_string(),
    ]);
    for (k, v) in qs {
        if reserved.contains(k) {
            continue;
        }
        // v formati accettati: "eq.42", "like.*foo*", "is.null", "is.not.null", ...
        let (op, val) = if let Some((op_str, rest)) = v.split_once('.') {
            (op_str, Some(rest.to_string()))
        } else {
            // default eq
            ("eq", Some(v.to_string()))
        };

        let op = match op {
            "eq" => FilterOp::Eq,
            "neq" => FilterOp::Neq,
            "gt" => FilterOp::Gt,
            "gte" => FilterOp::Gte,
            "lt" => FilterOp::Lt,
            "lte" => FilterOp::Lte,
            "like" => FilterOp::Like,
            "ilike" => FilterOp::ILike,
            "is" => FilterOp::Is,
            other => return Err(ApiError::BadRequest(format!("unsupported operator '{}'", other))),
        };

        let value = match op {
            FilterOp::Is => val, // atteso "null" o "not.null"
            _ => val,
        };

        opts.filters.push(Filter {
            column: k.to_string(),
            op,
            value,
        });
    }

    Ok(opts)
}



static RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap()
});

fn validate_ident(ident: &str) -> ApiResult<()> {
    if RE.is_match(ident) {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!("invalid identifier '{}'", ident)))
    }
}

// Mappa i data_type di information_schema al cast Postgres da usare nei placeholder ($1::int4, ...).
fn pg_cast_of(dt: &str) -> &'static str {
    match dt.to_lowercase().as_str() {
        // interi
        "smallint" => "int2",
        "integer" => "int4",
        "bigint" => "int8",
        // floating
        "real" => "float4",
        "double precision" => "float8",
        // numerici generici
        "numeric" | "decimal" => "numeric",
        // booleani
        "boolean" => "bool",
        // stringhe
        "character varying" | "character" | "text" | "citext" => "text",
        // uuid
        "uuid" => "uuid",
        // date/time
        "timestamp with time zone" => "timestamptz",
        "timestamp without time zone" => "timestamp",
        "date" => "date",
        "time without time zone" => "time",
        // json
        "jsonb" => "jsonb",
        "json" => "json",
        // fallback prudente: text
        _ => "text",
    }
}

fn is_texty(dt: &str) -> bool {
    matches!(
        dt.to_lowercase().as_str(),
        "character varying" | "character" | "text" | "citext"
    )
}

fn build_select_sql(meta: &TableMeta, opts: &QueryOptions) -> ApiResult<(String, Vec<String>)> {
    // Set colonne consentite
    let colset: HashSet<&str> = meta.columns.iter().map(|c| c.name.as_str()).collect();

    // Index: col_name -> data_type
    let mut coltypes: HashMap<&str, &str> = HashMap::with_capacity(meta.columns.len());
    for c in &meta.columns {
        coltypes.insert(c.name.as_str(), c.data_type.as_str());
    }

    // SELECT
    let select_cols = if let Some(cols) = &opts.select {
        if cols.is_empty() {
            "*".to_string()
        } else {
            let mut acc = Vec::with_capacity(cols.len());
            for c in cols {
                validate_ident(c)?;
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

    // WHERE
    let mut where_sql = String::new();
    let mut binds: Vec<String> = Vec::new();

    if !opts.filters.is_empty() {
        let mut parts: Vec<String> = Vec::with_capacity(opts.filters.len());

        for f in &opts.filters {
            validate_ident(&f.column)?;
            if !colset.contains(f.column.as_str()) {
                return Err(ApiError::BadRequest(format!("unknown column '{}'", f.column)));
            }

            let col = format!(r#""{}""#, f.column);
            let dt = *coltypes
                .get(f.column.as_str())
                .ok_or_else(|| ApiError::BadRequest(format!("missing type for column '{}'", f.column)))?;
            let cast = pg_cast_of(dt);

            // helper per aggiungere bind e restituire placeholder con cast
            let mut next_placeholder = |val: Option<&String>| -> ApiResult<String> {
                let v = val
                    .ok_or_else(|| ApiError::BadRequest(format!("missing value for filter on '{}'", f.column)))?;
                binds.push(v.clone());
                Ok(format!("${}::{}", binds.len(), cast))
            };

            let piece = match f.op {
                FilterOp::Eq => {
                    let ph = next_placeholder(f.value.as_ref())?;
                    format!("{} = {}", col, ph)
                }
                FilterOp::Neq => {
                    let ph = next_placeholder(f.value.as_ref())?;
                    format!("{} <> {}", col, ph)
                }
                FilterOp::Gt => {
                    let ph = next_placeholder(f.value.as_ref())?;
                    format!("{} > {}", col, ph)
                }
                FilterOp::Gte => {
                    let ph = next_placeholder(f.value.as_ref())?;
                    format!("{} >= {}", col, ph)
                }
                FilterOp::Lt => {
                    let ph = next_placeholder(f.value.as_ref())?;
                    format!("{} < {}", col, ph)
                }
                FilterOp::Lte => {
                    let ph = next_placeholder(f.value.as_ref())?;
                    format!("{} <= {}", col, ph)
                }
                FilterOp::Like => {
                    if !is_texty(dt) {
                        return Err(ApiError::BadRequest(format!(
                            "LIKE not supported on non-text column '{}' (type: {})",
                            f.column, dt
                        )));
                    }
                    let ph = {
                        // Per LIKE conviene castare comunque il placeholder a text per sicurezza.
                        binds.push(f.value.clone().unwrap_or_default());
                        format!("${}::text", binds.len())
                    };
                    format!("{} LIKE {}", col, ph)
                }
                FilterOp::ILike => {
                    if !is_texty(dt) {
                        return Err(ApiError::BadRequest(format!(
                            "ILIKE not supported on non-text column '{}' (type: {})",
                            f.column, dt
                        )));
                    }
                    let ph = {
                        binds.push(f.value.clone().unwrap_or_default());
                        format!("${}::text", binds.len())
                    };
                    format!("{} ILIKE {}", col, ph)
                }
                FilterOp::Is => {
                    match f.value.as_deref() {
                        Some("null") => format!("{} IS NULL", col),
                        Some("not.null") => format!("{} IS NOT NULL", col),
                        other => {
                            return Err(ApiError::BadRequest(format!(
                                "is.{} not supported (use 'is.null' or 'is.not.null')",
                                other.unwrap_or("?")
                            )))
                        }
                    }
                }
            };

            parts.push(piece);
        }

        where_sql = format!(" WHERE {}", parts.join(" AND "));
    }

    // ORDER BY
    let order_sql = if let Some((col, desc)) = &opts.order_by {
        validate_ident(col)?;
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

    // Rimpiazza placeholder sentinella di limit/offset con veri placeholder numerati
    let mut extra_binds: Vec<i64> = Vec::new();
    if let Some(l) = opts.limit {
        sql = sql.replace("$LIM", &format!("${}", binds.len() + 1));
        extra_binds.push(l);
    }
    if let Some(o) = opts.offset {
        sql = sql.replace("$OFF", &format!("${}", binds.len() + extra_binds.len() + 1));
        extra_binds.push(o);
    }

    // Concatena binds numerici (come stringhe). Verranno castati implicitamente da Postgres.
    for v in extra_binds {
        binds.push(v.to_string());
    }

    Ok((sql, binds))
}

/// Esegue la SELECT e ritorna un array JSON di righe, con intercettazione/mappatura errori.
/// - Errori di input (cast falliti, operator mismatch, formati data) → 400 BadRequest
/// - Tabella mancante → 404 NotFound
/// - Altri errori → 500 Internal (messaggio generico, errore completo nei log)
pub async fn select_rows(
    state: &AppState,
    schema: &str,
    table: &str,
    opts: &QueryOptions,
) -> ApiResult<Value> {
    let pool = &state.pool;
    let meta = load_table_meta(state, schema, table).await?;

    let (sql, binds) = build_select_sql(&meta, opts)?;

    let mut q = sqlx::query(&sql);
    for b in &binds {
        // I valori sono stringhe; il cast avviene lato Postgres grazie a $n::tipo nella SQL generata.
        q = q.bind(b.as_str());
    }

    let rows = match q.fetch_all(pool).await {
        Ok(rows) => rows,
        Err(e) => {
            use sqlx::Error;

            if let Error::Database(db_err) = &e {
                let code_cow = db_err.code();
                let code = code_cow.as_deref().unwrap_or("");
                let msg = db_err.message();

                match code {
                    // invalid_text_representation, invalid_datetime_format, datatype_mismatch,
                    // cannot_coerce, undefined_function (p.es. "operator does not exist: integer = text")
                    "22P02" | "22007" | "42804" | "42846" | "42883" => {
                        return Err(ApiError::BadRequest(msg.to_string()));
                    }
                    // undefined_column (in teoria prevenuto da validate_ident/colset)
                    "42703" => {
                        return Err(ApiError::BadRequest(format!("unknown column/expression: {}", msg)));
                    }
                    // undefined_table
                    "42P01" => {
                        return Err(ApiError::NotFound(format!(r#"table "{}"."{}" not found"#, schema, table)));
                    }
                    // qualunque altro errore DB → 500 con messaggio generico, dettagli nei log
                    _ => {
                        error!(%code, db_message=%msg, sql=%sql, ?binds, "unhandled database error");
                        return Err(ApiError::Internal("database error".into()));
                    }
                }
            }

            // Non-database error (pool chiuso, timeouts, ecc.)
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

/// Verifica esposizione della tabella usando `_oxyd_tables`.
/// - Se registry NON esiste → consenti.
/// - Se esiste e la tabella non è registrata o non esposta → NotFound.
/// - Ritorna anche il flag `require_auth` per far decidere al caller.
pub async fn ensure_exposed(state: &AppState, table: &str) -> ApiResult<bool> {
    match load_table_config(state, table).await? {
        None => Ok(false), // registry assente → require_auth = false (ambiente dev)
        Some(cfg) => {
            if !cfg.is_exposed {
                return Err(ApiError::NotFound(format!("table '{}' not exposed", table)));
            }
            Ok(cfg.require_auth)
        }
    }
}
