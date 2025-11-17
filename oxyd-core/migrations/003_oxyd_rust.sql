-- 003_oxyd_tables.sql

DROP TABLE IF EXISTS oxyd_internal._oxyd_tables;

CREATE TABLE oxyd_internal._oxyd_tables (
    table_name   text NOT NULL,
    is_exposed   boolean NOT NULL DEFAULT true,
    require_auth boolean NOT NULL DEFAULT true,
    allow_insert boolean NOT NULL DEFAULT true,
    allow_update boolean NOT NULL DEFAULT true,
    allow_delete boolean NOT NULL DEFAULT true,
    description  text,
    CONSTRAINT _oxyd_tables_pkey PRIMARY KEY (table_name)
);

ALTER TABLE oxyd_internal._oxyd_tables DISABLE ROW LEVEL SECURITY;