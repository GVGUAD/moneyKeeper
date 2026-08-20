-- Provider-neutral Banking and the Monobank anti-corruption boundary.

CREATE TABLE banking.provider_connections (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    provider TEXT NOT NULL CHECK (
        provider = btrim(provider) AND provider <> '' AND char_length(provider) <= 100
    ),
    state TEXT NOT NULL CHECK (state IN (
        'pending', 'active', 'pending_credential_validation', 'needs_reauth', 'revoked'
    )),
    active_credential_ciphertext BYTEA,
    active_credential_nonce BYTEA,
    active_credential_key_id TEXT,
    active_credential_envelope_version SMALLINT CHECK (active_credential_envelope_version >= 1),
    pending_credential_ciphertext BYTEA,
    pending_credential_nonce BYTEA,
    pending_credential_key_id TEXT,
    pending_credential_envelope_version SMALLINT CHECK (pending_credential_envelope_version >= 1),
    credential_generation BIGINT NOT NULL DEFAULT 1 CHECK (credential_generation >= 1),
    webhook_credential_ciphertext BYTEA,
    webhook_credential_nonce BYTEA,
    webhook_credential_key_id TEXT,
    webhook_credential_envelope_version SMALLINT CHECK (webhook_credential_envelope_version >= 1),
    webhook_lookup_digest BYTEA UNIQUE CHECK (
        webhook_lookup_digest IS NULL OR octet_length(webhook_lookup_digest) = 32
    ),
    webhook_desired_version BIGINT CHECK (webhook_desired_version >= 1),
    webhook_registered_version BIGINT CHECK (webhook_registered_version >= 1),
    webhook_registration_state TEXT NOT NULL DEFAULT 'not_requested' CHECK (
        webhook_registration_state IN ('not_requested', 'pending', 'registered', 'retry_due', 'failed', 'disabled')
    ),
    webhook_registration_attempts INTEGER NOT NULL DEFAULT 0 CHECK (webhook_registration_attempts >= 0),
    webhook_next_retry_at TIMESTAMPTZ,
    webhook_last_error TEXT CHECK (webhook_last_error IS NULL OR char_length(webhook_last_error) <= 500),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    revoked_at TIMESTAMPTZ,
    PRIMARY KEY (id, user_id),
    CONSTRAINT provider_connection_active_credential_complete CHECK (
        num_nonnulls(active_credential_ciphertext, active_credential_nonce,
            active_credential_key_id, active_credential_envelope_version) = 0
        OR
        (num_nonnulls(active_credential_ciphertext, active_credential_nonce,
            active_credential_key_id, active_credential_envelope_version) = 4
            AND octet_length(active_credential_ciphertext) > 0 AND octet_length(active_credential_nonce) >= 12
            AND active_credential_key_id <> '' AND active_credential_envelope_version >= 1)
    ),
    CONSTRAINT provider_connection_pending_credential_complete CHECK (
        num_nonnulls(pending_credential_ciphertext, pending_credential_nonce,
            pending_credential_key_id, pending_credential_envelope_version) = 0
        OR
        (num_nonnulls(pending_credential_ciphertext, pending_credential_nonce,
            pending_credential_key_id, pending_credential_envelope_version) = 4
            AND octet_length(pending_credential_ciphertext) > 0 AND octet_length(pending_credential_nonce) >= 12
            AND pending_credential_key_id <> '' AND pending_credential_envelope_version >= 1)
    ),
    CONSTRAINT provider_connection_webhook_credential_complete CHECK (
        num_nonnulls(webhook_credential_ciphertext, webhook_credential_nonce,
            webhook_credential_key_id, webhook_credential_envelope_version,
            webhook_lookup_digest) = 0
        OR
        (num_nonnulls(webhook_credential_ciphertext, webhook_credential_nonce,
            webhook_credential_key_id, webhook_credential_envelope_version,
            webhook_lookup_digest) = 5
            AND octet_length(webhook_credential_ciphertext) > 0 AND octet_length(webhook_credential_nonce) >= 12
            AND webhook_credential_key_id <> '' AND webhook_credential_envelope_version >= 1
            AND octet_length(webhook_lookup_digest) = 32)
    ),
    CONSTRAINT revoked_connection_has_no_usable_credentials CHECK (
        state <> 'revoked'
        OR (active_credential_ciphertext IS NULL AND pending_credential_ciphertext IS NULL
            AND webhook_credential_ciphertext IS NULL)
    )
);

