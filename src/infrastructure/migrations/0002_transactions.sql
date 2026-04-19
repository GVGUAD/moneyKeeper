CREATE TABLE categories (
    id         UUID        PRIMARY KEY NOT NULL,
    user_id    UUID        NOT NULL,
    name       TEXT        NOT NULL,
    color      TEXT,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_categories_user_id ON categories(user_id);

CREATE TABLE transactions (
    id           UUID        PRIMARY KEY NOT NULL,
    account_id   UUID        NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    user_id      UUID        NOT NULL,
    amount       NUMERIC     NOT NULL,
    currency     TEXT        NOT NULL,
    kind         TEXT        NOT NULL,
    -- kind values: Income | Expense | Transfer | Buy | Sell | StakingReward
    category_id  UUID        REFERENCES categories(id) ON DELETE SET NULL,
    note         TEXT,
    external_id  TEXT,
    transacted_at TIMESTAMPTZ NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL
);

-- Partial unique index for idempotent Monobank inserts
CREATE UNIQUE INDEX transactions_external_id_unique
    ON transactions (external_id)
    WHERE external_id IS NOT NULL;

CREATE INDEX idx_transactions_user_id       ON transactions(user_id);
CREATE INDEX idx_transactions_account_id    ON transactions(account_id);
CREATE INDEX idx_transactions_transacted_at ON transactions(transacted_at);
CREATE INDEX idx_transactions_category_id   ON transactions(category_id);

CREATE TABLE transfer_links (
    from_transaction_id UUID PRIMARY KEY NOT NULL REFERENCES transactions(id) ON DELETE CASCADE,
    to_transaction_id   UUID NOT NULL            REFERENCES transactions(id) ON DELETE CASCADE
);

CREATE TABLE trade_details (
    transaction_id UUID    PRIMARY KEY NOT NULL REFERENCES transactions(id) ON DELETE CASCADE,
    ticker         TEXT    NOT NULL,
    quantity       NUMERIC NOT NULL,
    price_per_unit NUMERIC,
    fee            NUMERIC
);
