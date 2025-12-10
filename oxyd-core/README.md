# oxyd-core

`oxyd-core` is the Axum/SQLx application that powers Oxyd. It provides ready-to-use primitives for secure, dynamic APIs:

- **JWT authentication** with refresh rotation, configurable TTLs, and middleware that identifies the current user.
- **Virtual CRUD**: a single `/api/{table}` route that generates SELECT/INSERT/UPDATE/DELETE from Postgres metadata and a central registry.
- **Cached registry (Moka)** to know which tables are exposed, whether they require auth, and which operations are enabled.
- **RLS-compatible**: the application context prepares parameters (`app.current_user_id`/similar) so PostgreSQL can enforce Row Level Security policies.

## Architecture

- `src/main.rs`: bootstrap. Loads environment variables, creates the `PgPool`, instantiates `RegistryCache`, and mounts the `auth` and `virtual_crud` routers on Axum.
- `src/auth/`: `/auth/*` routes for login, register, refresh, and me; includes password handling, JWT signing/verification, and response/error types.
- `src/virtual_crud/`: PostgREST-like generic CRUD engine.
  - `routes.rs`: dynamic `/api/{table}` route with handlers for GET/POST/PATCH/DELETE.
  - `registry/`: access to the `oxyd_internal._oxyd_tables` registry with Moka cache for (a) registry existence, (b) table configuration, (c) column/PK metadata.
  - `query.rs` + `types.rs`: parsing of URL params into `QueryOptions` and safe SQL builders (filters `eq`, `lt`, `like`, etc.).
  - `insert.rs`, `update.rs`, `delete.rs`, `select.rs`: operation-specific SQL builders with safeguards (no mass update/delete without filters, PK not editable, etc.).
- `src/general/`: shared types (`AppState` with pool, JWT secret, TTLs, cache), common error handling, and utilities.

## CRUD request flow

1. **Routing**: `/api/{table}` is served by the `virtual_crud` router with shared `state`.
2. **Registry check**: `ensure_exposed` verifies via cache/DB whether the table is exposed and if it requires authentication.
3. **Parsing**: `parse_query_options` handles `select`, `order`, `limit`, `offset`, and PostgREST-style filters.
4. **Safe SQL**: builders generate parameterized queries using column metadata and the PK.
5. **RLS**: Postgres policies apply visibility based on the user context.

## Configuration

Environment variables read at startup:

- `DATABASE_URL` – Postgres connection.
- `JWT_SECRET` – key used to sign JWTs.
- `ACCESS_TTL_SECS` / `REFRESH_TTL_SECS` – token lifetimes (default 15 min / 30 days).

## Future ideas

- **WebAssembly plug-ins** to inject business logic at runtime without recompiling.
- **CLI/SDK** for managing the registry, migrations, and e2e tests.
- **Tracing and metrics** ready for production.
