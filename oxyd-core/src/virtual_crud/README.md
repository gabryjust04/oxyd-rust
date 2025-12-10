# Virtual CRUD layer (`oxyd-core/src/virtual_crud`)

This module implements a **virtual, PostgREST-style CRUD layer** on top of PostgreSQL, using:

- **Axum** for HTTP routing,
- **SQLx** for database access,
- **Postgres metadata** (`information_schema` / `pg_catalog`) to introspect tables at runtime,
- **RLS (Row Level Security)** with a per-transaction `app.current_user_id` context.

The goal is to expose **generic REST endpoints per table** without writing ad-hoc handlers for each resource, while remaining:

- type-aware,
- RLS-friendly,
- safe against SQL injection and mass operations (no blind `DELETE FROM foo` / `UPDATE foo`).

---

## 1. High-level architecture

### 1.1 Main entrypoint: `routes.rs`

The `router` function wires all HTTP methods on a single dynamic route:

```rust
pub fn router(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/{table}",
            get(list_handler)
                .post(insert_handler)
                .patch(update_handler)
                .delete(delete_handler),
        )
        .with_state(state)
}
```

For any path `/api/{table}` we expose:

- **GET** → `list_handler` → dynamic SELECT  
- **POST** → `insert_handler` → dynamic INSERT (single or bulk)  
- **PATCH** → `update_handler` → dynamic UPDATE with filters  
- **DELETE** → `delete_handler` → dynamic DELETE with filters  

Each handler:

- Calls `ensure_exposed` to check if the table is allowed to be exposed.
- Optionally enforces authentication based on registry configuration.
- Parses query string into a `QueryOptions` struct (`parse_query_options`).
- Delegates the actual database operation to:
  - `select_rows`
  - `insert_rows`
  - `update_rows`
  - `delete_rows`
- All handlers expect `AppState` to contain at least a `PgPool` (`state.pool`).

---

### 1.2 Table registry: `registry.rs`

The registry is stored in a dedicated table:

```
oxyd_internal._oxyd_tables
```

with (at least) the following columns:

- `table_name text`
- `is_exposed bool`
- `require_auth bool`
- `allow_insert bool`
- `allow_update bool`
- `allow_delete bool`
- `description text`

#### Dev mode vs Prod mode

If the registry table does not exist:

- `load_table_config` returns `Ok(None)`.
- `ensure_exposed` treats this as **dev mode** → all tables are exposed and `require_auth = false`.

If the registry exists:

- If `is_exposed = false` → the table is hidden and the API returns **404**.

Key functions:

- `oxyd_tables_exists(state)` → checks registry existence  
- `load_table_config(state, table)` → loads per-table config  
- `load_table_meta(state, schema, table)` → loads columns, types, PK  
- `ensure_exposed(state, table)` → central visibility/auth logic  

---

### 1.3 Query parsing and SQL builders: `query.rs`

The module parses URL parameters into `QueryOptions`, then builds SQL.

#### `parse_query_options`

Supported parameters:

- `select` → comma-separated projection  
- `order` → `col.asc` / `col.desc`  
- `limit` (1–1000)  
- `offset` (>= 0)  
- **Filters** → any non-reserved key

Reserved: `select`, `order`, `limit`, `offset`

Filter syntax (PostgREST-style):

- `col=eq.42`
- `col=neq.42`
- `col=gt.10`
- `col=gte.10`
- `col=lt.10`
- `col=lte.10`
- `col=like.*foo*`
- `col=ilike.*foo*`
- `col=is.null`
- `col=is.not.null`

#### `build_where_sql(meta, opts, binds)`

- Validates column names
- Ensures column exists
- Pushes bind params (`$1`, `$2`, …)
- Applies correct PostgreSQL cast

Example:

```
WHERE "id" = $1::int4 AND "name" ILIKE $2::text
```

---

### 1.4 RLS and per-transaction user context

For each query:

1. Start transaction  
2. If user exists:  
   ```
   SELECT set_config('app.current_user_id', $1, true)
   ```
3. Execute SQL under RLS  
4. Commit/rollback  

RLS policies can read:

```
current_setting('app.current_user_id', true)
```

---

## 2. Handlers and behaviour

### 2.1 GET /api/{table} – SELECT

Flow:

1. `ensure_exposed`
2. Enforce auth
3. Parse query options
4. `select_rows`
5. Set RLS context
6. Run SELECT

Response:

```
[
  { "id": 1, "title": "hello" },
  { "id": 2, "title": "world" }
]
```

Examples:

```
GET /api/posts
GET /api/posts?select=id,title&order=created_at.desc
GET /api/posts?id=eq.42
```

---

### 2.2 POST /api/{table} – INSERT

Supports:

- Single JSON object  
- JSON array for bulk insert  

If table has `user_id`:

- If provided and ≠ authenticated user → 400
- Backend always forces `user_id = current_user`

Returns inserted rows.

Examples:

```
POST /api/posts
{ "title": "Hello", "body": "First post" }
```

Bulk:

```
POST /api/posts
[
  { "title": "A" },
  { "title": "B", "status": "draft" }
]
```

---

### 2.3 PATCH /api/{table} – UPDATE

Rules:

- Body is partial update object
- MUST include at least one filter (to avoid mass updates)
- Cannot update primary key
- Cannot update `user_id`

Example:

```
PATCH /api/posts?id=eq.42
{ "title": "Updated title" }
```

---

### 2.4 DELETE /api/{table} – DELETE

Rules:

- Requires filters  
- RLS enforced  
- Returns deleted rows  

Example:

```
DELETE /api/posts?id=eq.42
```

---

## 3. Query language summary

### 3.1 Special parameters

- `select`
- `order`
- `limit`
- `offset`

### 3.2 Filters

See section 1.3.

---

## 4. HTTP status codes

- **400** → invalid input, unknown column, constraint violation  
- **401** → auth required  
- **404** → table not exposed / not found  
- **500** → internal DB error  

---

## 5. Using the virtual CRUD

### Examples

```
GET /api/posts?select=id,title&limit=20
GET /api/posts?id=eq.42
POST /api/posts { ... }
PATCH /api/posts?id=eq.42 { ... }
DELETE /api/posts?id=eq.42
```

RLS controls row visibility.

---

## 6. Adding a new table

1. Create table  
2. (Optional) Add RLS using `current_setting`  
3. Add row in:

```
oxyd_internal._oxyd_tables
```

Example:

```sql
INSERT INTO oxyd_internal._oxyd_tables (
    table_name, is_exposed, require_auth, allow_insert, allow_update, allow_delete, description
) VALUES (
    'posts', true, true, true, true, true,
    'Blog posts exposed via virtual CRUD'
);
```

No Rust changes needed.

---

This module stays small and secure by letting PostgreSQL enforce integrity and RLS, while Rust handles:

- input validation  
- safe SQL generation  
- consistent error mapping  
