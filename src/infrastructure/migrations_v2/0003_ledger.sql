-- Finance V2 immutable, tenant-safe, double-entry Ledger.

CREATE DOMAIN ledger.numeric_28_8 AS NUMERIC
CHECK (
    SCALE(VALUE) <= 8
    AND VALUE BETWEEN -99999999999999999999.99999999
                  AND  99999999999999999999.99999999
);

CREATE TABLE ledger.accounts (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    name TEXT NOT NULL CHECK (
        name = BTRIM(name) AND name <> '' AND CHAR_LENGTH(name) <= 100
    ),
    currency VARCHAR(3) NOT NULL,
    nature TEXT NOT NULL CHECK (nature IN ('asset', 'liability', 'equity', 'income', 'expense')),
    kind TEXT NOT NULL CHECK (kind IN (
        'cash', 'debit_card', 'credit_card', 'current', 'savings', 'jar',
        'loan_payable', 'loan_receivable', 'system'
    )),
    authority TEXT NOT NULL CHECK (authority IN ('manual', 'provider_observed', 'system')),
    visibility TEXT NOT NULL CHECK (visibility IN ('user_visible', 'hidden')),
    lifecycle TEXT NOT NULL DEFAULT 'active' CHECK (lifecycle IN ('active', 'archived')),
    system_role TEXT CHECK (system_role IN (
        'uncategorized_income', 'uncategorized_expense', 'opening_balance_equity',
        'balance_adjustment_equity', 'fx_clearing', 'external_receivable',
        'external_payable', 'interest_receivable', 'interest_payable',
        'fee_receivable', 'fee_payable', 'portfolio_cash_clearing',
        'bad_debt_expense', 'debt_forgiveness_income'
    )),
    system_subject_reference TEXT CHECK (
        system_subject_reference IS NULL OR (
            system_subject_reference = BTRIM(system_subject_reference)
            AND system_subject_reference <> ''
            AND OCTET_LENGTH(system_subject_reference) <= 300
            AND system_subject_reference !~ '[[:cntrl:]]'
        )
    ),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id),
    CONSTRAINT ledger_account_currency_fk FOREIGN KEY (currency)
        REFERENCES reference_data.currencies (code),
    CONSTRAINT ledger_account_identity_currency_key UNIQUE (id, user_id, currency),
    CONSTRAINT ledger_account_posting_snapshot_key UNIQUE (id, user_id, currency, nature),
    CONSTRAINT ledger_account_policy CHECK (
        (
            authority IN ('manual', 'provider_observed')
            AND visibility = 'user_visible'
            AND system_role IS NULL
            AND system_subject_reference IS NULL
            AND (
                (nature = 'asset' AND kind IN (
                    'cash', 'debit_card', 'current', 'savings', 'jar', 'loan_receivable'
                ))
                OR (nature = 'liability' AND kind IN ('credit_card', 'loan_payable'))
            )
        )
        OR (
            authority = 'system'
            AND visibility = 'hidden'
            AND kind = 'system'
            AND system_role IS NOT NULL
        )
    )
);

CREATE UNIQUE INDEX ledger_system_account_role_key
    ON ledger.accounts (
        user_id, system_role, COALESCE(system_subject_reference, ''), currency
    )
    WHERE authority = 'system';

CREATE INDEX ledger_accounts_user_lifecycle_name
    ON ledger.accounts (user_id, lifecycle, lower(name), id);

