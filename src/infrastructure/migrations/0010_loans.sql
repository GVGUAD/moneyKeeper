-- Loans owns contractual state and durable accounting intent. Ledger identities
-- are deliberately opaque UUID values: there are no cross-context foreign keys.

CREATE TABLE loans.agreements (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    direction TEXT NOT NULL CHECK (direction IN ('borrowed', 'lent')),
    counterparty VARCHAR(200) NOT NULL CHECK (
        counterparty <> '' AND counterparty = BTRIM(counterparty)
        AND counterparty !~ '[[:cntrl:]]'
    ),
    contractual_principal NUMERIC(28,8) NOT NULL CHECK (contractual_principal > 0),
    currency VARCHAR(3) NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    start_date DATE NOT NULL,
    due_date DATE,
    annual_rate NUMERIC(18,10) CHECK (annual_rate >= 0),
    ledger_principal_account_id UUID,
    status TEXT NOT NULL CHECK (status IN (
        'draft', 'pending_accounting', 'active', 'failed', 'closed'
    )),
    accounting_error VARCHAR(512),
    version BIGINT NOT NULL CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (id),
    UNIQUE (id, user_id),
    CHECK (due_date IS NULL OR due_date >= start_date),
    CHECK ((status IN ('active', 'closed')) = (ledger_principal_account_id IS NOT NULL)
           OR status = 'failed')
);

CREATE TABLE loans.term_revisions (
    id UUID PRIMARY KEY,
    agreement_id UUID NOT NULL,
    user_id UUID NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    counterparty VARCHAR(200) NOT NULL CHECK (
        counterparty <> '' AND counterparty = BTRIM(counterparty)
        AND counterparty !~ '[[:cntrl:]]'
    ),
    contractual_principal NUMERIC(28,8) NOT NULL CHECK (contractual_principal > 0),
    start_date DATE NOT NULL,
    due_date DATE,
    annual_rate NUMERIC(18,10) CHECK (annual_rate >= 0),
    reason VARCHAR(500) NOT NULL CHECK (reason <> '' AND reason = BTRIM(reason)),
    recorded_at TIMESTAMPTZ NOT NULL,
    UNIQUE (agreement_id, user_id, revision),
    FOREIGN KEY (agreement_id, user_id)
        REFERENCES loans.agreements(id, user_id),
    CHECK (due_date IS NULL OR due_date >= start_date)
);

CREATE TABLE loans.component_balances (
    agreement_id UUID NOT NULL,
    user_id UUID NOT NULL,
    currency VARCHAR(3) NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    principal NUMERIC(28,8) NOT NULL DEFAULT 0 CHECK (principal >= 0),
    accrued_interest NUMERIC(28,8) NOT NULL DEFAULT 0 CHECK (accrued_interest >= 0),
    accrued_fee NUMERIC(28,8) NOT NULL DEFAULT 0 CHECK (accrued_fee >= 0),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (agreement_id, user_id),
    FOREIGN KEY (agreement_id, user_id)
        REFERENCES loans.agreements(id, user_id)
);

CREATE TABLE loans.movements (
    id UUID NOT NULL,
    agreement_id UUID NOT NULL,
    user_id UUID NOT NULL,
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    kind TEXT NOT NULL CHECK (kind IN ('disbursement','repayment','accrual','write_off')),
    currency VARCHAR(3) NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    principal NUMERIC(28,8) NOT NULL DEFAULT 0 CHECK (principal >= 0),
    accrued_interest NUMERIC(28,8) NOT NULL DEFAULT 0 CHECK (accrued_interest >= 0),
    accrued_fee NUMERIC(28,8) NOT NULL DEFAULT 0 CHECK (accrued_fee >= 0),
    current_interest NUMERIC(28,8) NOT NULL DEFAULT 0 CHECK (current_interest >= 0),
    current_fee NUMERIC(28,8) NOT NULL DEFAULT 0 CHECK (current_fee >= 0),
    cash_account_id UUID,
    reason VARCHAR(500),
    status TEXT NOT NULL CHECK (status IN ('replacement_requested','pending_accounting','posted','failed','reversal_pending','reversed')),
    process_correlation_id UUID NOT NULL,
    ledger_journal_id UUID,
    ledger_reversal_id UUID,
    replaces_movement_id UUID,
    reversed_by_movement_id UUID,
    last_error VARCHAR(512),
    requested_at TIMESTAMPTZ NOT NULL,
    posted_at TIMESTAMPTZ,
    reversed_at TIMESTAMPTZ,
    PRIMARY KEY (id),
    UNIQUE (id, user_id),
    UNIQUE (agreement_id, user_id, sequence),
    FOREIGN KEY (agreement_id, user_id)
        REFERENCES loans.agreements(id, user_id),
    FOREIGN KEY (replaces_movement_id, user_id)
        REFERENCES loans.movements(id, user_id),
    FOREIGN KEY (reversed_by_movement_id, user_id)
        REFERENCES loans.movements(id, user_id),
    CHECK (principal + accrued_interest + accrued_fee + current_interest + current_fee > 0),
    CHECK ((kind IN ('disbursement','repayment')) = (cash_account_id IS NOT NULL)
           OR kind IN ('accrual','write_off')),
    CHECK ((status IN ('posted','reversal_pending','reversed')) = (ledger_journal_id IS NOT NULL)),
    CHECK ((status = 'reversed') = (ledger_reversal_id IS NOT NULL)),
    CHECK (kind = 'write_off' OR reason IS NULL OR reason = BTRIM(reason)),
    CHECK (kind <> 'write_off' OR (reason IS NOT NULL AND reason <> '' AND reason = BTRIM(reason)))
);

