-- Durable Monobank validation, webhook, and statement worker state.  This is
-- additive so the previous Finance V2 binary can still read the database.

ALTER TABLE banking.provider_connections
    ADD COLUMN validation_state TEXT DEFAULT 'pending',
    ADD COLUMN validation_attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN validation_next_retry_at TIMESTAMPTZ,
    ADD COLUMN validation_last_error TEXT,
    ADD COLUMN validation_lease_holder TEXT,
    ADD COLUMN validation_lease_token BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN validation_lease_expires_at TIMESTAMPTZ,
    ADD COLUMN webhook_lease_holder TEXT,
    ADD COLUMN webhook_lease_token BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN webhook_lease_expires_at TIMESTAMPTZ,
    ADD COLUMN statement_next_request_at TIMESTAMPTZ;

UPDATE banking.provider_connections
SET validation_state = CASE state
    WHEN 'pending' THEN 'pending'
    WHEN 'pending_credential_validation' THEN 'pending'
    WHEN 'active' THEN 'succeeded'
    WHEN 'needs_reauth' THEN 'failed'
    WHEN 'revoked' THEN 'disabled'
END;

ALTER TABLE banking.provider_connections
    ALTER COLUMN validation_state SET NOT NULL,
    ADD CONSTRAINT provider_connection_validation_state CHECK (
        validation_state IN ('pending','running','retry_due','succeeded','failed','disabled')
    ),
    ADD CONSTRAINT provider_connection_validation_attempts CHECK (validation_attempts >= 0),
    ADD CONSTRAINT provider_connection_validation_lease_token CHECK (validation_lease_token >= 0),
    ADD CONSTRAINT provider_connection_webhook_lease_token CHECK (webhook_lease_token >= 0),
    ADD CONSTRAINT provider_connection_validation_error CHECK (
        validation_last_error IS NULL OR char_length(validation_last_error) <= 500
    ),
    ADD CONSTRAINT provider_connection_validation_lease CHECK (
        (validation_lease_holder IS NULL) = (validation_lease_expires_at IS NULL)
        AND (validation_lease_holder IS NULL OR (
            validation_lease_holder = btrim(validation_lease_holder)
            AND validation_lease_holder <> ''
            AND char_length(validation_lease_holder) <= 200
        ))
    ),
    ADD CONSTRAINT provider_connection_webhook_lease CHECK (
        (webhook_lease_holder IS NULL) = (webhook_lease_expires_at IS NULL)
        AND (webhook_lease_holder IS NULL OR (
            webhook_lease_holder = btrim(webhook_lease_holder)
            AND webhook_lease_holder <> ''
            AND char_length(webhook_lease_holder) <= 200
        ))
    );

CREATE INDEX banking_connections_validation_due
    ON banking.provider_connections (validation_next_retry_at, created_at, id)
    WHERE validation_state IN ('pending','retry_due','running');

CREATE INDEX banking_connections_webhook_due
    ON banking.provider_connections (webhook_next_retry_at, updated_at, id)
    WHERE webhook_registration_state IN ('pending','retry_due');

ALTER TABLE banking.webhook_receipts
    ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN provenance_generation BIGINT,
    ADD COLUMN next_retry_at TIMESTAMPTZ,
    ADD COLUMN last_error TEXT,
    ADD COLUMN lease_holder TEXT,
    ADD COLUMN lease_token BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN lease_expires_at TIMESTAMPTZ,
    ADD CONSTRAINT webhook_receipt_attempts CHECK (attempts >= 0),
    ADD CONSTRAINT webhook_receipt_lease_token CHECK (lease_token >= 0),
    ADD CONSTRAINT webhook_receipt_provenance_generation CHECK (
        provenance_generation IS NULL OR provenance_generation >= 1
    ),
    ADD CONSTRAINT webhook_receipt_error CHECK (
        last_error IS NULL OR char_length(last_error) <= 500
    ),
    ADD CONSTRAINT webhook_receipt_lease CHECK (
        (lease_holder IS NULL) = (lease_expires_at IS NULL)
        AND (lease_holder IS NULL OR (
            lease_holder = btrim(lease_holder)
            AND lease_holder <> ''
            AND char_length(lease_holder) <= 200
        ))
    ),
    ADD CONSTRAINT webhook_receipt_provenance_complete CHECK (
        num_nonnulls(provenance_ciphertext, provenance_nonce,
            provenance_key_id, provenance_envelope_version, provenance_generation) = 0
        OR (num_nonnulls(provenance_ciphertext, provenance_nonce,
            provenance_key_id, provenance_envelope_version, provenance_generation) = 5
            AND octet_length(provenance_ciphertext) > 0
            AND octet_length(provenance_nonce) >= 12
            AND provenance_key_id <> ''
            AND provenance_envelope_version >= 1)
    );

-- Receipts created by the previous binary retained only a digest, so their
-- payload cannot be replayed safely after upgrade.
UPDATE banking.webhook_receipts
SET state='quarantined', processed_at=clock_timestamp(),
    last_error='webhook payload unavailable after worker upgrade'
WHERE state='pending' AND provenance_ciphertext IS NULL;

CREATE INDEX banking_webhook_receipts_due
    ON banking.webhook_receipts (next_retry_at, received_at, id)
    WHERE state='pending';

