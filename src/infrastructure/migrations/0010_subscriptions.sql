CREATE TABLE subscriptions (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    provider TEXT NOT NULL,
    product_name TEXT NOT NULL,
    merchant_key TEXT NOT NULL,
    amount NUMERIC NOT NULL,
    currency TEXT NOT NULL,
    billing_period TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at BIGINT NOT NULL,
    last_charged_at BIGINT,
    next_expected_at BIGINT,
    category_id UUID,
    created_at BIGINT NOT NULL,

    CONSTRAINT subscriptions_user_merchant_unique UNIQUE (user_id, merchant_key)
);

CREATE INDEX subscriptions_user_status_idx ON subscriptions (user_id, status);
