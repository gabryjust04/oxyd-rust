use serde::{Deserialize, Serialize};

/// Metadati colonna letti da information_schema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnMeta {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    pub has_default: bool,
}

/// Metadati tabella (schema + pk + colonne)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableMeta {
    pub schema: String,               // es. "public"
    pub name: String,                 // es. "posts"
    pub primary_key: Vec<String>,     // anche composita (vuoto se nessuna PK)
    pub columns: Vec<ColumnMeta>,
}

/// Config registrata in _oxyd_tables
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OxydTableConfig {
    pub table_name: String,
    pub is_exposed: bool,
    pub require_auth: bool,
    pub allow_insert: bool,
    pub allow_update: bool,
    pub allow_delete: bool,
    pub description: Option<String>,
}

/// Operatori filtro in stile PostgREST-like (subset minimale)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
    Like,
    ILike,
    Is,       // IS NULL / IS NOT NULL (gestito con value = "null"/"not.null")
}

#[derive(Debug, Clone)]
pub struct Filter {
    pub column: String,
    pub op: FilterOp,
    pub value: Option<String>, // None per IS NULL / IS NOT NULL
}

/// Opzioni query parse da query string
#[derive(Debug, Clone, Default)]
pub struct QueryOptions {
    pub select: Option<Vec<String>>,               // select=col1,col2
    pub filters: Vec<Filter>,                      // id=eq.42  name=like.*abc*
    pub order_by: Option<(String, bool)>,          // order=col.asc|col.desc
    pub limit: Option<i64>,                        // limit=...
    pub offset: Option<i64>,                       // offset=...
}