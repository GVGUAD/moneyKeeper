-- Running account balance as reported by the external provider (e.g. Monobank)
-- immediately AFTER this transaction. NULL for manually-entered transactions.
ALTER TABLE transactions ADD COLUMN external_balance NUMERIC;
