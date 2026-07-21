-- Backfill row data separately from schema changes. These updates take row
-- locks but do not retain ACCESS EXCLUSIVE locks while scanning large tables.

-- Do not guess which historical charge owns a duplicated transaction link.
-- Run this before the UPDATE so the compatibility trigger cannot replace this
-- actionable diagnostic with a row-level unique-violation error.
DO $$
DECLARE
    duplicate_count BIGINT;
    example_transaction_id UUID;
BEGIN
    SELECT COUNT(*) INTO duplicate_count
    FROM (
        SELECT transaction_id
        FROM subscription_charges
        WHERE transaction_id IS NOT NULL
        GROUP BY transaction_id
        HAVING COUNT(*) > 1
    ) duplicates;

    IF duplicate_count > 0 THEN
        SELECT transaction_id INTO example_transaction_id
        FROM subscription_charges
        WHERE transaction_id IS NOT NULL
        GROUP BY transaction_id
        HAVING COUNT(*) > 1
        ORDER BY transaction_id
        LIMIT 1;

        RAISE EXCEPTION
            'cannot enforce one charge per transaction: % duplicate transaction link(s); example transaction_id=%',
            duplicate_count,
            example_transaction_id
            USING HINT = 'Unlink all but the intended subscription charge for each duplicated transaction, then retry migration 0016.';
    END IF;
END;
$$;

-- The normalization trigger performs the same conversion for 0011-era writes
-- that race with this backfill.
UPDATE subscription_charges
SET source_key = COALESCE(
        source_key,
        'legacy:' || user_id::text || ':' || email_message_id
    ),
    rfc_message_id = CASE
        WHEN source_key IS NULL THEN COALESCE(rfc_message_id, email_message_id)
        ELSE rfc_message_id
    END,
    match_started_at = COALESCE(match_started_at, created_at),
    match_source = CASE
        WHEN transaction_id IS NOT NULL THEN COALESCE(match_source, 'automatic')
        ELSE NULL
    END,
    match_status = CASE
        WHEN transaction_id IS NOT NULL THEN 'Matched'
        WHEN match_status = 'Matched' THEN 'Pending'
        ELSE match_status
    END;

UPDATE subscriptions
SET last_receipt_at = COALESCE(last_receipt_at, last_charged_at, started_at, created_at)
WHERE last_receipt_at IS NULL;

-- Remove dangling and cross-user category references before validating the FK.
UPDATE subscriptions s
SET category_id = NULL
WHERE category_id IS NOT NULL
  AND NOT EXISTS (
      SELECT 1
      FROM categories c
      WHERE c.id = s.category_id
        AND c.user_id = s.user_id
  );
