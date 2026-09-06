-- Durable, tenant-isolated asynchronous transaction classification.

CREATE TABLE classification.classification_backfill_jobs (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    range_from TIMESTAMPTZ NOT NULL,
    range_to TIMESTAMPTZ NOT NULL,
    state TEXT NOT NULL DEFAULT 'requested'
        CHECK (state IN ('requested','running','quota_deferred','completed','failed','cancelled')),
    cursor_occurred_at TIMESTAMPTZ,
    cursor_journal_entry_id UUID,
    cursor_ledger_sequence BIGINT,
    classified_count BIGINT NOT NULL DEFAULT 0 CHECK (classified_count >= 0),
    review_count BIGINT NOT NULL DEFAULT 0 CHECK (review_count >= 0),
    abstained_count BIGINT NOT NULL DEFAULT 0 CHECK (abstained_count >= 0),
    failed_count BIGINT NOT NULL DEFAULT 0 CHECK (failed_count >= 0),
    next_resume_at TIMESTAMPTZ,
    lease_holder TEXT,
    lease_token BIGINT NOT NULL DEFAULT 0 CHECK (lease_token >= 0),
    lease_expires_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (id, user_id),
    CHECK (range_from < range_to),
    CHECK (num_nonnulls(cursor_occurred_at,cursor_journal_entry_id,cursor_ledger_sequence) IN (0,3)),
    CHECK ((lease_holder IS NULL) = (lease_expires_at IS NULL)),
    CHECK (
        lease_holder IS NULL OR (
            lease_holder = btrim(lease_holder) AND lease_holder <> ''
            AND char_length(lease_holder) <= 200
        )
    ),
    CHECK ((state = 'completed') = (completed_at IS NOT NULL))
);

CREATE UNIQUE INDEX classification_one_active_backfill_per_user
    ON classification.classification_backfill_jobs (user_id)
    WHERE state IN ('requested','running','quota_deferred');

CREATE INDEX classification_backfills_due
    ON classification.classification_backfill_jobs (next_resume_at, updated_at, id)
    WHERE state IN ('requested','running','quota_deferred');

-- A regenerated prediction replaces the outcome for a transaction, never adds
-- another transaction to the job's counters.
CREATE TABLE classification.classification_backfill_results (
    job_id UUID NOT NULL,
    user_id UUID NOT NULL,
    journal_entry_id UUID NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('classified','review','abstained','failed')),
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (job_id, user_id, journal_entry_id),
    FOREIGN KEY (job_id, user_id)
        REFERENCES classification.classification_backfill_jobs (id, user_id),
    FOREIGN KEY (journal_entry_id, user_id)
        REFERENCES ledger.journal_entries (id, user_id)
);

CREATE TABLE classification.classification_targets (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    journal_entry_id UUID NOT NULL,
    origin TEXT NOT NULL CHECK (origin IN ('live','backfill')),
    backfill_job_id UUID,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN (
        'pending','retry_due','quota_deferred','auto_apply_pending','review_pending',
        'applying','abstained','completed','failed','stale'
    )),
    generation BIGINT NOT NULL DEFAULT 1 CHECK (generation >= 1),
    taxonomy_version BIGINT NOT NULL CHECK (taxonomy_version >= 1),
    annotation_version BIGINT NOT NULL CHECK (annotation_version >= 1),
    evidence_digest BYTEA NOT NULL CHECK (octet_length(evidence_digest) = 32),
    taxonomy_digest BYTEA NOT NULL CHECK (octet_length(taxonomy_digest) = 32),
    evidence JSONB NOT NULL CHECK (jsonb_typeof(evidence) = 'object'),
    source_occurred_at TIMESTAMPTZ NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    outbound_attempts INTEGER NOT NULL DEFAULT 0 CHECK (outbound_attempts >= 0),
    application_attempts INTEGER NOT NULL DEFAULT 0 CHECK (application_attempts >= 0),
    next_attempt_at TIMESTAMPTZ,
    last_error_code TEXT CHECK (
        last_error_code IS NULL OR last_error_code IN (
            'provider_transient','provider_rate_limited','provider_terminal',
            'provider_invalid_response','provider_configuration','persistence'
        )
    ),
    lease_holder TEXT,
    lease_token BIGINT NOT NULL DEFAULT 0 CHECK (lease_token >= 0),
    lease_expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id),
    UNIQUE (journal_entry_id, user_id),
    FOREIGN KEY (journal_entry_id, user_id)
        REFERENCES ledger.journal_entries (id, user_id),
    FOREIGN KEY (backfill_job_id, user_id)
        REFERENCES classification.classification_backfill_jobs (id, user_id),
    CHECK (
        (origin = 'live' AND backfill_job_id IS NULL)
        OR (origin = 'backfill' AND backfill_job_id IS NOT NULL)
    ),
    CHECK ((lease_holder IS NULL) = (lease_expires_at IS NULL)),
    CHECK (
        lease_holder IS NULL OR (
            lease_holder = btrim(lease_holder) AND lease_holder <> ''
            AND char_length(lease_holder) <= 200
        )
    )
);

