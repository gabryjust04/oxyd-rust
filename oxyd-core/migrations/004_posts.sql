-- 004_posts.sql


CREATE TABLE public.posts (
    id         bigserial PRIMARY KEY,
    user_id    bigint NOT NULL REFERENCES oxyd_auth.users(id),
    title      text NOT NULL,
    content    text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE public.posts ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.posts FORCE ROW LEVEL SECURITY;

ALTER TABLE public.posts ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.posts FORCE ROW LEVEL SECURITY;

CREATE POLICY posts_select_own ON public.posts
FOR SELECT USING (
  user_id = current_setting('app.current_user_id', true)::bigint
);

CREATE POLICY posts_insert_own ON public.posts
FOR INSERT WITH CHECK (
  user_id = current_setting('app.current_user_id', true)::bigint
);

CREATE POLICY posts_update_own ON public.posts
FOR UPDATE USING (
  user_id = current_setting('app.current_user_id', true)::bigint
) WITH CHECK (
  user_id = current_setting('app.current_user_id', true)::bigint
);

CREATE POLICY posts_delete_own ON public.posts
FOR DELETE USING (
  user_id = current_setting('app.current_user_id', true)::bigint
);

INSERT INTO oxyd_internal._oxyd_tables (
    table_name,
    is_exposed,
    require_auth
) VALUES (
    'posts',
    true,
    true
);