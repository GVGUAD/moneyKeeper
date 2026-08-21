-- Contacts-first shared bills. Ledger identities are deliberately opaque UUIDs:
-- this schema owns no foreign key and executes no SQL against another context.
CREATE TABLE sharing.contacts (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    display_name VARCHAR(200) NOT NULL CHECK (display_name = btrim(display_name) AND display_name <> ''),
    note VARCHAR(2000),
    lifecycle TEXT NOT NULL DEFAULT 'active' CHECK (lifecycle IN ('active', 'archived')),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id)
);
CREATE INDEX sharing_contacts_user_lifecycle_name ON sharing.contacts(user_id, lifecycle, lower(display_name), id);

CREATE TABLE sharing.bills (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    currency VARCHAR(3) NOT NULL CHECK (currency COLLATE "C" ~ '^[A-Z]{3}$'),
    current_revision INTEGER NOT NULL DEFAULT 1 CHECK (current_revision >= 1),
    status TEXT NOT NULL CHECK (status IN ('pending_accounting','active','failed','pending_cancellation','cancelled')),
    active_settlements INTEGER NOT NULL DEFAULT 0 CHECK (active_settlements >= 0),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    cancellation_reason VARCHAR(1000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id),
    UNIQUE (id, user_id, currency)
);

CREATE TABLE sharing.bill_revisions (
    bill_id UUID NOT NULL,
    user_id UUID NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    title VARCHAR(300) NOT NULL CHECK (title = btrim(title) AND title <> ''),
    occurred_at TIMESTAMPTZ NOT NULL,
    total NUMERIC(28,8) NOT NULL CHECK (total > 0),
    currency VARCHAR(3) NOT NULL,
    accounting_status TEXT NOT NULL CHECK (accounting_status IN ('pending','posted','failed')),
    accounting_correlation_id UUID NOT NULL,
    accounting_journal_id UUID,
    accounting_reversal_journal_id UUID,
    last_error VARCHAR(1000),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (bill_id, user_id, revision),
    FOREIGN KEY (bill_id, user_id, currency) REFERENCES sharing.bills(id, user_id, currency)
);

CREATE TABLE sharing.contributions (
    id UUID NOT NULL,
    bill_id UUID NOT NULL,
    user_id UUID NOT NULL,
    revision INTEGER NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    participant_kind TEXT NOT NULL CHECK (participant_kind IN ('current_user','contact')),
    participant_contact_id UUID,
    amount NUMERIC(28,8) NOT NULL CHECK (amount > 0),
    currency VARCHAR(3) NOT NULL,
    evidence_kind TEXT NOT NULL CHECK (evidence_kind IN ('external','manual','existing_journals')),
    ledger_account_id UUID,
    PRIMARY KEY (id, user_id),
    UNIQUE (bill_id, user_id, revision, position),
    FOREIGN KEY (bill_id, user_id, revision) REFERENCES sharing.bill_revisions(bill_id, user_id, revision),
    FOREIGN KEY (participant_contact_id, user_id) REFERENCES sharing.contacts(id, user_id),
    CHECK ((participant_kind = 'current_user' AND participant_contact_id IS NULL) OR (participant_kind = 'contact' AND participant_contact_id IS NOT NULL)),
    CHECK (participant_kind = 'current_user' OR evidence_kind = 'external')
);

CREATE TABLE sharing.contribution_journal_allocations (
    contribution_id UUID NOT NULL,
    user_id UUID NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    ledger_journal_id UUID NOT NULL,
    amount NUMERIC(28,8) NOT NULL CHECK (amount > 0),
    currency VARCHAR(3) NOT NULL,
    PRIMARY KEY (contribution_id, user_id, position),
    UNIQUE (user_id, ledger_journal_id, contribution_id),
    FOREIGN KEY (contribution_id, user_id) REFERENCES sharing.contributions(id, user_id)
);
CREATE INDEX sharing_contribution_ledger_allocations ON sharing.contribution_journal_allocations(user_id, ledger_journal_id);

CREATE TABLE sharing.participant_shares (
    bill_id UUID NOT NULL,
    user_id UUID NOT NULL,
    revision INTEGER NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    participant_kind TEXT NOT NULL CHECK (participant_kind IN ('current_user','contact')),
    participant_contact_id UUID,
    amount NUMERIC(28,8) NOT NULL CHECK (amount >= 0),
    currency VARCHAR(3) NOT NULL,
    PRIMARY KEY (bill_id, user_id, revision, position),
    FOREIGN KEY (bill_id, user_id, revision) REFERENCES sharing.bill_revisions(bill_id, user_id, revision),
    FOREIGN KEY (participant_contact_id, user_id) REFERENCES sharing.contacts(id, user_id),
    CHECK ((participant_kind = 'current_user' AND participant_contact_id IS NULL) OR (participant_kind = 'contact' AND participant_contact_id IS NOT NULL))
);
CREATE UNIQUE INDEX sharing_one_share_per_participant ON sharing.participant_shares(bill_id, user_id, revision, participant_kind, COALESCE(participant_contact_id, '00000000-0000-0000-0000-000000000000'::uuid));