CREATE TABLE ledger.journal_entries (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    ledger_sequence BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
    command_name TEXT NOT NULL CHECK (
        command_name = BTRIM(command_name) AND command_name <> ''
        AND OCTET_LENGTH(command_name) <= 100 AND command_name !~ '[[:cntrl:]]'
    ),
    source TEXT NOT NULL CHECK (source IN ('manual', 'import', 'system', 'correction', 'reconciliation')),
    purpose TEXT NOT NULL CHECK (purpose IN ('ordinary', 'correction', 'reversal', 'approved_reconciliation')),
    description TEXT NOT NULL CHECK (
        description = BTRIM(description) AND description <> '' AND CHAR_LENGTH(description) <= 500
    ),
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('user', 'system', 'external')),
    actor_reference TEXT CHECK (
        actor_reference IS NULL OR (
            actor_reference = BTRIM(actor_reference) AND actor_reference <> ''
            AND OCTET_LENGTH(actor_reference) <= 500 AND actor_reference !~ '[[:cntrl:]]'
        )
    ),
    occurred_at TIMESTAMPTZ NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL,
    correlation_id UUID NOT NULL,
    causation_id UUID,
    idempotency_key TEXT NOT NULL CHECK (
        idempotency_key = BTRIM(idempotency_key) AND idempotency_key <> ''
        AND OCTET_LENGTH(idempotency_key) <= 200 AND idempotency_key !~ '[[:cntrl:]]'
    ),
    reverses_transaction_id UUID,
    corrects_transaction_id UUID,
    replaces_transaction_id UUID,
    external_source_kind TEXT,
    external_source_reference TEXT,
    external_revision BIGINT,
    fx_rate NUMERIC CHECK (fx_rate IS NULL OR fx_rate > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id),
    CONSTRAINT journal_single_relation CHECK (
        num_nonnulls(reverses_transaction_id, corrects_transaction_id, replaces_transaction_id) <= 1
    ),
    CONSTRAINT journal_reversal_relation_required CHECK (
        purpose <> 'reversal' OR reverses_transaction_id IS NOT NULL
    ),
    CONSTRAINT journal_reverses_fk FOREIGN KEY (reverses_transaction_id, user_id)
        REFERENCES ledger.journal_entries (id, user_id),
    CONSTRAINT journal_corrects_fk FOREIGN KEY (corrects_transaction_id, user_id)
        REFERENCES ledger.journal_entries (id, user_id),
    CONSTRAINT journal_replaces_fk FOREIGN KEY (replaces_transaction_id, user_id)
        REFERENCES ledger.journal_entries (id, user_id),
    CONSTRAINT journal_idempotency_key UNIQUE (user_id, command_name, idempotency_key)
);

CREATE UNIQUE INDEX journal_one_reversal_per_original
    ON ledger.journal_entries (user_id, reverses_transaction_id)
    WHERE reverses_transaction_id IS NOT NULL;

CREATE UNIQUE INDEX journal_external_revision_key
    ON ledger.journal_entries (
        user_id, external_source_kind, external_source_reference, external_revision
    )
    WHERE external_source_kind IS NOT NULL;

CREATE INDEX journal_user_activity_order
    ON ledger.journal_entries (user_id, occurred_at DESC, ledger_sequence DESC);

CREATE TABLE ledger.postings (
    id UUID NOT NULL,
    journal_entry_id UUID NOT NULL,
    user_id UUID NOT NULL,
    account_id UUID NOT NULL,
    currency VARCHAR(3) NOT NULL,
    account_nature TEXT NOT NULL CHECK (account_nature IN ('asset', 'liability', 'equity', 'income', 'expense')),
    position SMALLINT NOT NULL CHECK (position > 0),
    signed_amount ledger.numeric_28_8 NOT NULL CHECK (signed_amount <> 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id),
    CONSTRAINT posting_journal_fk FOREIGN KEY (journal_entry_id, user_id)
        REFERENCES ledger.journal_entries (id, user_id),
    CONSTRAINT posting_account_snapshot_fk FOREIGN KEY (account_id, user_id, currency, account_nature)
        REFERENCES ledger.accounts (id, user_id, currency, nature),
    CONSTRAINT posting_stable_position UNIQUE (journal_entry_id, user_id, position)
);

CREATE INDEX postings_account_activity
    ON ledger.postings (user_id, account_id, journal_entry_id, position);

CREATE TABLE ledger.transaction_annotations (
    id UUID NOT NULL,
    journal_entry_id UUID NOT NULL,
    user_id UUID NOT NULL,
    description TEXT NOT NULL CHECK (
        description = BTRIM(description) AND description <> '' AND CHAR_LENGTH(description) <= 500
    ),
    category_id UUID,
    note TEXT CHECK (note IS NULL OR (note = BTRIM(note) AND note <> '' AND CHAR_LENGTH(note) <= 2000)),
    tags TEXT[] NOT NULL DEFAULT '{}' CHECK (
        cardinality(tags) <= 20 AND array_position(tags, NULL) IS NULL
    ),
    budget_visibility TEXT NOT NULL DEFAULT 'included'
        CHECK (budget_visibility IN ('included', 'excluded')),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id),
    CONSTRAINT annotation_journal_fk FOREIGN KEY (journal_entry_id, user_id)
        REFERENCES ledger.journal_entries (id, user_id),
    CONSTRAINT annotation_one_per_journal UNIQUE (journal_entry_id, user_id)
);

