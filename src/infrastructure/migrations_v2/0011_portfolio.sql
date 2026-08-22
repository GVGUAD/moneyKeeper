-- Portfolio owns security facts and valuation. Ledger identities remain opaque
-- UUIDs and no Portfolio table references a foreign context.
CREATE TABLE portfolio.instruments (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    identifier_kind TEXT NOT NULL CHECK (identifier_kind IN ('isin','manual')),
    identifier VARCHAR(100) NOT NULL CHECK (identifier = BTRIM(identifier) AND identifier <> ''),
    instrument_type TEXT NOT NULL CHECK (instrument_type = 'ovdp'),
    issuer_type TEXT NOT NULL CHECK (issuer_type = 'sovereign_bond'),
    display_name VARCHAR(300) NOT NULL CHECK (display_name = BTRIM(display_name) AND display_name <> ''),
    currency VARCHAR(3) NOT NULL CHECK (currency COLLATE "C" ~ '^[A-Z]{3}$'),
    face_value NUMERIC(28,8) NOT NULL CHECK (face_value > 0),
    issue_date DATE NOT NULL,
    maturity_date DATE NOT NULL,
    coupon_kind TEXT NOT NULL CHECK (coupon_kind IN ('fixed','zero_coupon','unknown')),
    coupon_rate NUMERIC(18,10),
    source TEXT NOT NULL CHECK (source = 'manual'),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (id),
    UNIQUE (id, user_id),
    UNIQUE (user_id, identifier_kind, identifier),
    CHECK (issue_date <= maturity_date),
    CHECK ((coupon_kind = 'fixed') = (coupon_rate IS NOT NULL)),
    CHECK (coupon_rate IS NULL OR coupon_rate >= 0)
);

CREATE TABLE portfolio.accounts (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    name VARCHAR(200) NOT NULL CHECK (name = BTRIM(name) AND name <> ''),
    lifecycle TEXT NOT NULL DEFAULT 'active' CHECK (lifecycle IN ('active','archived')),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (id),
    UNIQUE (id, user_id)
);

