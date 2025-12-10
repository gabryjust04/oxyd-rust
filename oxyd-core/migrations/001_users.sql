-- 001_users.sql
DROP TABLE    IF EXISTS oxyd_auth.users;
DROP SEQUENCE IF EXISTS oxyd_auth.users_id_seq;

CREATE SEQUENCE IF NOT EXISTS oxyd_auth.users_id_seq;

CREATE TABLE oxyd_auth.users (
    id            bigint NOT NULL DEFAULT nextval('oxyd_auth.users_id_seq'::regclass),
    email         text   NOT NULL,
    password_hash text   NOT NULL,
    is_active     boolean NOT NULL DEFAULT true,
    created_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT users_pkey PRIMARY KEY (id),
    CONSTRAINT users_email_key UNIQUE (email)
);

ALTER SEQUENCE oxyd_auth.users_id_seq OWNED BY oxyd_auth.users.id;

-- niente RLS qui: tabella interna usata solo dal backend
ALTER TABLE oxyd_auth.users DISABLE ROW LEVEL SECURITY;