CREATE TABLE ledger.balance_correction_details (
    journal_entry_id UUID NOT NULL,
    user_id UUID NOT NULL,
    account_id UUID NOT NULL,
    currency VARCHAR(3) NOT NULL,
    before_display_balance ledger.numeric_28_8 NOT NULL,
    target_display_balance ledger.numeric_28_8 NOT NULL,
    display_delta ledger.numeric_28_8 NOT NULL CHECK (display_delta <> 0),
    observed_balance_version BIGINT NOT NULL CHECK (observed_balance_version >= 1),
    reason TEXT NOT NULL CHECK (
        reason = BTRIM(reason) AND reason <> '' AND CHAR_LENGTH(reason) <= 500
    ),
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('user', 'system', 'external')),
    actor_reference TEXT,
    observed_at TIMESTAMPTZ NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (journal_entry_id, user_id),
    CONSTRAINT correction_journal_fk FOREIGN KEY (journal_entry_id, user_id)
        REFERENCES ledger.journal_entries (id, user_id),
    CONSTRAINT correction_account_fk FOREIGN KEY (account_id, user_id, currency)
        REFERENCES ledger.accounts (id, user_id, currency)
);

CREATE TABLE ledger.reconciliation_streams (
    user_id UUID NOT NULL,
    account_id UUID NOT NULL,
    source_kind TEXT NOT NULL,
    source_stream_id TEXT NOT NULL,
    latest_observed_at TIMESTAMPTZ NOT NULL,
    latest_source_sequence BIGINT NOT NULL CHECK (latest_source_sequence >= 0),
    latest_observation_id UUID NOT NULL,
    active_case_id UUID,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (user_id, account_id, source_kind, source_stream_id),
    CONSTRAINT reconciliation_stream_account_fk FOREIGN KEY (account_id, user_id)
        REFERENCES ledger.accounts (id, user_id)
);

CREATE TABLE ledger.reconciliation_cases (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    account_id UUID NOT NULL,
    observation_id UUID NOT NULL,
    source_kind TEXT NOT NULL,
    source_stream_id TEXT NOT NULL,
    source_item_id TEXT NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL,
    source_sequence BIGINT NOT NULL CHECK (source_sequence >= 0),
    recorded_at TIMESTAMPTZ NOT NULL,
    provider_reported_balance ledger.numeric_28_8 NOT NULL,
    available_balance ledger.numeric_28_8,
    currency VARCHAR(3) NOT NULL,
    captured_ledger_balance ledger.numeric_28_8 NOT NULL,
    captured_balance_version BIGINT NOT NULL CHECK (captured_balance_version >= 1),
    delta ledger.numeric_28_8 NOT NULL,
    status TEXT NOT NULL CHECK (status IN (
        'matched', 'pending', 'superseded', 'ignored_older', 'approved', 'dismissed', 'stale'
    )),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    approval_journal_id UUID,
    reason TEXT,
    decision_actor_kind TEXT,
    decision_actor_reference TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (id, user_id),
    CONSTRAINT reconciliation_observation_key UNIQUE (user_id, observation_id),
    CONSTRAINT reconciliation_account_fk FOREIGN KEY (account_id, user_id, currency)
        REFERENCES ledger.accounts (id, user_id, currency),
    CONSTRAINT reconciliation_approval_journal_fk FOREIGN KEY (approval_journal_id, user_id)
        REFERENCES ledger.journal_entries (id, user_id),
    CONSTRAINT reconciliation_status_shape CHECK (
        (status = 'matched' AND delta = 0 AND approval_journal_id IS NULL)
        OR (status IN ('pending', 'superseded', 'ignored_older', 'dismissed', 'stale') AND approval_journal_id IS NULL)
        OR (status = 'approved' AND approval_journal_id IS NOT NULL)
    )
);

ALTER TABLE ledger.reconciliation_streams
    ADD CONSTRAINT reconciliation_stream_active_case_fk
    FOREIGN KEY (active_case_id, user_id)
    REFERENCES ledger.reconciliation_cases (id, user_id)
    DEFERRABLE INITIALLY DEFERRED;

CREATE INDEX reconciliation_cases_user_status_order
    ON ledger.reconciliation_cases (user_id, status, observed_at DESC, source_sequence DESC, observation_id DESC);

