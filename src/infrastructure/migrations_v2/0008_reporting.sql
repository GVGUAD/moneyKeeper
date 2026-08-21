-- Reporting owns rebuildable read models and no financial command tables.
CREATE TABLE reporting.consumed_events (
 consumer_name TEXT NOT NULL,event_id UUID NOT NULL,event_type TEXT NOT NULL,source_sequence BIGINT NOT NULL,payload_digest BYTEA NOT NULL CHECK(octet_length(payload_digest)=32),processed_at TIMESTAMPTZ NOT NULL,
 PRIMARY KEY(consumer_name,event_id)
);
CREATE TABLE reporting.checkpoints (consumer_name TEXT PRIMARY KEY,last_sequence BIGINT NOT NULL DEFAULT 0,updated_at TIMESTAMPTZ NOT NULL);
CREATE TABLE reporting.dead_letters (id UUID PRIMARY KEY,consumer_name TEXT NOT NULL,event_id UUID NOT NULL,event_type TEXT NOT NULL,reason VARCHAR(500) NOT NULL,recorded_at TIMESTAMPTZ NOT NULL,UNIQUE(consumer_name,event_id));
CREATE TABLE reporting.account_balances (
 user_id UUID NOT NULL,account_id UUID NOT NULL,currency VARCHAR(3) NOT NULL,account_kind TEXT NOT NULL,balance NUMERIC(28,8) NOT NULL,as_of TIMESTAMPTZ NOT NULL,source_sequence BIGINT NOT NULL,PRIMARY KEY(user_id,account_id)
);
CREATE TABLE reporting.balance_history (user_id UUID NOT NULL,account_id UUID NOT NULL,journal_entry_id UUID NOT NULL,currency VARCHAR(3) NOT NULL,balance NUMERIC(28,8) NOT NULL,effective_at TIMESTAMPTZ NOT NULL,source_sequence BIGINT NOT NULL,PRIMARY KEY(user_id,account_id,journal_entry_id));
CREATE TABLE reporting.cashflows (user_id UUID NOT NULL,journal_entry_id UUID NOT NULL,flow_kind TEXT NOT NULL,amount NUMERIC(28,8) NOT NULL,currency VARCHAR(3) NOT NULL,category_id UUID,effective_at TIMESTAMPTZ NOT NULL,reversed BOOLEAN NOT NULL DEFAULT false,source_sequence BIGINT NOT NULL,PRIMARY KEY(user_id,journal_entry_id,flow_kind));
CREATE TABLE reporting.reconciliations (user_id UUID NOT NULL,case_id UUID NOT NULL,state TEXT NOT NULL,case_version BIGINT NOT NULL,balance_version BIGINT NOT NULL,observation_sequence BIGINT NOT NULL,ledger_event_sequence BIGINT NOT NULL,event_id UUID NOT NULL,updated_at TIMESTAMPTZ NOT NULL,PRIMARY KEY(user_id,case_id));
CREATE TABLE reporting.reconciliation_history (user_id UUID NOT NULL,case_id UUID NOT NULL,state TEXT NOT NULL,case_version BIGINT NOT NULL,ledger_event_sequence BIGINT NOT NULL,event_id UUID NOT NULL,occurred_at TIMESTAMPTZ NOT NULL,PRIMARY KEY(event_id));
CREATE TABLE reporting.recurring_summary (user_id UUID NOT NULL,subscription_id UUID NOT NULL,currency VARCHAR(3) NOT NULL,total NUMERIC(28,8) NOT NULL,charge_count BIGINT NOT NULL,last_charge_at TIMESTAMPTZ,source_sequence BIGINT NOT NULL,PRIMARY KEY(user_id,subscription_id,currency));
CREATE TABLE reporting.fx_rates (observation_id UUID PRIMARY KEY,source TEXT NOT NULL,source_revision TEXT NOT NULL,base_currency VARCHAR(3) NOT NULL,quote_currency VARCHAR(3) NOT NULL,rate NUMERIC(28,12) NOT NULL,effective_at TIMESTAMPTZ NOT NULL,observed_at TIMESTAMPTZ NOT NULL,source_sequence BIGINT NOT NULL);
CREATE INDEX reporting_fx_as_of ON reporting.fx_rates(base_currency,quote_currency,effective_at DESC,observed_at DESC,source_sequence DESC);
CREATE TABLE reporting.bill_positions (user_id UUID NOT NULL,bill_id UUID NOT NULL,currency VARCHAR(3) NOT NULL,receivable NUMERIC(28,8) NOT NULL,payable NUMERIC(28,8) NOT NULL,source_sequence BIGINT NOT NULL,PRIMARY KEY(user_id,bill_id));
CREATE TABLE reporting.loan_summaries (user_id UUID NOT NULL,loan_id UUID NOT NULL,currency VARCHAR(3) NOT NULL,principal NUMERIC(28,8) NOT NULL,interest NUMERIC(28,8) NOT NULL,fees NUMERIC(28,8) NOT NULL,source_sequence BIGINT NOT NULL,PRIMARY KEY(user_id,loan_id));
CREATE TABLE reporting.portfolio_valuations (user_id UUID NOT NULL,portfolio_account_id UUID NOT NULL,currency VARCHAR(3) NOT NULL,value NUMERIC(28,8) NOT NULL,valued_at TIMESTAMPTZ NOT NULL,source_sequence BIGINT NOT NULL,PRIMARY KEY(user_id,portfolio_account_id,valued_at));
