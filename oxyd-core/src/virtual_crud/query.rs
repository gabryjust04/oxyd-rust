// oxyd-core/src/virtual_crud/query.rs



use std::collections::{HashMap, HashSet};

use regex::Regex;
use std::sync::LazyLock;

use crate::general::errors::{ApiError, ApiResult};
use super::types::*;




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

pub fn validate_ident(ident: &str) -> ApiResult<()> {
    if RE.is_match(ident) {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!("invalid identifier '{}'", ident)))
    }
}

// Mappa i data_type di information_schema al cast Postgres da usare nei placeholder ($1::int4, ...).
pub fn pg_cast_of(dt: &str) -> &'static str {
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



pub fn build_where_sql(
    meta: &TableMeta,
    opts: &QueryOptions,
    binds: &mut Vec<String>,
) -> ApiResult<String> {
    let colset: HashSet<&str> = meta.columns.iter().map(|c| c.name.as_str()).collect();

    // indice tipi
    let mut coltypes: HashMap<&str, &str> = HashMap::with_capacity(meta.columns.len());
    for c in &meta.columns {
        coltypes.insert(c.name.as_str(), c.data_type.as_str());
    }

    if opts.filters.is_empty() {
        return Ok(String::new());
    }

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

        let mut next_placeholder = |val: Option<&String>| -> ApiResult<String> {
            let v = val.ok_or_else(|| {
                ApiError::BadRequest(format!("missing value for filter on '{}'", f.column))
            })?;
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
                binds.push(f.value.clone().unwrap_or_default());
                let ph = format!("${}::text", binds.len());
                format!("{} LIKE {}", col, ph)
            }
            FilterOp::ILike => {
                if !is_texty(dt) {
                    return Err(ApiError::BadRequest(format!(
                        "ILIKE not supported on non-text column '{}' (type: {})",
                        f.column, dt
                    )));
                }
                binds.push(f.value.clone().unwrap_or_default());
                let ph = format!("${}::text", binds.len());
                format!("{} ILIKE {}", col, ph)
            }
            FilterOp::Is => match f.value.as_deref() {
                Some("null") => format!("{} IS NULL", col),
                Some("not.null") => format!("{} IS NOT NULL", col),
                other => {
                    return Err(ApiError::BadRequest(format!(
                        "is.{} not supported (use 'is.null' or 'is.not.null')",
                        other.unwrap_or("?")
                    )))
                }
            },
        };

        parts.push(piece);
    }

    Ok(format!(" WHERE {}", parts.join(" AND ")))
}