CREATE TABLE ledger.command_receipts (
    user_id UUID NOT NULL,
    command_name TEXT NOT NULL CHECK (
        command_name = BTRIM(command_name) AND command_name <> '' AND OCTET_LENGTH(command_name) <= 100
    ),
    idempotency_key TEXT NOT NULL CHECK (
        idempotency_key = BTRIM(idempotency_key) AND idempotency_key <> ''
        AND OCTET_LENGTH(idempotency_key) <= 200 AND idempotency_key !~ '[[:cntrl:]]'
    ),
    request_hash BYTEA NOT NULL CHECK (octet_length(request_hash) = 32),
    status TEXT NOT NULL CHECK (status IN ('in_progress', 'completed', 'failed', 'cancelled')),
    result JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (user_id, command_name, idempotency_key),
    CHECK ((status = 'in_progress') = (completed_at IS NULL))
);

CREATE TABLE ledger.account_balances (
    account_id UUID NOT NULL,
    user_id UUID NOT NULL,
    currency VARCHAR(3) NOT NULL,
    signed_balance ledger.numeric_28_8 NOT NULL DEFAULT 0,
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    as_of TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (account_id, user_id),
    CONSTRAINT account_balance_account_fk FOREIGN KEY (account_id, user_id, currency)
        REFERENCES ledger.accounts (id, user_id, currency)
);

CREATE TABLE ledger.audit_events (
    sequence BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_id UUID NOT NULL UNIQUE,
    user_id UUID NOT NULL,
    aggregate_kind TEXT NOT NULL CHECK (
        aggregate_kind = BTRIM(aggregate_kind) AND aggregate_kind <> '' AND OCTET_LENGTH(aggregate_kind) <= 100
    ),
    aggregate_id UUID NOT NULL,
    event_type TEXT NOT NULL CHECK (
        event_type = BTRIM(event_type) AND event_type <> '' AND OCTET_LENGTH(event_type) <= 200
    ),
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('user', 'system', 'external')),
    actor_reference TEXT,
    correlation_id UUID NOT NULL,
    payload JSONB NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE INDEX ledger_audit_aggregate_order
    ON ledger.audit_events (user_id, aggregate_kind, aggregate_id, sequence);

CREATE FUNCTION ledger.reject_immutable_financial_mutation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'posted Ledger financial facts are immutable';
END;
$$;

CREATE TRIGGER journal_entries_are_immutable
BEFORE UPDATE OR DELETE ON ledger.journal_entries
FOR EACH ROW EXECUTE FUNCTION ledger.reject_immutable_financial_mutation();

CREATE TRIGGER postings_are_immutable
BEFORE UPDATE OR DELETE ON ledger.postings
FOR EACH ROW EXECUTE FUNCTION ledger.reject_immutable_financial_mutation();

CREATE TRIGGER correction_details_are_immutable
BEFORE UPDATE OR DELETE ON ledger.balance_correction_details
FOR EACH ROW EXECUTE FUNCTION ledger.reject_immutable_financial_mutation();

CREATE TRIGGER audit_events_are_immutable
BEFORE UPDATE OR DELETE ON ledger.audit_events
FOR EACH ROW EXECUTE FUNCTION ledger.reject_immutable_financial_mutation();

CREATE FUNCTION ledger.reject_account_currency_change()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.currency <> OLD.currency THEN
        RAISE EXCEPTION 'Ledger account currency is immutable';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER account_currency_is_immutable
BEFORE UPDATE OF currency ON ledger.accounts
FOR EACH ROW EXECUTE FUNCTION ledger.reject_account_currency_change();

CREATE FUNCTION ledger.assert_journal_is_balanced()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
DECLARE
    target_journal UUID;
    target_user UUID;
    posting_count BIGINT;
BEGIN
    target_journal := NEW.id;
    target_user := NEW.user_id;
    IF TG_TABLE_NAME = 'postings' THEN
        target_journal := NEW.journal_entry_id;
    END IF;

    SELECT COUNT(*) INTO posting_count
    FROM ledger.postings
    WHERE journal_entry_id = target_journal AND user_id = target_user;

    IF posting_count < 2 THEN
        RAISE EXCEPTION 'journal entry requires at least two postings';
    END IF;

    IF EXISTS (
        SELECT 1
        FROM ledger.postings
        WHERE journal_entry_id = target_journal AND user_id = target_user
        GROUP BY currency
        HAVING SUM(signed_amount) <> 0
    ) THEN
        RAISE EXCEPTION 'journal entry is not balanced independently per currency';
    END IF;

    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER journal_entry_balance_at_commit
AFTER INSERT ON ledger.journal_entries
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION ledger.assert_journal_is_balanced();

CREATE CONSTRAINT TRIGGER posting_balance_at_commit
AFTER INSERT ON ledger.postings
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION ledger.assert_journal_is_balanced();