CREATE TABLE portfolio.command_receipts (
    user_id UUID NOT NULL,
    command_scope VARCHAR(120) NOT NULL CHECK (command_scope = BTRIM(command_scope) AND command_scope <> ''),
    idempotency_key VARCHAR(200) NOT NULL CHECK (idempotency_key = BTRIM(idempotency_key) AND idempotency_key <> '' AND idempotency_key !~ '[[:cntrl:]]'),
    canonical_request_hash BYTEA NOT NULL CHECK (octet_length(canonical_request_hash) = 32),
    status TEXT NOT NULL CHECK (status IN ('processing','completed','failed')),
    status_code SMALLINT CHECK (status_code BETWEEN 100 AND 599),
    durable_result JSONB,
    aggregate_id UUID,
    aggregate_version BIGINT CHECK (aggregate_version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (user_id, command_scope, idempotency_key),
    CHECK ((status = 'processing') = (completed_at IS NULL)),
    CHECK ((status = 'processing') = (durable_result IS NULL)),
    CHECK ((status = 'processing') = (status_code IS NULL))
);

CREATE TABLE portfolio.transactions (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    account_id UUID NOT NULL,
    instrument_id UUID NOT NULL,
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    position_version BIGINT NOT NULL CHECK (position_version > 0),
    kind TEXT NOT NULL CHECK (kind IN ('opening_position','buy','sell','coupon','redemption','position_correction','reversal')),
    status TEXT NOT NULL DEFAULT 'posted' CHECK (status = 'posted'),
    quantity NUMERIC(28,8) NOT NULL,
    currency VARCHAR(3) NOT NULL CHECK (currency COLLATE "C" ~ '^[A-Z]{3}$'),
    source TEXT NOT NULL CHECK (source IN ('manual','correction','reversal')),
    reason VARCHAR(1000),
    reversal_of UUID,
    actor_id UUID NOT NULL,
    correlation_id UUID NOT NULL,
    effective_at TIMESTAMPTZ NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (id),
    UNIQUE (id, user_id),
    UNIQUE (account_id, instrument_id, user_id, sequence),
    UNIQUE (reversal_of, user_id),
    FOREIGN KEY (account_id, user_id) REFERENCES portfolio.accounts(id, user_id),
    FOREIGN KEY (instrument_id, user_id) REFERENCES portfolio.instruments(id, user_id),
    FOREIGN KEY (reversal_of, user_id) REFERENCES portfolio.transactions(id, user_id),
    CHECK (quantity <> 0 OR kind IN ('coupon','reversal')),
    CHECK (quantity = TRUNC(quantity)),
    CHECK ((kind = 'reversal') = (reversal_of IS NOT NULL)),
    CHECK (kind NOT IN ('opening_position','buy','sell','redemption') OR quantity > 0),
    CHECK (kind <> 'position_correction' OR (reason IS NOT NULL AND reason = BTRIM(reason) AND reason <> ''))
);

CREATE TABLE portfolio.transaction_components (
    id UUID NOT NULL,
    transaction_id UUID NOT NULL,
    user_id UUID NOT NULL,
    component_kind TEXT NOT NULL CHECK (component_kind IN ('acquisition_cost','proceeds','fee','accrued_interest','coupon','cost_delta')),
    amount NUMERIC(28,8),
    currency VARCHAR(3) NOT NULL CHECK (currency COLLATE "C" ~ '^[A-Z]{3}$'),
    cost_known BOOLEAN NOT NULL DEFAULT TRUE,
    PRIMARY KEY (id),
    UNIQUE (transaction_id, user_id, component_kind),
    FOREIGN KEY (transaction_id, user_id) REFERENCES portfolio.transactions(id, user_id),
    CHECK ((cost_known AND amount IS NOT NULL) OR (NOT cost_known AND amount IS NULL)),
    CHECK (amount IS NULL OR amount BETWEEN -99999999999999999999.99999999 AND 99999999999999999999.99999999)
);

CREATE TABLE portfolio.position_lots (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    account_id UUID NOT NULL,
    instrument_id UUID NOT NULL,
    source_transaction_id UUID NOT NULL,
    original_quantity NUMERIC(28,8) NOT NULL CHECK (original_quantity > 0 AND original_quantity = TRUNC(original_quantity)),
    remaining_quantity NUMERIC(28,8) NOT NULL CHECK (remaining_quantity >= 0 AND remaining_quantity <= original_quantity AND remaining_quantity = TRUNC(remaining_quantity)),
    original_cost NUMERIC(28,8),
    remaining_cost NUMERIC(28,8),
    currency VARCHAR(3) NOT NULL CHECK (currency COLLATE "C" ~ '^[A-Z]{3}$'),
    acquired_at TIMESTAMPTZ NOT NULL,
    created_sequence BIGINT NOT NULL CHECK (created_sequence > 0),
    PRIMARY KEY (id),
    UNIQUE (id, user_id),
    FOREIGN KEY (account_id, user_id) REFERENCES portfolio.accounts(id, user_id),
    FOREIGN KEY (instrument_id, user_id) REFERENCES portfolio.instruments(id, user_id),
    FOREIGN KEY (source_transaction_id, user_id) REFERENCES portfolio.transactions(id, user_id),
    CHECK ((original_cost IS NULL) = (remaining_cost IS NULL)),
    CHECK (original_cost IS NULL OR (original_cost >= 0 AND remaining_cost >= 0 AND remaining_cost <= original_cost))
);

CREATE TABLE portfolio.lot_allocations (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    disposal_transaction_id UUID NOT NULL,
    lot_id UUID NOT NULL,
    quantity NUMERIC(28,8) NOT NULL CHECK (quantity > 0 AND quantity = TRUNC(quantity)),
    allocated_cost NUMERIC(28,8),
    currency VARCHAR(3) NOT NULL CHECK (currency COLLATE "C" ~ '^[A-Z]{3}$'),
    reverses_allocation_id UUID,
    recorded_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (id),
    UNIQUE (id, user_id),
    UNIQUE (disposal_transaction_id, user_id, lot_id),
    UNIQUE (reverses_allocation_id, user_id),
    FOREIGN KEY (disposal_transaction_id, user_id) REFERENCES portfolio.transactions(id, user_id),
    FOREIGN KEY (lot_id, user_id) REFERENCES portfolio.position_lots(id, user_id),
    FOREIGN KEY (reverses_allocation_id, user_id) REFERENCES portfolio.lot_allocations(id, user_id),
    CHECK (reverses_allocation_id IS NULL OR allocated_cost IS NOT NULL)
);

CREATE TABLE portfolio.position_projection (
    user_id UUID NOT NULL,
    account_id UUID NOT NULL,
    instrument_id UUID NOT NULL,
    quantity NUMERIC(28,8) NOT NULL CHECK (quantity >= 0),
    known_cost_quantity NUMERIC(28,8) NOT NULL CHECK (known_cost_quantity >= 0),
    unknown_cost_quantity NUMERIC(28,8) NOT NULL CHECK (unknown_cost_quantity >= 0),
    remaining_known_cost NUMERIC(28,8) NOT NULL CHECK (remaining_known_cost >= 0),
    realized_proceeds NUMERIC(28,8) NOT NULL DEFAULT 0,
    realized_allocated_cost NUMERIC(28,8) NOT NULL DEFAULT 0,
    realized_fees NUMERIC(28,8) NOT NULL DEFAULT 0,
    realized_gain_loss NUMERIC(28,8),
    currency VARCHAR(3) NOT NULL CHECK (currency COLLATE "C" ~ '^[A-Z]{3}$'),
    version BIGINT NOT NULL CHECK (version >= 0),
    source_sequence BIGINT NOT NULL DEFAULT 0 CHECK (source_sequence >= 0),
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (user_id, account_id, instrument_id),
    FOREIGN KEY (account_id, user_id) REFERENCES portfolio.accounts(id, user_id),
    FOREIGN KEY (instrument_id, user_id) REFERENCES portfolio.instruments(id, user_id),
    CHECK (known_cost_quantity + unknown_cost_quantity = quantity)
);

CREATE TABLE portfolio.valuation_snapshots (
    id UUID NOT NULL,
    user_id UUID NOT NULL,
    account_id UUID NOT NULL,
    instrument_id UUID NOT NULL,
    price_per_instrument NUMERIC(28,8) NOT NULL CHECK (price_per_instrument > 0),
    accrued_interest_per_instrument NUMERIC(28,8) NOT NULL DEFAULT 0 CHECK (accrued_interest_per_instrument >= 0),
    currency VARCHAR(3) NOT NULL CHECK (currency COLLATE "C" ~ '^[A-Z]{3}$'),
    quote_convention TEXT NOT NULL DEFAULT 'absolute_per_instrument' CHECK (quote_convention = 'absolute_per_instrument'),
    source VARCHAR(200) NOT NULL CHECK (source = BTRIM(source) AND source <> ''),
    quoted_at TIMESTAMPTZ NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL,
    event_sequence BIGINT NOT NULL CHECK (event_sequence > 0),
    PRIMARY KEY (id),
    UNIQUE (id, user_id),
    FOREIGN KEY (account_id, user_id) REFERENCES portfolio.accounts(id, user_id),
    FOREIGN KEY (instrument_id, user_id) REFERENCES portfolio.instruments(id, user_id)
);

CREATE TABLE portfolio.latest_valuation_projection (
    user_id UUID NOT NULL,
    account_id UUID NOT NULL,
    instrument_id UUID NOT NULL,
    snapshot_id UUID NOT NULL,
    quantity NUMERIC(28,8) NOT NULL CHECK (quantity >= 0),
    price_per_instrument NUMERIC(28,8) NOT NULL,
    accrued_interest_per_instrument NUMERIC(28,8) NOT NULL,
    market_value NUMERIC(28,8) NOT NULL CHECK (market_value >= 0),
    currency VARCHAR(3) NOT NULL,
    as_of TIMESTAMPTZ NOT NULL,
    source_sequence BIGINT NOT NULL,
    PRIMARY KEY (user_id, account_id, instrument_id),
    FOREIGN KEY (snapshot_id, user_id) REFERENCES portfolio.valuation_snapshots(id, user_id)
);

CREATE TABLE portfolio.cash_settlement_processes (
    transaction_id UUID NOT NULL,
    user_id UUID NOT NULL,
    generation BIGINT NOT NULL DEFAULT 1 CHECK (generation > 0),
    action TEXT NOT NULL CHECK (action IN ('post','cancel_or_reverse')),
    cash_flow TEXT CHECK (cash_flow IN ('incoming','outgoing')),
    state TEXT NOT NULL CHECK (state IN ('pending','posted','retrying','failed','cancelled_no_financial_effect','reversed')),
    cash_account_id UUID,
    amount NUMERIC(28,8),
    currency VARCHAR(3),
    correlation_id UUID NOT NULL,
    ledger_journal_id UUID,
    ledger_reversal_id UUID,
    last_error VARCHAR(1000),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    lease_owner VARCHAR(200),
    lease_expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (transaction_id, user_id, generation),
    FOREIGN KEY (transaction_id, user_id) REFERENCES portfolio.transactions(id, user_id),
    CHECK ((cash_account_id IS NULL) = (amount IS NULL)),
    CHECK ((cash_account_id IS NULL) = (cash_flow IS NULL)),
    CHECK (amount IS NULL OR amount > 0),
    CHECK (state NOT IN ('posted','reversed') OR ledger_journal_id IS NOT NULL),
    CHECK ((state = 'reversed' AND ledger_reversal_id IS NOT NULL)
        OR (state <> 'reversed' AND ledger_reversal_id IS NULL))
);

CREATE TABLE portfolio.audit_log (
    sequence BIGSERIAL PRIMARY KEY,
    user_id UUID NOT NULL,
    aggregate_type VARCHAR(100) NOT NULL,
    aggregate_id UUID NOT NULL,
    aggregate_version BIGINT NOT NULL CHECK (aggregate_version > 0),
    action VARCHAR(100) NOT NULL,
    correlation_id UUID NOT NULL,
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    recorded_at TIMESTAMPTZ NOT NULL
);

-- Reporting owns these rebuildable event-fed projections. It never queries
-- Portfolio private tables.
CREATE TABLE reporting.portfolio_positions (
    user_id UUID NOT NULL,
    account_id UUID NOT NULL,
    instrument_id UUID NOT NULL,
    quantity NUMERIC(28,8) NOT NULL,
    known_cost_quantity NUMERIC(28,8) NOT NULL,
    unknown_cost_quantity NUMERIC(28,8) NOT NULL,
    remaining_known_cost NUMERIC(28,8) NOT NULL,
    realized_gain_loss NUMERIC(28,8),
    market_value NUMERIC(28,8),
    currency VARCHAR(3) NOT NULL,
    valuation_as_of TIMESTAMPTZ,
    valuation_event_id UUID,
    source_sequence BIGINT NOT NULL,
    PRIMARY KEY (user_id, account_id, instrument_id)
);
CREATE TABLE reporting.portfolio_activity_history (
    event_id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    account_id UUID,
    instrument_id UUID,
    transaction_id UUID,
    event_kind VARCHAR(100) NOT NULL,
    correlation_id UUID NOT NULL,
    payload JSONB NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    source_sequence BIGINT NOT NULL
);
CREATE INDEX reporting_portfolio_activity_order ON reporting.portfolio_activity_history(user_id, occurred_at, source_sequence, event_id);

CREATE INDEX portfolio_transaction_activity_idx ON portfolio.transactions(user_id, effective_at DESC, sequence DESC, id DESC);
CREATE INDEX portfolio_lot_fifo_idx ON portfolio.position_lots(account_id, instrument_id, acquired_at, created_sequence, id);
CREATE INDEX portfolio_cash_process_claim_idx ON portfolio.cash_settlement_processes(state, lease_expires_at, updated_at, transaction_id);
CREATE INDEX portfolio_valuation_order_idx ON portfolio.valuation_snapshots(user_id, account_id, instrument_id, quoted_at DESC, event_sequence DESC, id DESC);

CREATE FUNCTION portfolio.reject_immutable_fact_change() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'Portfolio facts are immutable; append a correction or reversal'; END; $$;
CREATE TRIGGER portfolio_transactions_immutable BEFORE UPDATE OR DELETE ON portfolio.transactions FOR EACH ROW EXECUTE FUNCTION portfolio.reject_immutable_fact_change();
CREATE TRIGGER portfolio_components_immutable BEFORE UPDATE OR DELETE ON portfolio.transaction_components FOR EACH ROW EXECUTE FUNCTION portfolio.reject_immutable_fact_change();
CREATE TRIGGER portfolio_allocations_immutable BEFORE UPDATE OR DELETE ON portfolio.lot_allocations FOR EACH ROW EXECUTE FUNCTION portfolio.reject_immutable_fact_change();
CREATE TRIGGER portfolio_valuations_immutable BEFORE UPDATE OR DELETE ON portfolio.valuation_snapshots FOR EACH ROW EXECUTE FUNCTION portfolio.reject_immutable_fact_change();
