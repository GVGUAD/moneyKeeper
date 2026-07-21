-- no-transaction
CREATE UNIQUE INDEX CONCURRENTLY subscription_charges_source_key_unique
    ON subscription_charges (source_key);
