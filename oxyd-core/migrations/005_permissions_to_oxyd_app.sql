-- 005_permissions_to_oxyd_app.sql


-- 3.1 Permesso ad usare gli schemi (necessario per referenziare le tabelle)
GRANT USAGE ON SCHEMA public        TO oxyd_app;
GRANT USAGE ON SCHEMA oxyd_auth     TO oxyd_app;
GRANT USAGE ON SCHEMA oxyd_internal TO oxyd_app;

-- 3.2 Permessi CRUD su tutte le tabelle esistenti negli schemi
--  - public: qui ci metterai le tabelle "dati" con RLS
--  - oxyd_auth: users + refresh_sessions (no RLS, solo backend)
--  - oxyd_internal: _oxyd_tables (no RLS, solo backend)
GRANT SELECT, INSERT, UPDATE, DELETE
    ON ALL TABLES IN SCHEMA public        TO oxyd_app;
GRANT SELECT, INSERT, UPDATE, DELETE
    ON ALL TABLES IN SCHEMA oxyd_auth     TO oxyd_app;
GRANT SELECT, INSERT, UPDATE, DELETE
    ON ALL TABLES IN SCHEMA oxyd_internal TO oxyd_app;

-- 3.3 Permessi sulle SEQUENZE esistenti (per nextval, currval, ecc.)
GRANT USAGE, SELECT, UPDATE
    ON ALL SEQUENCES IN SCHEMA public        TO oxyd_app;
GRANT USAGE, SELECT, UPDATE
    ON ALL SEQUENCES IN SCHEMA oxyd_auth     TO oxyd_app;
GRANT USAGE, SELECT, UPDATE
    ON ALL SEQUENCES IN SCHEMA oxyd_internal TO oxyd_app;
