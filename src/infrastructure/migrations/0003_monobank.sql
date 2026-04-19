CREATE TABLE monobank_connections (
    id                  UUID    PRIMARY KEY NOT NULL,
    account_id          UUID    NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    user_id             UUID    NOT NULL,
    token               TEXT    NOT NULL,
    monobank_account_id TEXT    NOT NULL,
    sync_status         TEXT    NOT NULL DEFAULT 'pending',
    last_synced_at      BIGINT,
    created_at          BIGINT  NOT NULL
);

CREATE INDEX idx_monobank_connections_user_id    ON monobank_connections(user_id);
CREATE INDEX idx_monobank_connections_account_id ON monobank_connections(account_id);
CREATE UNIQUE INDEX idx_monobank_connections_monobank_account_id
    ON monobank_connections(monobank_account_id);