CREATE TABLE sharing.obligations (
    id UUID NOT NULL,
    bill_id UUID NOT NULL,
    user_id UUID NOT NULL,
    revision INTEGER NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    debtor_kind TEXT NOT NULL CHECK (debtor_kind IN ('current_user','contact')),
    debtor_contact_id UUID,
    creditor_kind TEXT NOT NULL CHECK (creditor_kind IN ('current_user','contact')),
    creditor_contact_id UUID,
    original_amount NUMERIC(28,8) NOT NULL CHECK (original_amount > 0),
    settled_amount NUMERIC(28,8) NOT NULL DEFAULT 0 CHECK (settled_amount >= 0 AND settled_amount <= original_amount),
    currency VARCHAR(3) NOT NULL,
    PRIMARY KEY (id, user_id),
    UNIQUE (bill_id, user_id, revision, position),
    FOREIGN KEY (bill_id, user_id, revision) REFERENCES sharing.bill_revisions(bill_id, user_id, revision),
    FOREIGN KEY (debtor_contact_id, user_id) REFERENCES sharing.contacts(id, user_id),
    FOREIGN KEY (creditor_contact_id, user_id) REFERENCES sharing.contacts(id, user_id),
    CHECK ((debtor_kind = 'current_user' AND debtor_contact_id IS NULL) OR (debtor_kind = 'contact' AND debtor_contact_id IS NOT NULL)),
    CHECK ((creditor_kind = 'current_user' AND creditor_contact_id IS NULL) OR (creditor_kind = 'contact' AND creditor_contact_id IS NOT NULL)),
    CHECK ((debtor_kind, COALESCE(debtor_contact_id, '00000000-0000-0000-0000-000000000000'::uuid)) <> (creditor_kind, COALESCE(creditor_contact_id, '00000000-0000-0000-0000-000000000000'::uuid)))
);
CREATE INDEX sharing_obligations_bill_order ON sharing.obligations(user_id, bill_id, revision, position, id);

CREATE TABLE sharing.settlements (
    id UUID NOT NULL,
    bill_id UUID NOT NULL,
    user_id UUID NOT NULL,
    obligation_id UUID NOT NULL,
    amount NUMERIC(28,8) NOT NULL CHECK (amount > 0),
    currency VARCHAR(3) NOT NULL,
    evidence_kind TEXT NOT NULL CHECK (evidence_kind IN ('external','manual','existing_journal')),
    ledger_account_id UUID,
    ledger_journal_id UUID,
    status TEXT NOT NULL CHECK (status IN ('pending_accounting','posted','failed')),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version >= 1),
    accounting_correlation_id UUID NOT NULL,
    accounting_journal_id UUID,
    last_error VARCHAR(1000),
    occurred_at TIMESTAMPTZ NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id, user_id),
    FOREIGN KEY (bill_id, user_id) REFERENCES sharing.bills(id, user_id),
    FOREIGN KEY (obligation_id, user_id) REFERENCES sharing.obligations(id, user_id)
);
CREATE INDEX sharing_settlements_bill_status ON sharing.settlements(user_id, bill_id, status, id);

CREATE TABLE sharing.settlement_reversals (
    settlement_id UUID NOT NULL,
    user_id UUID NOT NULL,
    reason VARCHAR(1000) NOT NULL CHECK (reason = btrim(reason) AND reason <> ''),
    correlation_id UUID NOT NULL,
    ledger_reversal_journal_id UUID,
    reversed_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (settlement_id, user_id),
    FOREIGN KEY (settlement_id, user_id) REFERENCES sharing.settlements(id, user_id)
);

CREATE TABLE sharing.command_receipts (
    user_id UUID NOT NULL,
    command_scope VARCHAR(200) NOT NULL,
    idempotency_key VARCHAR(200) NOT NULL,
    canonical_request_hash BYTEA NOT NULL CHECK (octet_length(canonical_request_hash) = 32),
    result_status SMALLINT NOT NULL CHECK (result_status BETWEEN 100 AND 599),
    durable_result JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (user_id, command_scope, idempotency_key)
);

