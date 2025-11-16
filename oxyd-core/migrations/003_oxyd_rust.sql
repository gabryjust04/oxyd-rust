CREATE TABLE "public"."_oxyd_tables" (
    "table_name" text NOT NULL,
    "is_exposed" bool NOT NULL DEFAULT true,
    "require_auth" bool NOT NULL DEFAULT true,
    "allow_insert" bool NOT NULL DEFAULT true,
    "allow_update" bool NOT NULL DEFAULT true,
    "allow_delete" bool NOT NULL DEFAULT true,
    "description" text,
    PRIMARY KEY ("table_name")
);