CREATE INDEX banking_connections_by_user
    ON banking.provider_connections (user_id, state, created_at DESC, id);
CREATE INDEX banking_connections_webhook_retry
    ON banking.provider_connections (webhook_next_retry_at, id)
    WHERE webhook_registration_state = 'retry_due';

CREATE TABLE banking.external_resources (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    external_resource_id TEXT NOT NULL CHECK (
        external_resource_id = btrim(external_resource_id)
        AND external_resource_id <> '' AND char_length(external_resource_id) <= 200
    ),
    kind TEXT NOT NULL CHECK (kind IN (
        'card', 'current_account', 'jar', 'security_portfolio', 'unsupported'
    )),
    funding_model TEXT NOT NULL CHECK (funding_model IN (
        'own_funds', 'revolving_credit', 'unknown'
    )),
    currency VARCHAR(3) NOT NULL CHECK (currency COLLATE "C" ~ '^[A-Z]{3}$'),
    masked_label TEXT NOT NULL CHECK (
        masked_label = btrim(masked_label) AND masked_label <> '' AND char_length(masked_label) <= 200
    ),
    masked_pan TEXT CHECK (masked_pan IS NULL OR char_length(masked_pan) <= 32),
    masked_iban TEXT CHECK (masked_iban IS NULL OR char_length(masked_iban) <= 64),
    credit_limit NUMERIC(28,8),
    discovery_state TEXT NOT NULL DEFAULT 'active' CHECK (
        discovery_state IN ('active', 'needs_review', 'unsupported', 'removed')
    ),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id),
    UNIQUE (id, user_id, connection_id),
    UNIQUE (connection_id, external_resource_id),
    CONSTRAINT external_resource_connection_fk
        FOREIGN KEY (connection_id, user_id)
        REFERENCES banking.provider_connections (id, user_id)
);

CREATE INDEX banking_resources_by_connection
    ON banking.external_resources (user_id, connection_id, discovery_state, kind, id);

CREATE TABLE banking.resource_mappings (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    external_resource_id UUID NOT NULL,
    ledger_account_id UUID,
    mapping_version BIGINT NOT NULL CHECK (mapping_version >= 1),
    effective_provider_revision BIGINT NOT NULL DEFAULT 1 CHECK (effective_provider_revision >= 1),
    state TEXT NOT NULL CHECK (state IN (
        'pending_account_creation', 'active', 'inactive', 'needs_review', 'failed'
    )),
    process_correlation_id UUID,
    reason TEXT CHECK (reason IS NULL OR (reason = btrim(reason) AND reason <> '' AND char_length(reason) <= 500)),
    effective_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id),
    UNIQUE (external_resource_id, user_id, mapping_version),
    CONSTRAINT resource_mapping_resource_fk
        FOREIGN KEY (external_resource_id, user_id, connection_id)
        REFERENCES banking.external_resources (id, user_id, connection_id),
    CONSTRAINT resource_mapping_end_state CHECK (
        (state IN ('active', 'pending_account_creation', 'needs_review') AND ended_at IS NULL)
        OR (state IN ('inactive', 'failed'))
    )
);

CREATE UNIQUE INDEX banking_one_active_mapping_per_resource
    ON banking.resource_mappings (user_id, external_resource_id)
    WHERE state IN ('active', 'pending_account_creation', 'needs_review');
CREATE UNIQUE INDEX banking_one_active_mapping_per_ledger_account
    ON banking.resource_mappings (user_id, ledger_account_id)
    WHERE state = 'active';

