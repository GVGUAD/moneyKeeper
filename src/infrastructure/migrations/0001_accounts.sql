CREATE TABLE accounts (
    id          UUID        PRIMARY KEY NOT NULL,
    user_id     UUID        NOT NULL,
    name        TEXT        NOT NULL,
    account_type TEXT       NOT NULL,
    currency    TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL
);

-- account_type values: Cash | Bank | Savings | Loan | Investment | Binance
CREATE INDEX idx_accounts_user_id ON accounts(user_id);

CREATE TABLE savings_details (
    account_id         UUID    PRIMARY KEY NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    interest_rate      NUMERIC NOT NULL,
    compounding_period TEXT    NOT NULL -- values: Daily | Monthly | Quarterly | Annually
);

CREATE TABLE loan_details (
    account_id    UUID    PRIMARY KEY NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    counterparty  TEXT    NOT NULL,
    direction     TEXT    NOT NULL, -- values: Borrowed | Lent
    interest_rate NUMERIC,
    due_date      DATE
);

CREATE TABLE investment_details (
    account_id UUID PRIMARY KEY NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    broker     TEXT
);

CREATE TABLE binance_details (
    account_id UUID PRIMARY KEY NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    label      TEXT
);
