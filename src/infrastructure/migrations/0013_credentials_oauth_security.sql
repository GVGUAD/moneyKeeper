-- Fast credential-encryption and OAuth-state expansion. Legacy plaintext
-- columns remain readable during the dual-read/dual-write deployment phase.
ALTER TABLE bank_connections
    ADD COLUMN token_encrypted TEXT;

ALTER TABLE email_connections
    ADD COLUMN oauth_access_token_encrypted TEXT,
    ADD COLUMN oauth_refresh_token_encrypted TEXT;

-- Keep replicas running the 0011-era email INSERT/UPDATE statements compatible
-- with normalized mailbox identity. The advisory lock plus duplicate lookup
-- closes the gap until migration 0025 installs the concurrent unique index.
CREATE FUNCTION normalize_email_connection_identity()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
DECLARE
    identity_changed BOOLEAN;
BEGIN
    NEW.email_address := lower(btrim(NEW.email_address));

    identity_changed := TG_OP = 'INSERT';
    IF TG_OP = 'UPDATE' THEN
        identity_changed := NEW.user_id IS DISTINCT FROM OLD.user_id
            OR NEW.provider IS DISTINCT FROM OLD.provider
            OR NEW.email_address IS DISTINCT FROM lower(btrim(OLD.email_address));
    END IF;

    IF identity_changed THEN
        PERFORM pg_advisory_xact_lock(
            hashtextextended(
                'email-connection:' || NEW.user_id::text || ':' || NEW.provider || ':' || NEW.email_address,
                0
            )
        );

        IF EXISTS (
            SELECT 1
            FROM email_connections existing
            WHERE existing.user_id = NEW.user_id
              AND existing.provider = NEW.provider
              AND lower(btrim(existing.email_address)) = NEW.email_address
              AND existing.id <> NEW.id
        ) THEN
            RAISE EXCEPTION USING
                ERRCODE = 'unique_violation',
                MESSAGE = format(
                    'email connection already exists for user=%s provider=%s normalized_address=%s',
                    NEW.user_id,
                    NEW.provider,
                    NEW.email_address
                ),
                HINT = 'Reconnect the existing mailbox or explicitly remove the duplicate connection.',
                CONSTRAINT = 'email_connections_user_provider_address_unique';
        END IF;
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER email_connections_normalize_identity
BEFORE INSERT OR UPDATE ON email_connections
FOR EACH ROW
EXECUTE FUNCTION normalize_email_connection_identity();

-- Enforce normalization for new and updated rows immediately without scanning
-- legacy data while this expand transaction holds its short catalog locks.
ALTER TABLE email_connections
    ADD CONSTRAINT email_connections_normalized_address_check
        CHECK (email_address = lower(btrim(email_address))) NOT VALID;

-- OAuth state values are never stored directly. PKCE verifiers are credentials
-- too, so they use the same versioned encryption envelope as long-lived tokens.
CREATE TABLE gmail_oauth_states (
    state_hash BYTEA PRIMARY KEY,
    user_id UUID NOT NULL,
    pkce_verifier_encrypted TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX gmail_oauth_states_expiry_idx
    ON gmail_oauth_states (expires_at)
    WHERE consumed_at IS NULL;