CREATE TABLE loans.movement_status_history (
    movement_id UUID NOT NULL,
    user_id UUID NOT NULL,
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    status TEXT NOT NULL CHECK (status IN ('replacement_requested','pending_accounting','posted','failed','reversal_pending','reversed')),
    ledger_journal_id UUID,
    error VARCHAR(512),
    recorded_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (movement_id, user_id, sequence),
    FOREIGN KEY (movement_id, user_id)
        REFERENCES loans.movements(id, user_id)
);

CREATE TABLE loans.replacement_processes (
    original_movement_id UUID NOT NULL,
    replacement_movement_id UUID NOT NULL,
    user_id UUID NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'replacement_requested','reversing_original','original_reversed',
        'posting_replacement','posted','retry_due','terminal_failure',
        'replacement_failed_after_reversal'
    )),
    correlation_id UUID NOT NULL,
    last_error VARCHAR(512),
    version BIGINT NOT NULL CHECK (version > 0),
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (original_movement_id, replacement_movement_id, user_id),
    FOREIGN KEY (original_movement_id, user_id)
        REFERENCES loans.movements(id, user_id),
    FOREIGN KEY (replacement_movement_id, user_id)
        REFERENCES loans.movements(id, user_id)
);

CREATE TABLE loans.reversal_requests (
    movement_id UUID NOT NULL,
    agreement_id UUID NOT NULL,
    user_id UUID NOT NULL,
    reason VARCHAR(500) NOT NULL CHECK (reason <> '' AND reason = BTRIM(reason)),
    correlation_id UUID NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending','posted','retry_due','terminal_failure')),
    ledger_reversal_id UUID,
    last_error VARCHAR(512),
    requested_at TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (movement_id, user_id),
    FOREIGN KEY (agreement_id, user_id) REFERENCES loans.agreements(id, user_id),
    FOREIGN KEY (movement_id, user_id) REFERENCES loans.movements(id, user_id),
    CHECK ((state = 'posted') = (ledger_reversal_id IS NOT NULL))
);

CREATE TABLE loans.command_receipts (
    user_id UUID NOT NULL,
    command_scope VARCHAR(100) NOT NULL CHECK (
        command_scope <> '' AND command_scope = BTRIM(command_scope)
    ),
    idempotency_key VARCHAR(200) NOT NULL CHECK (
        idempotency_key <> '' AND idempotency_key = BTRIM(idempotency_key)
        AND idempotency_key !~ '[[:cntrl:]]'
    ),
    canonical_request_hash BYTEA NOT NULL CHECK (octet_length(canonical_request_hash) = 32),
    status_code SMALLINT NOT NULL CHECK (status_code BETWEEN 100 AND 599),
    durable_result JSONB NOT NULL,
    aggregate_id UUID NOT NULL,
    aggregate_version BIGINT NOT NULL CHECK (aggregate_version > 0),
    created_at TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (user_id, command_scope, idempotency_key)
);

CREATE TABLE loans.audit_log (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    agreement_id UUID NOT NULL,
    movement_id UUID,
    action VARCHAR(100) NOT NULL,
    details JSONB NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (agreement_id, user_id)
        REFERENCES loans.agreements(id, user_id),
    FOREIGN KEY (movement_id, user_id)
        REFERENCES loans.movements(id, user_id)
);

CREATE INDEX loans_agreements_activity_idx
    ON loans.agreements(user_id, status, updated_at DESC, id);
CREATE INDEX loans_movements_activity_idx
    ON loans.movements(user_id, agreement_id, sequence DESC);
CREATE INDEX loans_movements_pending_idx
    ON loans.movements(requested_at, id) WHERE status = 'pending_accounting';

-- Reporting owns this rebuildable row; Phase 4 reserved the table and Phase 6
-- completes the Loan consumer contract without introducing a foreign key.
ALTER TABLE reporting.loan_summaries
    ADD COLUMN direction TEXT CHECK (direction IN ('borrowed','lent')),
    ADD COLUMN status TEXT NOT NULL DEFAULT 'pending_accounting' CHECK (
        status IN ('pending_accounting','active','failed','closed')
    );

CREATE FUNCTION loans.reject_immutable_history_change()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'loan history is immutable';
END
$$;

CREATE TRIGGER loans_term_revisions_immutable
BEFORE UPDATE OR DELETE ON loans.term_revisions
FOR EACH ROW EXECUTE FUNCTION loans.reject_immutable_history_change();

CREATE TRIGGER loans_movement_status_history_immutable
BEFORE UPDATE OR DELETE ON loans.movement_status_history
FOR EACH ROW EXECUTE FUNCTION loans.reject_immutable_history_change();

CREATE FUNCTION loans.reject_posted_movement_change()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status IN ('posted','reversed') THEN
        IF TG_OP = 'DELETE' OR NEW.kind <> OLD.kind OR NEW.currency <> OLD.currency
           OR NEW.principal <> OLD.principal
           OR NEW.accrued_interest <> OLD.accrued_interest
           OR NEW.accrued_fee <> OLD.accrued_fee
           OR NEW.current_interest <> OLD.current_interest
           OR NEW.current_fee <> OLD.current_fee
           OR NEW.cash_account_id IS DISTINCT FROM OLD.cash_account_id
           OR NEW.reason IS DISTINCT FROM OLD.reason
           OR NEW.ledger_journal_id IS DISTINCT FROM OLD.ledger_journal_id THEN
            RAISE EXCEPTION 'posted loan movement facts are immutable';
        END IF;
    END IF;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END
$$;

CREATE TRIGGER loans_posted_movements_immutable
BEFORE UPDATE OR DELETE ON loans.movements
FOR EACH ROW EXECUTE FUNCTION loans.reject_posted_movement_change();
