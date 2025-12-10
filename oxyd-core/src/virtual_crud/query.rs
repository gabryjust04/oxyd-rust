// oxyd-core/src/virtual_crud/query.rs

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::general::errors::{ApiError, ApiResult};
use super::types::*;

/// Parse a PostgREST-like query string into a strongly-typed [`QueryOptions`].
///
/// Supported query parameters:
///
/// - `select=id,name,...`
/// - `order=col.asc` / `order=col.desc` / `order=col`
/// - `limit=10`
/// - `offset=20`
/// - Filters on any non-reserved key:
///     - `id=eq.42`
///     - `name=like.*foo*`
///     - `name=ilike.*bar*`
///     - `deleted_at=is.null`
///     - `deleted_at=is.not.null`
///
/// Anything that is not `select`, `order`, `limit`, or `offset` is treated as a filter.
pub fn parse_query_options(qs: &HashMap<String, String>) -> ApiResult<QueryOptions> {
    let mut opts = QueryOptions::default();

    // ── SELECT clause parsing ──────────────────────────────────────────────────
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

    // ── ORDER BY parsing ───────────────────────────────────────────────────────
    if let Some(order) = qs.get("order") {
        // order=col.asc | col.desc | col
        let mut parts = order.split('.').map(|s| s.trim());
        let col = parts.next().unwrap_or("").to_string();
        let dir = parts.next().unwrap_or("asc");

        if !col.is_empty() {
            // Store: (column, is_descending)
            opts.order_by = Some((col, dir.eq_ignore_ascii_case("desc")));
        }
    }

    // ── LIMIT / OFFSET parsing with basic sanity limits ───────────────────────
    if let Some(lim) = qs.get("limit") {
        if let Ok(v) = lim.parse::<i64>() {
            // Clamp to a reasonable range to avoid accidental "download the world".
            opts.limit = Some(v.clamp(1, 1000));
        }
    }

    if let Some(off) = qs.get("offset") {
        if let Ok(v) = off.parse::<i64>() {
            // Offset cannot be negative.
            opts.offset = Some(v.max(0));
        }
    }

    // ── Filters: any non-reserved key becomes a filter on that column ─────────
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

        // Accepted value formats:
        //   "eq.42"
        //   "like.*foo*"
        //   "is.null"
        //   "is.not.null"
        // If no operator is given, default to "eq".
        let (op_str, val_str) = if let Some((op_part, rest)) = v.split_once('.') {
            (op_part, Some(rest.to_string()))
        } else {
            ("eq", Some(v.to_string()))
        };

        // Map textual operator to enum.
        let op = match op_str {
            "eq" => FilterOp::Eq,
            "neq" => FilterOp::Neq,
            "gt" => FilterOp::Gt,
            "gte" => FilterOp::Gte,
            "lt" => FilterOp::Lt,
            "lte" => FilterOp::Lte,
            "like" => FilterOp::Like,
            "ilike" => FilterOp::ILike,
            "is" => FilterOp::Is,
            other => {
                return Err(ApiError::BadRequest(format!(
                    "unsupported operator '{}'",
                    other
                )))
            }
        };

        // `is` filters expect things like "null" or "not.null"; we still keep
        // the raw string and validate later in `build_where_sql`.
        let value = match op {
            FilterOp::Is => val_str,
            _ => val_str,
        };

        opts.filters.push(Filter {
            column: k.to_string(),
            op,
            value,
        });
    }

    Ok(opts)
}

// Global regex for validating SQL identifiers (simple whitelist).
//
// This accepts standard "unquoted identifier" syntax:
//   - starts with letter or underscore
//   - then letters, digits, or underscores
//
// Anything more complex should be rejected to avoid injection surfaces.
static RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap()
});

/// Validate that an identifier (column name, etc.) is syntactically safe.
///
/// Note: this does not check that the column *exists*; that's done elsewhere.
/// It only ensures the string is a safe identifier that we can interpolate
/// as `"identifier"` in SQL without risk of injection.
pub fn validate_ident(ident: &str) -> ApiResult<()> {
    if RE.is_match(ident) {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!(
            "invalid identifier '{}'",
            ident
        )))
    }
}

