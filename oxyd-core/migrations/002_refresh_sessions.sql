-- 002_refresh_sessions.sql
DROP TABLE    IF EXISTS oxyd_auth.refresh_sessions;
DROP SEQUENCE IF EXISTS oxyd_auth.refresh_sessions_id_seq;

CREATE SEQUENCE IF NOT EXISTS oxyd_auth.refresh_sessions_id_seq;

CREATE TABLE oxyd_auth.refresh_sessions (
    id          bigint NOT NULL DEFAULT nextval('oxyd_auth.refresh_sessions_id_seq'::regclass),
    user_id     bigint NOT NULL,
    token_hash  text   NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    revoked     boolean NOT NULL DEFAULT false,
    replaced_by bigint,
    CONSTRAINT refresh_sessions_pkey PRIMARY KEY (id),
    CONSTRAINT refresh_sessions_user_id_fkey
        FOREIGN KEY (user_id)
        REFERENCES oxyd_auth.users(id)
        ON DELETE CASCADE,
    CONSTRAINT refresh_sessions_replaced_by_fkey
        FOREIGN KEY (replaced_by)
        REFERENCES oxyd_auth.refresh_sessions(id)
);

CREATE UNIQUE INDEX refresh_sessions_token_hash_key
    ON oxyd_auth.refresh_sessions (token_hash);

CREATE INDEX refresh_sessions_user_id_idx
    ON oxyd_auth.refresh_sessions (user_id);

CREATE INDEX refresh_sessions_expires_at_idx
    ON oxyd_auth.refresh_sessions (expires_at);

ALTER SEQUENCE oxyd_auth.refresh_sessions_id_seq
    OWNED BY oxyd_auth.refresh_sessions.id;

ALTER TABLE oxyd_auth.refresh_sessions DISABLE ROW LEVEL SECURITY;