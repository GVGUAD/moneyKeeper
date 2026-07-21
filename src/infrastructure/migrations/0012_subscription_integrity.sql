-- Expand the subscription schema without scanning the existing tables. The
-- data backfill, constraint validation, and concurrent indexes are deliberately
-- split into later migrations so this transaction only holds its catalog locks
-- for short metadata changes.

ALTER TABLE subscription_charges
    ADD COLUMN source TEXT NOT NULL DEFAULT 'gmail',
    ADD COLUMN source_key TEXT,
    ADD COLUMN source_connection_id UUID,
    ADD COLUMN provider_message_id TEXT,
    ADD COLUMN rfc_message_id TEXT,
    ADD COLUMN match_started_at BIGINT,
    ADD COLUMN match_source TEXT;

-- Keep the expand migration compatible with replicas still running migration
-- 0011-era code. Those writers only supply email_message_id and never supply
-- source_key, match_started_at, or match_source. The trigger normalizes both
-- legacy and current writes before the new NOT VALID constraints are checked.
CREATE FUNCTION normalize_subscription_charge_integrity()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
DECLARE
    transaction_link_changed BOOLEAN;
BEGIN
    transaction_link_changed := TG_OP = 'INSERT';
    IF TG_OP = 'UPDATE' THEN
        transaction_link_changed := NEW.transaction_id IS DISTINCT FROM OLD.transaction_id;
    END IF;

    IF NEW.source_key IS NULL OR btrim(NEW.source_key) = '' THEN
        NEW.rfc_message_id := COALESCE(NEW.rfc_message_id, NEW.email_message_id);
        NEW.source_key := 'legacy:' || NEW.user_id::text || ':' || NEW.email_message_id;
    END IF;

    NEW.email_message_id := NEW.source_key;
    NEW.match_started_at := COALESCE(
        NEW.match_started_at,
        NEW.created_at,
        EXTRACT(EPOCH FROM NOW())::BIGINT
    );

    IF NEW.transaction_id IS NOT NULL AND transaction_link_changed THEN
        -- Serialize links for a transaction until the concurrent unique index
        -- is installed. This closes the preflight/index-build race and gives
        -- old writers the same one-charge reservation semantics.
        PERFORM pg_advisory_xact_lock(
            hashtextextended('subscription-charge-transaction:' || NEW.transaction_id::text, 0)
        );
        IF EXISTS (
            SELECT 1
            FROM subscription_charges existing
            WHERE existing.transaction_id = NEW.transaction_id
              AND existing.id <> NEW.id
        ) THEN
            RAISE EXCEPTION USING
                ERRCODE = 'unique_violation',
                MESSAGE = format(
                    'transaction %s is already linked to another subscription charge',
                    NEW.transaction_id
                ),
                CONSTRAINT = 'subscription_charges_transaction_unique';
        END IF;

    END IF;

    IF NEW.transaction_id IS NOT NULL THEN
        NEW.match_status := 'Matched';
        NEW.match_source := COALESCE(NEW.match_source, 'automatic');
    ELSIF NEW.match_status = 'Matched' THEN
        -- Migration 0011 used ON DELETE SET NULL and could leave this legacy
        -- state behind. A fresh Pending charge can safely be matched again.
        NEW.match_status := 'Pending';
        NEW.match_source := NULL;
    ELSE
        NEW.match_source := NULL;
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER subscription_charges_normalize_integrity
BEFORE INSERT OR UPDATE ON subscription_charges
FOR EACH ROW
EXECUTE FUNCTION normalize_subscription_charge_integrity();

