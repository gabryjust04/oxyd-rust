-- 000_init.sql

/* ---------------------------------------------------------------------------
   0. CREA RUOLO oxyd_app (se non esiste)
   Questo deve essere eseguito con utente superuser (es. postgres)
--------------------------------------------------------------------------- */
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'oxyd_app') THEN
        CREATE ROLE oxyd_app LOGIN PASSWORD 'oxyd_password';
    END IF;
END$$;

/* ---------------------------------------------------------------------------
   1. PERMESSO A CONNETTERSI AL DATABASE
--------------------------------------------------------------------------- */
GRANT CONNECT ON DATABASE mydb TO oxyd_app;


/* ---------------------------------------------------------------------------
   2. CREA SCHEMI (se non esistono)
--------------------------------------------------------------------------- */
CREATE SCHEMA IF NOT EXISTS oxyd_auth;
CREATE SCHEMA IF NOT EXISTS oxyd_internal;

/* ---------------------------------------------------------------------------
   3. PERMESSI DI USO DEGLI SCHEMI
--------------------------------------------------------------------------- */
GRANT USAGE ON SCHEMA public        TO oxyd_app;
GRANT USAGE ON SCHEMA oxyd_auth     TO oxyd_app;
GRANT USAGE ON SCHEMA oxyd_internal TO oxyd_app;
