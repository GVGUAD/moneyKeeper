-- Fence refresh-token rotation against concurrent OAuth reconnects. A claimed
-- worker may finish reading with old credentials, but it cannot overwrite a
-- newer browser reconnect because every credential write is compare-and-swap.
ALTER TABLE email_connections
    ADD COLUMN credential_version BIGINT NOT NULL DEFAULT 0;
