ALTER TABLE monobank_connections RENAME TO bank_connections;
ALTER TABLE bank_connections RENAME COLUMN monobank_account_id TO external_account_id;
ALTER TABLE bank_connections ADD COLUMN provider TEXT NOT NULL DEFAULT 'monobank';

ALTER INDEX idx_monobank_connections_user_id RENAME TO idx_bank_connections_user_id;
ALTER INDEX idx_monobank_connections_account_id RENAME TO idx_bank_connections_account_id;
ALTER INDEX idx_monobank_connections_monobank_account_id RENAME TO idx_bank_connections_external_account_id;