ALTER TABLE banking.provider_event_processes
    ADD COLUMN lease_holder TEXT,
    ADD COLUMN lease_token BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN lease_expires_at TIMESTAMPTZ,
    ADD CONSTRAINT provider_event_process_lease_token CHECK (lease_token >= 0),
    ADD CONSTRAINT provider_event_process_lease CHECK (
        (lease_holder IS NULL) = (lease_expires_at IS NULL)
        AND (lease_holder IS NULL OR (
            lease_holder = btrim(lease_holder) AND lease_holder <> ''
            AND char_length(lease_holder) <= 200
        ))
    );

ALTER TABLE banking.balance_observation_deliveries
    ADD COLUMN lease_holder TEXT,
    ADD COLUMN lease_token BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN lease_expires_at TIMESTAMPTZ,
    ADD CONSTRAINT balance_observation_delivery_lease_token CHECK (lease_token >= 0),
    ADD CONSTRAINT balance_observation_delivery_lease CHECK (
        (lease_holder IS NULL) = (lease_expires_at IS NULL)
        AND (lease_holder IS NULL OR (
            lease_holder = btrim(lease_holder) AND lease_holder <> ''
            AND char_length(lease_holder) <= 200
        ))
    );

-- Freeze the set and order of resources covered by a requested synchronization.
CREATE TABLE banking.sync_job_resources (
    sync_job_id UUID NOT NULL,
    user_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    external_resource_id UUID NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 1),
    snapshot_from TIMESTAMPTZ NOT NULL,
    snapshot_to TIMESTAMPTZ NOT NULL,
    next_from TIMESTAMPTZ NOT NULL,
    state TEXT NOT NULL DEFAULT 'requested' CHECK (state IN ('requested','completed')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (sync_job_id, user_id, external_resource_id),
    UNIQUE (sync_job_id, user_id, position),
    FOREIGN KEY (sync_job_id, user_id, connection_id)
        REFERENCES banking.sync_jobs (id, user_id, connection_id),
    FOREIGN KEY (external_resource_id, user_id, connection_id)
        REFERENCES banking.external_resources (id, user_id, connection_id),
    CHECK (snapshot_from <= next_from AND next_from <= snapshot_to)
);

CREATE INDEX banking_sync_job_resources_next
    ON banking.sync_job_resources (sync_job_id, user_id, state, position);

INSERT INTO banking.sync_job_resources
    (sync_job_id,user_id,connection_id,external_resource_id,position,
     snapshot_from,snapshot_to,next_from)
SELECT job.id,job.user_id,job.connection_id,resource.id,
       row_number() OVER (PARTITION BY job.id ORDER BY resource.created_at,resource.id)::integer,
       job.requested_from-(job.overlap_seconds::bigint*interval '1 second'),
       job.requested_to,
       job.requested_from-(job.overlap_seconds::bigint*interval '1 second')
FROM banking.sync_jobs job
JOIN banking.external_resources resource
  ON resource.user_id=job.user_id AND resource.connection_id=job.connection_id
WHERE job.state IN ('requested','running','retry_due')
  AND resource.kind IN ('card','current_account','jar')
  AND resource.discovery_state IN ('active','needs_review');

CREATE FUNCTION banking.reject_sync_job_resource_snapshot_mutation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'Banking sync resource snapshots are immutable';
    END IF;
    IF OLD.sync_job_id IS DISTINCT FROM NEW.sync_job_id
       OR OLD.user_id IS DISTINCT FROM NEW.user_id
       OR OLD.connection_id IS DISTINCT FROM NEW.connection_id
       OR OLD.external_resource_id IS DISTINCT FROM NEW.external_resource_id
       OR OLD.position IS DISTINCT FROM NEW.position
       OR OLD.snapshot_from IS DISTINCT FROM NEW.snapshot_from
       OR OLD.snapshot_to IS DISTINCT FROM NEW.snapshot_to THEN
        RAISE EXCEPTION 'Banking sync resource snapshots are immutable';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER sync_job_resource_snapshots_are_immutable
BEFORE UPDATE OR DELETE ON banking.sync_job_resources
FOR EACH ROW EXECUTE FUNCTION banking.reject_sync_job_resource_snapshot_mutation();

ALTER TABLE banking.sync_pages
    ADD COLUMN external_resource_id UUID,
    ADD COLUMN window_from TIMESTAMPTZ,
    ADD COLUMN window_to TIMESTAMPTZ,
    ADD CONSTRAINT sync_page_statement_identity
        UNIQUE (id, user_id, sync_job_id),
    ADD CONSTRAINT sync_page_statement_window CHECK (
        num_nonnulls(external_resource_id, window_from, window_to) IN (0, 3)
        AND (window_from IS NULL OR window_from <= window_to)
    ),
    ADD CONSTRAINT sync_page_statement_resource_fk
        FOREIGN KEY (external_resource_id, user_id, connection_id)
        REFERENCES banking.external_resources (id, user_id, connection_id);

-- A page waits for exactly the provider revisions observed in that response.
CREATE TABLE banking.sync_page_events (
    sync_page_id UUID NOT NULL,
    sync_job_id UUID NOT NULL,
    user_id UUID NOT NULL,
    provider_event_id UUID NOT NULL,
    intake_outcome TEXT NOT NULL CHECK (intake_outcome IN ('new','duplicate','conflicting_content')),
    PRIMARY KEY (sync_page_id, user_id, provider_event_id),
    FOREIGN KEY (sync_page_id, user_id, sync_job_id)
        REFERENCES banking.sync_pages (id, user_id, sync_job_id),
    FOREIGN KEY (provider_event_id, user_id)
        REFERENCES banking.provider_events (id, user_id)
);

CREATE INDEX banking_sync_page_events_by_job
    ON banking.sync_page_events (sync_job_id, user_id, sync_page_id);