CREATE INDEX classification_targets_due
    ON classification.classification_targets (
        (CASE WHEN origin = 'live' THEN 0 ELSE 1 END),
        next_attempt_at,
        created_at,
        source_occurred_at DESC,
        id
    )
    WHERE state IN ('pending','retry_due','quota_deferred');

CREATE INDEX classification_targets_actionable
    ON classification.classification_targets (updated_at, id)
    WHERE state IN ('auto_apply_pending','applying');

CREATE TABLE classification.classification_decisions (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    target_id UUID NOT NULL,
    journal_entry_id UUID NOT NULL,
    generation BIGINT NOT NULL CHECK (generation >= 1),
    candidate_category_id UUID,
    confidence_basis_points INTEGER NOT NULL
        CHECK (confidence_basis_points BETWEEN 0 AND 10000),
    reason_code TEXT NOT NULL CHECK (reason_code IN (
        'merchant_match','mcc_match','description_match','amount_pattern',
        'tenant_example','mixed_signals','insufficient_evidence'
    )),
    explanation TEXT NOT NULL CHECK (
        explanation = btrim(explanation) AND explanation <> ''
        AND char_length(explanation) <= 240 AND explanation !~ '[[:cntrl:]]'
    ),
    state TEXT NOT NULL CHECK (state IN (
        'auto_apply_pending','review_pending','applying','applied','accepted',
        'corrected','rejected','abstained','stale','failed'
    )),
    resolution_action TEXT CHECK (resolution_action IN ('accept','correct','reject')),
    chosen_category_id UUID,
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    taxonomy_version BIGINT NOT NULL CHECK (taxonomy_version >= 1),
    annotation_version BIGINT NOT NULL CHECK (annotation_version >= 1),
    evidence_digest BYTEA NOT NULL CHECK (octet_length(evidence_digest) = 32),
    taxonomy_digest BYTEA NOT NULL CHECK (octet_length(taxonomy_digest) = 32),
    provider TEXT NOT NULL CHECK (
        provider = btrim(provider) AND provider <> '' AND char_length(provider) <= 100
    ),
    model TEXT NOT NULL CHECK (
        model = btrim(model) AND model <> '' AND char_length(model) <= 200
    ),
    prompt_version TEXT NOT NULL CHECK (
        prompt_version = btrim(prompt_version) AND prompt_version <> ''
        AND char_length(prompt_version) <= 100
    ),
    provider_duration_ms BIGINT NOT NULL DEFAULT 0 CHECK (provider_duration_ms >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    resolved_at TIMESTAMPTZ,
    PRIMARY KEY (id, user_id),
    UNIQUE (target_id, user_id, generation),
    FOREIGN KEY (target_id, user_id)
        REFERENCES classification.classification_targets (id, user_id),
    FOREIGN KEY (journal_entry_id, user_id)
        REFERENCES ledger.journal_entries (id, user_id),
    FOREIGN KEY (candidate_category_id, user_id)
        REFERENCES classification.categories (id, user_id),
    FOREIGN KEY (chosen_category_id, user_id)
        REFERENCES classification.categories (id, user_id),
    CHECK (candidate_category_id IS NOT NULL OR state IN ('abstained','failed')),
    CHECK (
        (resolution_action IS NULL AND chosen_category_id IS NULL AND resolved_at IS NULL)
        OR resolution_action IS NOT NULL
    ),
    CHECK (resolution_action <> 'reject' OR chosen_category_id IS NULL)
);

CREATE INDEX classification_review_queue
    ON classification.classification_decisions (user_id, created_at, id)
    WHERE state = 'review_pending';

CREATE INDEX classification_application_queue
    ON classification.classification_decisions (updated_at, id)
    WHERE state IN ('auto_apply_pending','applying');

CREATE TABLE classification.classification_feedback_examples (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    journal_entry_id UUID NOT NULL,
    source TEXT NOT NULL CHECK (source IN (
        'manual','accepted','corrected','rejected','automatic_removed'
    )),
    positive_category_id UUID,
    negative_category_id UUID,
    description TEXT NOT NULL CHECK (
        description = btrim(description) AND description <> ''
        AND char_length(description) <= 500
    ),
    amount NUMERIC(28,8) NOT NULL CHECK (amount > 0),
    currency VARCHAR(3) NOT NULL CHECK (currency COLLATE "C" ~ '^[A-Z]{3}$'),
    occurred_at TIMESTAMPTZ NOT NULL,
    provider TEXT CHECK (
        provider IS NULL OR (
            provider = btrim(provider) AND provider <> '' AND char_length(provider) <= 100
        )
    ),
    merchant_mcc INTEGER CHECK (merchant_mcc BETWEEN 0 AND 9999),
    account_label TEXT CHECK (
        account_label IS NULL OR (
            account_label = btrim(account_label) AND account_label <> ''
            AND char_length(account_label) <= 200
        )
    ),
    evidence_digest BYTEA NOT NULL CHECK (octet_length(evidence_digest) = 32),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id),
    FOREIGN KEY (journal_entry_id, user_id)
        REFERENCES ledger.journal_entries (id, user_id),
    FOREIGN KEY (positive_category_id, user_id)
        REFERENCES classification.categories (id, user_id),
    FOREIGN KEY (negative_category_id, user_id)
        REFERENCES classification.categories (id, user_id),
    CHECK (num_nonnulls(positive_category_id, negative_category_id) >= 1)
);