-- NOT VALID avoids scanning the existing table while still enforcing these
-- invariants for every new or updated row. Migration 0017 validates them after
-- the row-level backfill in migration 0016.
ALTER TABLE subscription_charges
    ADD CONSTRAINT subscription_charges_source_key_not_null_check
        CHECK (source_key IS NOT NULL) NOT VALID,
    ADD CONSTRAINT subscription_charges_match_started_at_not_null_check
        CHECK (match_started_at IS NOT NULL) NOT VALID,
    ADD CONSTRAINT subscription_charges_source_check
        CHECK (source IN ('gmail', 'manual', 'other')) NOT VALID,
    ADD CONSTRAINT subscription_charges_match_source_check
        CHECK (match_source IS NULL OR match_source IN ('automatic', 'manual')) NOT VALID,
    ADD CONSTRAINT subscription_charges_internal_message_key_check
        CHECK (email_message_id = source_key) NOT VALID,
    ADD CONSTRAINT subscription_charges_source_connection_id_fkey
        FOREIGN KEY (source_connection_id) REFERENCES email_connections(id)
        ON DELETE SET NULL NOT VALID,
    ADD CONSTRAINT subscription_charges_match_state_check
        CHECK (
            (
                match_status = 'Matched'
                AND transaction_id IS NOT NULL
                AND match_source IS NOT NULL
            )
            OR (
                match_status IN ('Pending', 'Unmatched')
                AND transaction_id IS NULL
                AND match_source IS NULL
            )
        ) NOT VALID;

CREATE TABLE subscription_charge_match_rejections (
    id UUID PRIMARY KEY,
    charge_id UUID NOT NULL REFERENCES subscription_charges(id) ON DELETE CASCADE,
    transaction_id UUID NOT NULL REFERENCES transactions(id) ON DELETE CASCADE,
    user_id UUID NOT NULL,
    reason TEXT NOT NULL,
    created_at BIGINT NOT NULL,

    CONSTRAINT subscription_charge_rejections_unique
        UNIQUE (charge_id, transaction_id)
);

CREATE INDEX subscription_charge_rejections_user_idx
    ON subscription_charge_match_rejections (user_id, created_at);

-- Keep charge state internally consistent when a linked transaction is
-- deleted. A fresh matching window allows another bank transaction to match.
CREATE FUNCTION reset_subscription_charge_match_on_transaction_delete()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    UPDATE subscription_charges
    SET transaction_id = NULL,
        match_status = 'Pending',
        match_source = NULL,
        match_started_at = EXTRACT(EPOCH FROM NOW())::BIGINT
    WHERE transaction_id = OLD.id;
    RETURN OLD;
END;
$$;

CREATE TRIGGER transactions_reset_subscription_charge_match
BEFORE DELETE ON transactions
FOR EACH ROW
EXECUTE FUNCTION reset_subscription_charge_match_on_transaction_delete();

ALTER TABLE subscriptions
    ADD COLUMN product_name_override TEXT,
    ADD COLUMN billing_period_override TEXT,
    ADD COLUMN status_override TEXT,
    ADD COLUMN last_receipt_at BIGINT,
    ADD CONSTRAINT subscriptions_billing_period_override_check
        CHECK (
            billing_period_override IS NULL
            OR billing_period_override IN ('weekly', 'monthly', 'yearly')
        ) NOT VALID,
    ADD CONSTRAINT subscriptions_status_override_check
        CHECK (status_override IS NULL OR status_override IN ('active', 'inactive')) NOT VALID,
    ADD CONSTRAINT subscriptions_last_receipt_at_not_null_check
        CHECK (last_receipt_at IS NOT NULL) NOT VALID,
    ADD CONSTRAINT subscriptions_category_id_fkey
        FOREIGN KEY (category_id) REFERENCES categories(id) ON DELETE SET NULL NOT VALID;

-- Old application replicas omit last_receipt_at. Populate it before constraint
-- checks so their inserts and updates remain valid during the rolling deploy.
CREATE FUNCTION normalize_subscription_receipt_timestamp()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.last_receipt_at := GREATEST(
        COALESCE(NEW.last_receipt_at, NEW.started_at, NEW.created_at),
        COALESCE(NEW.last_charged_at, NEW.started_at, NEW.created_at)
    );
    RETURN NEW;
END;
$$;

CREATE TRIGGER subscriptions_normalize_receipt_timestamp
BEFORE INSERT OR UPDATE ON subscriptions
FOR EACH ROW
EXECUTE FUNCTION normalize_subscription_receipt_timestamp();

CREATE TABLE subscription_tombstones (
    user_id UUID NOT NULL,
    provider TEXT NOT NULL,
    merchant_key TEXT NOT NULL,
    deleted_at BIGINT NOT NULL,

    CONSTRAINT subscription_tombstones_pkey
        PRIMARY KEY (user_id, provider, merchant_key)
);

CREATE INDEX subscription_tombstones_deleted_idx
    ON subscription_tombstones (deleted_at);
