-- Durable scheduling, leases, and body-free per-message ingestion outcomes.
ALTER TABLE email_connections
    ADD COLUMN next_sync_at BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN sync_attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN sync_last_error_kind TEXT,
    ADD COLUMN sync_lease_owner UUID,
    ADD COLUMN sync_lease_expires_at BIGINT;

CREATE INDEX email_connections_due_sync_idx
    ON email_connections (next_sync_at, created_at)
    WHERE status = 'connected';

CREATE INDEX email_connections_active_lease_idx
    ON email_connections (sync_lease_expires_at)
    WHERE sync_lease_owner IS NOT NULL;

CREATE TABLE email_message_ingestions (
    id UUID PRIMARY KEY,
    connection_id UUID NOT NULL REFERENCES email_connections(id) ON DELETE CASCADE,
    user_id UUID NOT NULL,
    provider_message_id TEXT NOT NULL,
    rfc_message_id TEXT,
    outcome TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    error_kind TEXT,
    next_retry_at BIGINT,
    received_at BIGINT NOT NULL,
    processed_at BIGINT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,

    CONSTRAINT email_message_ingestions_outcome_check
        CHECK (outcome IN ('processed', 'ignored', 'failed', 'dead_letter')),
    CONSTRAINT email_message_ingestions_connection_message_unique
        UNIQUE (connection_id, provider_message_id)
);

CREATE INDEX email_message_ingestions_retry_idx
    ON email_message_ingestions (connection_id, next_retry_at)
    WHERE outcome = 'failed';

CREATE INDEX email_message_ingestions_dead_letter_idx
    ON email_message_ingestions (connection_id, updated_at)
    WHERE outcome = 'dead_letter';

CREATE INDEX email_message_ingestions_user_received_idx
    ON email_message_ingestions (user_id, received_at DESC);