/// Map `information_schema.data_type` strings to PostgreSQL cast targets.
///
/// This is used to generate placeholders such as `$1::int4`, `$2::timestamptz`, etc.
/// We reduce the wide variety of `information_schema` names to a smaller set of
/// canonical internal types.
pub fn pg_cast_of(dt: &str) -> &'static str {
    match dt.to_lowercase().as_str() {
        // integers
        "smallint" => "int2",
        "integer" => "int4",
        "bigint" => "int8",

        // floating point
        "real" => "float4",
        "double precision" => "float8",

        // generic numeric
        "numeric" | "decimal" => "numeric",

        // boolean
        "boolean" => "bool",

        // strings
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

        // Safe fallback: treat unknown types as text.
        _ => "text",
    }
}

/// Helper to decide if a type behaves as "text-like" for LIKE/ILIKE usage.
fn is_texty(dt: &str) -> bool {
    matches!(
        dt.to_lowercase().as_str(),
        "character varying" | "character" | "text" | "citext"
    )
}

/// Build the `WHERE` SQL fragment and fill the `binds` vector with values.
///
/// This function:
/// - Validates that filtered columns exist and are valid identifiers.
/// - Infers the column type from metadata and computes the right cast.
/// - Generates `$N::type` expressions for each filter value.
/// - Pushes each value into `binds` in the exact order placeholders appear.
///
/// Example output:
/// - ` WHERE "id" = $1::int4 AND "name" ILIKE $2::text`
pub fn build_where_sql(
    meta: &TableMeta,
    opts: &QueryOptions,
    binds: &mut Vec<String>,
) -> ApiResult<String> {
    // Fast membership check for allowed columns.
    let colset: HashSet<&str> = meta.columns.iter().map(|c| c.name.as_str()).collect();

    // Index column types by name: "id" → "integer", "name" → "text", ...
    let mut coltypes: HashMap<&str, &str> = HashMap::with_capacity(meta.columns.len());
    for c in &meta.columns {
        coltypes.insert(c.name.as_str(), c.data_type.as_str());
    }

    // No filters → no WHERE clause.
    if opts.filters.is_empty() {
        return Ok(String::new());
    }

    let mut parts: Vec<String> = Vec::with_capacity(opts.filters.len());

    for f in &opts.filters {
        // Check identifier syntax.
        validate_ident(&f.column)?;

        // Check that the column is actually present in the table.
        if !colset.contains(f.column.as_str()) {
            return Err(ApiError::BadRequest(format!(
                "unknown column '{}'",
                f.column
            )));
        }

        // Quoted column name for safety.
        let col = format!(r#""{}""#, f.column);

        // Lookup column type; if missing, we fail with a clear error.
        let dt = *coltypes
            .get(f.column.as_str())
            .ok_or_else(|| {
                ApiError::BadRequest(format!("missing type for column '{}'", f.column))
            })?;

        let cast = pg_cast_of(dt);

        // Small closure to:
        // - ensure a value is present
        // - push it to the binds list
        // - return the placeholder string `${index}::type`
        let mut next_placeholder = |val: Option<&String>| -> ApiResult<String> {
            let v = val.ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "missing value for filter on '{}'",
                    f.column
                ))
            })?;
            binds.push(v.clone());
            Ok(format!("${}::{}", binds.len(), cast))
        };

        // Build the SQL piece for this single filter.
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
                // LIKE only makes sense for text-like columns.
                if !is_texty(dt) {
                    return Err(ApiError::BadRequest(format!(
                        "LIKE not supported on non-text column '{}' (type: {})",
                        f.column, dt
                    )));
                }

                // Use the raw value (or empty string if missing) and cast to text.
                binds.push(f.value.clone().unwrap_or_default());
                let ph = format!("${}::text", binds.len());
                format!("{} LIKE {}", col, ph)
            }
            FilterOp::ILike => {
                // ILIKE only makes sense for text-like columns.
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
                    // Anything else is rejected with a clear hint.
                    return Err(ApiError::BadRequest(format!(
                        "is.{} not supported (use 'is.null' or 'is.not.null')",
                        other.unwrap_or("?")
                    )))
                }
            },
        };

        parts.push(piece);
    }

    // Join all filter pieces with AND.
    Ok(format!(" WHERE {}", parts.join(" AND ")))
}