CREATE TABLE banking.provider_events (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    external_resource_id UUID NOT NULL,
    external_event_id TEXT NOT NULL CHECK (
        external_event_id = btrim(external_event_id)
        AND external_event_id <> '' AND char_length(external_event_id) <= 200
    ),
    revision BIGINT NOT NULL CHECK (revision >= 1),
    transaction_state TEXT NOT NULL CHECK (transaction_state IN ('pending', 'settled', 'reversed')),
    original_amount NUMERIC(28,8),
    original_currency VARCHAR(3) CHECK (original_currency IS NULL OR original_currency COLLATE "C" ~ '^[A-Z]{3}$'),
    operation_amount NUMERIC(28,8) NOT NULL CHECK (operation_amount <> 0),
    operation_currency VARCHAR(3) NOT NULL CHECK (operation_currency COLLATE "C" ~ '^[A-Z]{3}$'),
    description TEXT NOT NULL CHECK (char_length(description) <= 500),
    merchant_mcc INTEGER CHECK (merchant_mcc BETWEEN 0 AND 9999),
    content_digest BYTEA NOT NULL CHECK (octet_length(content_digest) = 32),
    provenance_ciphertext BYTEA,
    provenance_nonce BYTEA,
    provenance_key_id TEXT,
    provenance_envelope_version SMALLINT CHECK (provenance_envelope_version >= 1),
    effective_at TIMESTAMPTZ NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id),
    UNIQUE (connection_id, external_resource_id, external_event_id, revision),
    CONSTRAINT provider_event_resource_fk
        FOREIGN KEY (external_resource_id, user_id, connection_id)
        REFERENCES banking.external_resources (id, user_id, connection_id),
    CONSTRAINT provider_event_time_order CHECK (effective_at <= recorded_at),
    CONSTRAINT provider_event_provenance_complete CHECK (
        (provenance_ciphertext IS NULL AND provenance_nonce IS NULL
            AND provenance_key_id IS NULL AND provenance_envelope_version IS NULL)
        OR
        (octet_length(provenance_ciphertext) > 0 AND octet_length(provenance_nonce) >= 12
            AND provenance_key_id <> '' AND provenance_envelope_version >= 1)
    )
);

CREATE TABLE banking.provider_event_processes (
    provider_event_id UUID NOT NULL,
    user_id UUID NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'ready', 'waiting_for_mapping', 'waiting_for_prior_revision', 'posting',
        'posted', 'no_financial_change', 'retry_due', 'quarantined', 'terminal_failure'
    )),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_retry_at TIMESTAMPTZ,
    last_error TEXT CHECK (last_error IS NULL OR char_length(last_error) <= 500),
    ledger_journal_entry_id UUID,
    process_version BIGINT NOT NULL DEFAULT 1 CHECK (process_version >= 1),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (provider_event_id, user_id),
    FOREIGN KEY (provider_event_id, user_id)
        REFERENCES banking.provider_events (id, user_id)
);

CREATE TABLE banking.provider_event_conflicts (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    provider_event_id UUID NOT NULL,
    conflicting_digest BYTEA NOT NULL CHECK (octet_length(conflicting_digest) = 32),
    reason TEXT NOT NULL CHECK (reason <> '' AND char_length(reason) <= 500),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id),
    UNIQUE (provider_event_id, conflicting_digest),
    FOREIGN KEY (provider_event_id, user_id)
        REFERENCES banking.provider_events (id, user_id)
);

CREATE INDEX banking_events_ready
    ON banking.provider_event_processes (next_retry_at, provider_event_id)
    WHERE state IN ('ready', 'retry_due');
CREATE INDEX banking_events_chronological
    ON banking.provider_events (user_id, external_resource_id, external_event_id, revision, recorded_at, id);

