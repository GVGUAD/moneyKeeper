-- no-transaction
-- Partial unique indexes on populated production tables must be built without
-- blocking ordinary writes. Migration 0016 provides the actionable duplicate
-- diagnostic before this build starts.
CREATE UNIQUE INDEX CONCURRENTLY subscription_charges_transaction_unique
    ON subscription_charges (transaction_id)
    WHERE transaction_id IS NOT NULL;
