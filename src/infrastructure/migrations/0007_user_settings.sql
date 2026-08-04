CREATE TABLE user_settings (
    user_id       UUID        PRIMARY KEY NOT NULL,
    base_currency TEXT        NOT NULL DEFAULT 'UAH',
    updated_at    TIMESTAMPTZ NOT NULL
);