CREATE TABLE banking.balance_observations (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    external_resource_id UUID NOT NULL,
    source_sequence BIGINT NOT NULL CHECK (source_sequence >= 1),
    basis TEXT NOT NULL CHECK (basis IN ('reported', 'available', 'credit_limit', 'statement_running')),
    provider_amount NUMERIC(28,8) NOT NULL,
    provider_currency VARCHAR(3) NOT NULL CHECK (provider_currency COLLATE "C" ~ '^[A-Z]{3}$'),
    sign_semantics TEXT NOT NULL CHECK (
        sign_semantics = btrim(sign_semantics) AND sign_semantics <> '' AND char_length(sign_semantics) <= 100
    ),
    comparable_amount NUMERIC(28,8),
    comparable_currency VARCHAR(3) CHECK (comparable_currency IS NULL OR comparable_currency COLLATE "C" ~ '^[A-Z]{3}$'),
    non_comparable_reason TEXT CHECK (
        non_comparable_reason IS NULL OR (non_comparable_reason = btrim(non_comparable_reason)
        AND non_comparable_reason <> '' AND char_length(non_comparable_reason) <= 500)
    ),
    observed_at TIMESTAMPTZ NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    provenance_digest BYTEA CHECK (provenance_digest IS NULL OR octet_length(provenance_digest) = 32),
    PRIMARY KEY (id, user_id),
    UNIQUE (external_resource_id, source_sequence),
    CONSTRAINT balance_observation_resource_fk
        FOREIGN KEY (external_resource_id, user_id, connection_id)
        REFERENCES banking.external_resources (id, user_id, connection_id),
    CONSTRAINT balance_observation_time_order CHECK (observed_at <= recorded_at),
    CONSTRAINT balance_observation_comparability CHECK (
        (comparable_amount IS NOT NULL AND comparable_currency IS NOT NULL AND non_comparable_reason IS NULL)
        OR (comparable_amount IS NULL AND comparable_currency IS NULL AND non_comparable_reason IS NOT NULL)
    )
);

CREATE TABLE banking.balance_observation_deliveries (
    observation_id UUID NOT NULL,
    user_id UUID NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'pending', 'delivered', 'ignored_older', 'retry_due', 'terminal_failure', 'not_comparable'
    )),
    reconciliation_case_id UUID,
    active_case_id UUID,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_retry_at TIMESTAMPTZ,
    last_error TEXT CHECK (last_error IS NULL OR char_length(last_error) <= 500),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (observation_id, user_id),
    FOREIGN KEY (observation_id, user_id)
        REFERENCES banking.balance_observations (id, user_id)
);

CREATE INDEX banking_observations_undelivered
    ON banking.balance_observation_deliveries (next_retry_at, observation_id)
    WHERE state IN ('pending', 'retry_due');
CREATE INDEX banking_observations_chronological
    ON banking.balance_observations (user_id, external_resource_id, observed_at DESC, source_sequence DESC, id DESC);

CREATE TABLE banking.sync_jobs (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    requested_from TIMESTAMPTZ NOT NULL,
    requested_to TIMESTAMPTZ NOT NULL,
    overlap_seconds INTEGER NOT NULL DEFAULT 0 CHECK (overlap_seconds BETWEEN 0 AND 86400),
    state TEXT NOT NULL CHECK (state IN (
        'requested', 'running', 'waiting_for_events', 'retry_due', 'completed', 'failed', 'cancelled'
    )),
    cursor TEXT CHECK (cursor IS NULL OR char_length(cursor) <= 500),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_retry_at TIMESTAMPTZ,
    last_error TEXT CHECK (last_error IS NULL OR char_length(last_error) <= 500),
    connection_version BIGINT NOT NULL CHECK (connection_version >= 1),
    credential_generation BIGINT NOT NULL CHECK (credential_generation >= 1),
    lease_holder TEXT CHECK (lease_holder IS NULL OR char_length(lease_holder) <= 200),
    lease_token BIGINT NOT NULL DEFAULT 0 CHECK (lease_token >= 0),
    lease_expires_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id),
    UNIQUE (id, user_id, connection_id),
    FOREIGN KEY (connection_id, user_id)
        REFERENCES banking.provider_connections (id, user_id),
    CHECK (requested_from <= requested_to)
);

CREATE INDEX banking_jobs_due
    ON banking.sync_jobs (next_retry_at, created_at, id)
    WHERE state IN ('requested', 'retry_due');
CREATE INDEX banking_jobs_by_connection
    ON banking.sync_jobs (user_id, connection_id, created_at DESC, id);

