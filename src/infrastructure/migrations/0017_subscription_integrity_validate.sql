-- Validation uses SHARE UPDATE EXCLUSIVE locks, which continue to permit
-- ordinary reads and writes. The validated not-null checks let PostgreSQL set
-- attnotnull without another heap scan; ACCESS EXCLUSIVE is held only for the
-- final short catalog updates at the end of this migration.

ALTER TABLE subscription_charges
    VALIDATE CONSTRAINT subscription_charges_source_key_not_null_check;
ALTER TABLE subscription_charges
    VALIDATE CONSTRAINT subscription_charges_match_started_at_not_null_check;
ALTER TABLE subscription_charges
    VALIDATE CONSTRAINT subscription_charges_source_check;
ALTER TABLE subscription_charges
    VALIDATE CONSTRAINT subscription_charges_match_source_check;
ALTER TABLE subscription_charges
    VALIDATE CONSTRAINT subscription_charges_internal_message_key_check;
ALTER TABLE subscription_charges
    VALIDATE CONSTRAINT subscription_charges_source_connection_id_fkey;
ALTER TABLE subscription_charges
    VALIDATE CONSTRAINT subscription_charges_match_state_check;

ALTER TABLE subscriptions
    VALIDATE CONSTRAINT subscriptions_billing_period_override_check;
ALTER TABLE subscriptions
    VALIDATE CONSTRAINT subscriptions_status_override_check;
ALTER TABLE subscriptions
    VALIDATE CONSTRAINT subscriptions_last_receipt_at_not_null_check;
ALTER TABLE subscriptions
    VALIDATE CONSTRAINT subscriptions_category_id_fkey;

ALTER TABLE subscription_charges
    ALTER COLUMN source_key SET NOT NULL,
    ALTER COLUMN match_started_at SET NOT NULL;

ALTER TABLE subscriptions
    ALTER COLUMN last_receipt_at SET NOT NULL;
