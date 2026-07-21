-- Preflight normalized mailbox collisions before touching legacy rows. New and
-- concurrent legacy-writer inserts are already normalized and serialized by
-- the migration 0013 trigger, so this diagnostic cannot race a new duplicate.
DO $$
DECLARE
    duplicate_count BIGINT;
    example_user_id UUID;
    example_provider TEXT;
    example_address TEXT;
BEGIN
    SELECT COUNT(*) INTO duplicate_count
    FROM (
        SELECT user_id, provider, lower(btrim(email_address)) AS normalized_address
        FROM email_connections
        GROUP BY user_id, provider, lower(btrim(email_address))
        HAVING COUNT(*) > 1
    ) duplicates;

    IF duplicate_count > 0 THEN
        SELECT user_id, provider, lower(btrim(email_address))
        INTO example_user_id, example_provider, example_address
        FROM email_connections
        GROUP BY user_id, provider, lower(btrim(email_address))
        HAVING COUNT(*) > 1
        ORDER BY user_id, provider, lower(btrim(email_address))
        LIMIT 1;

        RAISE EXCEPTION
            'cannot normalize email connections: % duplicate normalized mailbox identity row(s); example user_id=% provider=% normalized_address=%',
            duplicate_count,
            example_user_id,
            example_provider,
            example_address
            USING HINT = 'Merge or remove duplicate rows grouped by user_id, provider, and lower(btrim(email_address)), then retry migration 0023.';
    END IF;
END;
$$;

-- This takes row locks only. The normalization constraint is NOT VALID until
-- migration 0024, so no ACCESS EXCLUSIVE lock is retained across this scan.
UPDATE email_connections
SET email_address = lower(btrim(email_address))
WHERE email_address IS DISTINCT FROM lower(btrim(email_address));