CREATE TABLE banking.sync_pages (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    sync_job_id UUID NOT NULL,
    page_number BIGINT NOT NULL CHECK (page_number >= 1),
    provider_cursor TEXT CHECK (provider_cursor IS NULL OR char_length(provider_cursor) <= 500),
    next_cursor TEXT CHECK (next_cursor IS NULL OR char_length(next_cursor) <= 500),
    expected_events INTEGER NOT NULL CHECK (expected_events >= 0),
    processed_events INTEGER NOT NULL DEFAULT 0 CHECK (processed_events >= 0),
    quarantined_events INTEGER NOT NULL DEFAULT 0 CHECK (quarantined_events >= 0),
    state TEXT NOT NULL CHECK (state IN ('intaking', 'waiting_for_events', 'completed')),
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id),
    UNIQUE (sync_job_id, page_number),
    FOREIGN KEY (sync_job_id, user_id, connection_id)
        REFERENCES banking.sync_jobs (id, user_id, connection_id),
    CHECK (processed_events + quarantined_events <= expected_events),
    CHECK ((state = 'completed') = (completed_at IS NOT NULL)),
    CHECK (state <> 'completed' OR processed_events + quarantined_events = expected_events)
);

CREATE TABLE banking.command_receipts (
    user_id UUID NOT NULL,
    scope TEXT NOT NULL CHECK (scope = btrim(scope) AND scope <> '' AND char_length(scope) <= 100),
    idempotency_key TEXT NOT NULL CHECK (
        idempotency_key = btrim(idempotency_key) AND idempotency_key <> '' AND char_length(idempotency_key) <= 200
    ),
    request_hash BYTEA NOT NULL CHECK (octet_length(request_hash) = 32),
    result JSONB NOT NULL,
    status_code SMALLINT NOT NULL CHECK (status_code BETWEEN 100 AND 599),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (user_id, scope, idempotency_key)
);

CREATE TABLE banking.webhook_receipts (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    delivery_digest BYTEA NOT NULL CHECK (octet_length(delivery_digest) = 32),
    provenance_ciphertext BYTEA,
    provenance_nonce BYTEA,
    provenance_key_id TEXT,
    provenance_envelope_version SMALLINT CHECK (provenance_envelope_version >= 1),
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'processed', 'quarantined')),
    received_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    processed_at TIMESTAMPTZ,
    PRIMARY KEY (id, user_id),
    UNIQUE (connection_id, delivery_digest),
    FOREIGN KEY (connection_id, user_id)
        REFERENCES banking.provider_connections (id, user_id)
);

CREATE FUNCTION banking.reject_immutable_fact_mutation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'Banking provider facts are immutable';
END;
$$;

CREATE TRIGGER provider_events_are_immutable
BEFORE UPDATE OR DELETE ON banking.provider_events
FOR EACH ROW EXECUTE FUNCTION banking.reject_immutable_fact_mutation();

CREATE TRIGGER balance_observations_are_immutable
BEFORE UPDATE OR DELETE ON banking.balance_observations
FOR EACH ROW EXECUTE FUNCTION banking.reject_immutable_fact_mutation();

CREATE TRIGGER provider_event_conflicts_are_immutable
BEFORE UPDATE OR DELETE ON banking.provider_event_conflicts
FOR EACH ROW EXECUTE FUNCTION banking.reject_immutable_fact_mutation();

CREATE FUNCTION banking.reject_hard_delete()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'Banking history cannot be hard deleted';
END;
$$;

CREATE TRIGGER provider_connections_cannot_be_deleted
BEFORE DELETE ON banking.provider_connections
FOR EACH ROW EXECUTE FUNCTION banking.reject_hard_delete();
CREATE TRIGGER external_resources_cannot_be_deleted
BEFORE DELETE ON banking.external_resources
FOR EACH ROW EXECUTE FUNCTION banking.reject_hard_delete();
CREATE TRIGGER resource_mappings_cannot_be_deleted
BEFORE DELETE ON banking.resource_mappings
FOR EACH ROW EXECUTE FUNCTION banking.reject_hard_delete();