CREATE UNIQUE INDEX classification_feedback_fact_unique
    ON classification.classification_feedback_examples (
        user_id,
        journal_entry_id,
        source,
        COALESCE(positive_category_id, '00000000-0000-0000-0000-000000000000'::uuid),
        COALESCE(negative_category_id, '00000000-0000-0000-0000-000000000000'::uuid)
    );

CREATE INDEX classification_feedback_lookup
    ON classification.classification_feedback_examples (
        user_id, provider, merchant_mcc, created_at DESC, id
    );

CREATE TABLE classification.classification_daily_usage (
    user_id UUID NOT NULL,
    usage_date DATE NOT NULL,
    outbound_calls INTEGER NOT NULL DEFAULT 0 CHECK (outbound_calls >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (user_id, usage_date)
);

-- One durable reservation per outbound attempt, including process crashes.
-- Only redacted outcome metadata is retained; no provider request or response.
CREATE TABLE classification.classification_provider_attempts (
    target_id UUID NOT NULL,
    user_id UUID NOT NULL,
    lease_token BIGINT NOT NULL,
    reserved_at TIMESTAMPTZ NOT NULL,
    outcome TEXT CHECK (outcome IN ('succeeded','failed')),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (target_id, user_id, lease_token),
    FOREIGN KEY (target_id, user_id)
        REFERENCES classification.classification_targets (id, user_id),
    CHECK ((outcome IS NULL) = (completed_at IS NULL))
);

CREATE TABLE classification.classification_command_receipts (
    user_id UUID NOT NULL,
    command_scope TEXT NOT NULL CHECK (
        command_scope = btrim(command_scope) AND command_scope <> ''
        AND char_length(command_scope) <= 100
    ),
    idempotency_key TEXT NOT NULL CHECK (
        idempotency_key = btrim(idempotency_key) AND idempotency_key <> ''
        AND octet_length(idempotency_key) <= 200 AND idempotency_key !~ '[[:cntrl:]]'
    ),
    command_name TEXT NOT NULL CHECK (
        command_name = btrim(command_name) AND command_name <> ''
        AND char_length(command_name) <= 100
    ),
    request_hash BYTEA NOT NULL CHECK (octet_length(request_hash) = 32),
    status TEXT NOT NULL CHECK (status IN ('processing','succeeded','rejected','failed')),
    http_status SMALLINT,
    response_body JSONB,
    aggregate_id UUID,
    aggregate_version BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (user_id, command_scope, idempotency_key),
    CHECK ((status = 'processing') = (completed_at IS NULL))
);

CREATE TABLE classification.classification_consumed_events (
    consumer_name TEXT NOT NULL CHECK (
        consumer_name = btrim(consumer_name) AND consumer_name <> ''
        AND char_length(consumer_name) <= 200
    ),
    event_id UUID NOT NULL,
    event_type TEXT NOT NULL CHECK (
        event_type = btrim(event_type) AND event_type <> ''
        AND char_length(event_type) <= 200
    ),
    sequence BIGINT,
    payload_digest BYTEA NOT NULL CHECK (octet_length(payload_digest) = 32),
    processed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (consumer_name, event_id)
);

-- AI provenance must always point at an auditable prediction owned by the same tenant.
ALTER TABLE ledger.transaction_annotations
    ADD CONSTRAINT annotation_classification_decision_fk
    FOREIGN KEY (classification_decision_id, user_id)
    REFERENCES classification.classification_decisions (id, user_id)
    DEFERRABLE INITIALLY DEFERRED;