CREATE TABLE sharing.audit_log (
    sequence BIGSERIAL PRIMARY KEY,
    user_id UUID NOT NULL,
    aggregate_type VARCHAR(100) NOT NULL,
    aggregate_id UUID NOT NULL,
    aggregate_version BIGINT NOT NULL CHECK (aggregate_version >= 1),
    action VARCHAR(100) NOT NULL,
    correlation_id UUID NOT NULL,
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE sharing.contact_balance_projection (
    user_id UUID NOT NULL,
    contact_id UUID NOT NULL,
    currency VARCHAR(3) NOT NULL,
    receivable NUMERIC(28,8) NOT NULL DEFAULT 0 CHECK (receivable >= 0),
    payable NUMERIC(28,8) NOT NULL DEFAULT 0 CHECK (payable >= 0),
    source_version BIGINT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (user_id, contact_id, currency),
    FOREIGN KEY (contact_id, user_id) REFERENCES sharing.contacts(id, user_id)
);

-- Rebuildable Reporting history populated only from public Sharing events.
CREATE TABLE reporting.bill_position_history (
    event_id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    bill_id UUID NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    state TEXT NOT NULL CHECK (state IN ('active', 'cancelled')),
    currency VARCHAR(3),
    receivable NUMERIC(28,8),
    payable NUMERIC(28,8),
    cancellation_reason VARCHAR(1000),
    occurred_at TIMESTAMPTZ NOT NULL,
    source_sequence BIGINT NOT NULL
);
CREATE INDEX reporting_bill_position_history_order ON reporting.bill_position_history(user_id, bill_id, source_sequence, event_id);

CREATE FUNCTION sharing.reject_immutable_fact_mutation() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'Sharing allocation facts are immutable'; END; $$;

CREATE FUNCTION sharing.protect_bill_revision_facts() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN RAISE EXCEPTION 'Sharing bill revisions are immutable'; END IF;
    IF (OLD.bill_id, OLD.user_id, OLD.revision, OLD.title, OLD.occurred_at, OLD.total, OLD.currency, OLD.accounting_correlation_id, OLD.recorded_at)
       IS DISTINCT FROM
       (NEW.bill_id, NEW.user_id, NEW.revision, NEW.title, NEW.occurred_at, NEW.total, NEW.currency, NEW.accounting_correlation_id, NEW.recorded_at)
    THEN RAISE EXCEPTION 'Sharing bill revision facts are immutable'; END IF;
    RETURN NEW;
END; $$;
CREATE TRIGGER sharing_bill_revisions_immutable BEFORE UPDATE OR DELETE ON sharing.bill_revisions FOR EACH ROW EXECUTE FUNCTION sharing.protect_bill_revision_facts();
CREATE TRIGGER sharing_contributions_immutable BEFORE UPDATE OR DELETE ON sharing.contributions FOR EACH ROW EXECUTE FUNCTION sharing.reject_immutable_fact_mutation();
CREATE TRIGGER sharing_contribution_allocations_immutable BEFORE UPDATE OR DELETE ON sharing.contribution_journal_allocations FOR EACH ROW EXECUTE FUNCTION sharing.reject_immutable_fact_mutation();
CREATE TRIGGER sharing_participant_shares_immutable BEFORE UPDATE OR DELETE ON sharing.participant_shares FOR EACH ROW EXECUTE FUNCTION sharing.reject_immutable_fact_mutation();

CREATE FUNCTION sharing.protect_posted_settlement() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status = 'posted' THEN RAISE EXCEPTION 'Posted Sharing settlements are immutable; append a reversal'; END IF;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END; $$;
CREATE TRIGGER sharing_posted_settlements_immutable BEFORE UPDATE OR DELETE ON sharing.settlements FOR EACH ROW EXECUTE FUNCTION sharing.protect_posted_settlement();
CREATE FUNCTION sharing.protect_settlement_reversal_fact() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN RAISE EXCEPTION 'Sharing settlement reversals are immutable'; END IF;
    IF (OLD.settlement_id, OLD.user_id, OLD.reason, OLD.correlation_id, OLD.reversed_at)
       IS DISTINCT FROM
       (NEW.settlement_id, NEW.user_id, NEW.reason, NEW.correlation_id, NEW.reversed_at)
    THEN RAISE EXCEPTION 'Sharing settlement reversal facts are immutable'; END IF;
    IF OLD.ledger_reversal_journal_id IS NOT NULL AND OLD.ledger_reversal_journal_id IS DISTINCT FROM NEW.ledger_reversal_journal_id
    THEN RAISE EXCEPTION 'Sharing settlement reversal accounting is immutable'; END IF;
    RETURN NEW;
END; $$;
CREATE TRIGGER sharing_settlement_reversals_immutable BEFORE UPDATE OR DELETE ON sharing.settlement_reversals FOR EACH ROW EXECUTE FUNCTION sharing.protect_settlement_reversal_fact();
