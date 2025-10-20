-- 002_refresh_sessions.sql
CREATE TABLE refresh_sessions (
  id BIGSERIAL PRIMARY KEY,
  user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash TEXT NOT NULL UNIQUE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  expires_at TIMESTAMPTZ NOT NULL,
  revoked BOOLEAN NOT NULL DEFAULT FALSE,
  replaced_by BIGINT REFERENCES refresh_sessions(id)
);

CREATE INDEX ON refresh_sessions (user_id);
CREATE INDEX ON refresh_sessions (expires_at);