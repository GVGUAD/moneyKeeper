-- Classification taxonomy: tenant-scoped hierarchy and aggregate version.

CREATE TABLE classification.category_taxonomies (
    user_id UUID PRIMARY KEY,
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    starter_template_version INTEGER CHECK (starter_template_version >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE classification.category_command_receipts (
    user_id UUID NOT NULL,
    command_scope TEXT NOT NULL,
    idempotency_key TEXT NOT NULL CHECK (
        idempotency_key <> '' AND octet_length(idempotency_key) <= 200
    ),
    request_hash BYTEA NOT NULL CHECK (octet_length(request_hash) = 32),
    response_body JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (user_id, command_scope, idempotency_key)
);

ALTER TABLE classification.categories
    ADD COLUMN parent_id UUID,
    ADD COLUMN position INTEGER,
    ADD COLUMN color VARCHAR(7),
    ADD COLUMN icon TEXT;

WITH ranked AS (
    SELECT id,
           user_id,
           row_number() OVER (
               PARTITION BY user_id
               ORDER BY lower(name), id
           ) - 1 AS position
    FROM classification.categories
)
UPDATE classification.categories AS category
SET position = ranked.position
FROM ranked
WHERE category.id = ranked.id
  AND category.user_id = ranked.user_id;

ALTER TABLE classification.categories
    ALTER COLUMN position SET NOT NULL,
    ADD CONSTRAINT categories_position_non_negative CHECK (position >= 0),
    ADD CONSTRAINT categories_color_is_hex CHECK (
        color IS NULL OR color COLLATE "C" ~ '^#[0-9A-F]{6}$'
    ),
    ADD CONSTRAINT categories_icon_is_known CHECK (
        icon IS NULL OR icon IN (
            'tag', 'wallet-plus', 'briefcase', 'laptop', 'chart-line',
            'circle-ellipsis', 'wallet-minus', 'house', 'building', 'bolt',
            'utensils', 'shopping-basket', 'coffee', 'car', 'bus', 'fuel',
            'taxi', 'heart-pulse', 'pill', 'shopping-bag', 'clapperboard',
            'repeat', 'plane', 'graduation-cap', 'gift', 'receipt'
        )
    ),
    ADD CONSTRAINT categories_tenant_parent_fk
        FOREIGN KEY (parent_id, user_id)
        REFERENCES classification.categories (id, user_id)
        ON DELETE RESTRICT
        DEFERRABLE INITIALLY DEFERRED,
    ADD CONSTRAINT categories_sibling_position_unique
        UNIQUE NULLS NOT DISTINCT (user_id, parent_id, position)
        DEFERRABLE INITIALLY DEFERRED;

DROP INDEX classification.categories_active_name_unique;

CREATE UNIQUE INDEX categories_active_sibling_name_unique
    ON classification.categories (
        user_id,
        COALESCE(parent_id, '00000000-0000-0000-0000-000000000000'::uuid),
        lower(name)
    )
    WHERE lifecycle = 'active';

DROP INDEX classification.categories_user_lifecycle_name;

CREATE INDEX categories_user_tree_order
    ON classification.categories (user_id, parent_id, position, id);

INSERT INTO classification.category_taxonomies (
    user_id,
    version,
    starter_template_version,
    created_at,
    updated_at
)
SELECT user_id,
       1,
       NULL,
       min(created_at),
       max(updated_at)
FROM classification.categories
GROUP BY user_id;
