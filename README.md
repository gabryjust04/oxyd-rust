# Oxyd (Rust)

Oxyd is a pilot project that aims to become a **Rust-powered micro-framework**: it ships with JWT auth, a PostgREST-style virtual CRUD, and a foundation built for speed, security, and modularity.

## Vision

- **First-class performance**: Rust, Axum, and SQLx to serve secure, fast APIs backed by Postgres and Moka caching.
- **Ready out of the box**: prewired login/refresh/me endpoints, virtual CRUD that exposes tables dynamically, and user context propagation for RLS.
- **Secure by default**: input validation, safely generated SQL, and checks on exposed tables and permissions.
- **Extensible**: modular design (auth, CRUD, registry) that allows new behaviors without rewriting handlers.

## What it offers today

- **JWT auth** with refresh rotation and configurable TTLs via env (`ACCESS_TTL_SECS`, `REFRESH_TTL_SECS`).
- **Virtual CRUD** generated at runtime from Postgres metadata, with Moka caching for registry and schema.
- **Central registry** (`oxyd_internal._oxyd_tables`) that decides which tables are exposed, whether they require auth, and which operations are enabled.
- **RLS friendly**: relies on database Row Level Security using an application context for the current user.

## Roadmap

- **WASM plug-ins**: load WebAssembly scripts at runtime to add custom logic without redeploying.
- **DX toolkit**: generators for table/registry scaffolding, management CLI, and client packages.
- **Observability**: structured logging, distributed tracing, and production-ready metrics.

## Repository structure

- `oxyd-core/`: the heart of the app (Axum + SQLx) with modules for auth, virtual CRUD, and shared types.
- `migrations/` (under `oxyd-core/`): SQLx migrations for the registry and DB bootstrap.
- `tests/`: end-to-end scenarios for the core.

## Getting started

1. Export environment variables (`DATABASE_URL`, `JWT_SECRET`, `ACCESS_TTL_SECS`, `REFRESH_TTL_SECS`).
2. `cd oxyd-core && cargo run` to start the server on `0.0.0.0:8080`.
3. Use `/auth/login` and `/auth/register` to generate tokens, then hit `/api/{table}` for CRUD.

For more details on internal modules, see `oxyd-core/README.md`.