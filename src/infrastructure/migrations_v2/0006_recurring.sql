-- Recurring owns subscription inventory, evidence and append-only matching decisions.
CREATE FUNCTION recurring.reject_immutable_fact_mutation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'recurring facts are immutable'; END $$;
CREATE TABLE recurring.subscriptions (
 id UUID NOT NULL,user_id UUID NOT NULL,merchant TEXT NOT NULL CHECK(merchant<>''),status TEXT NOT NULL CHECK(status IN ('active','paused','cancelled')),
 cadence TEXT NOT NULL CHECK(cadence IN ('weekly','monthly','quarterly','yearly','irregular')),category_id UUID,
 expected_amount NUMERIC(28,8),currency VARCHAR(3),next_expected_at TIMESTAMPTZ,version BIGINT NOT NULL CHECK(version>0),
 created_at TIMESTAMPTZ NOT NULL,updated_at TIMESTAMPTZ NOT NULL,PRIMARY KEY(id,user_id)
);
CREATE UNIQUE INDEX recurring_active_merchant
 ON recurring.subscriptions(user_id,lower(merchant)) WHERE status<>'cancelled';
CREATE TABLE recurring.charge_evidence (
 id UUID NOT NULL,user_id UUID NOT NULL,subscription_id UUID,source_context TEXT NOT NULL,source_evidence_id UUID NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('renewal','one_time','refund','cancellation')),merchant TEXT NOT NULL,amount NUMERIC(28,8),currency VARCHAR(3),
 charged_at TIMESTAMPTZ,recorded_at TIMESTAMPTZ NOT NULL,PRIMARY KEY(id,user_id),UNIQUE(user_id,source_context,source_evidence_id),
 FOREIGN KEY(subscription_id,user_id) REFERENCES recurring.subscriptions(id,user_id)
);
CREATE TABLE recurring.charge_matching (
 evidence_id UUID NOT NULL,user_id UUID NOT NULL,version BIGINT NOT NULL DEFAULT 0 CHECK(version>=0),allocated_amount NUMERIC(28,8) NOT NULL DEFAULT 0,
 state TEXT NOT NULL CHECK(state IN ('undecided','partially_matched','matched','rejected')),updated_at TIMESTAMPTZ NOT NULL,
 PRIMARY KEY(evidence_id,user_id),FOREIGN KEY(evidence_id,user_id) REFERENCES recurring.charge_evidence(id,user_id)
);
CREATE TABLE recurring.match_records (
 id UUID NOT NULL,user_id UUID NOT NULL,evidence_id UUID NOT NULL,matching_version BIGINT NOT NULL CHECK(matching_version>0),decision_source TEXT NOT NULL,
 category_id UUID,created_at TIMESTAMPTZ NOT NULL,PRIMARY KEY(id,user_id),UNIQUE(evidence_id,matching_version),
 FOREIGN KEY(evidence_id,user_id) REFERENCES recurring.charge_matching(evidence_id,user_id)
);
CREATE TABLE recurring.match_allocations (
 match_id UUID NOT NULL,user_id UUID NOT NULL,journal_entry_id UUID NOT NULL,amount NUMERIC(28,8) NOT NULL CHECK(amount>0),currency VARCHAR(3) NOT NULL,
 PRIMARY KEY(match_id,user_id,journal_entry_id),FOREIGN KEY(match_id,user_id) REFERENCES recurring.match_records(id,user_id)
);
CREATE TABLE recurring.rejections (
 id UUID NOT NULL,user_id UUID NOT NULL,evidence_id UUID NOT NULL,matching_version BIGINT NOT NULL,reason TEXT NOT NULL,recorded_at TIMESTAMPTZ NOT NULL,
 PRIMARY KEY(id,user_id),UNIQUE(evidence_id,matching_version),FOREIGN KEY(evidence_id,user_id) REFERENCES recurring.charge_matching(evidence_id,user_id)
);
CREATE TABLE recurring.unmatches (
 id UUID NOT NULL,user_id UUID NOT NULL,evidence_id UUID NOT NULL,match_id UUID NOT NULL,matching_version BIGINT NOT NULL,recorded_at TIMESTAMPTZ NOT NULL,
 PRIMARY KEY(id,user_id),UNIQUE(user_id,match_id),UNIQUE(evidence_id,matching_version),
 FOREIGN KEY(evidence_id,user_id) REFERENCES recurring.charge_matching(evidence_id,user_id),FOREIGN KEY(match_id,user_id) REFERENCES recurring.match_records(id,user_id)
);
CREATE TABLE recurring.ledger_candidates (
 journal_entry_id UUID NOT NULL,user_id UUID NOT NULL,event_sequence BIGINT NOT NULL,amount NUMERIC(28,8) NOT NULL,currency VARCHAR(3) NOT NULL,
 merchant TEXT,category_id UUID,reversed BOOLEAN NOT NULL DEFAULT false,occurred_at TIMESTAMPTZ NOT NULL,PRIMARY KEY(journal_entry_id,user_id)
);
CREATE TABLE recurring.categorization_processes (
 match_id UUID NOT NULL,user_id UUID NOT NULL,state TEXT NOT NULL CHECK(state IN ('pending','posted','retry_due','terminal_no_effect','compensating','compensated','review_required')),
 process_generation BIGINT NOT NULL,prior_category_id UUID,prior_annotation_version BIGINT,produced_annotation_version BIGINT,
 attempts INTEGER NOT NULL DEFAULT 0,next_retry_at TIMESTAMPTZ,last_error TEXT,updated_at TIMESTAMPTZ NOT NULL,PRIMARY KEY(match_id,user_id),
 FOREIGN KEY(match_id,user_id) REFERENCES recurring.match_records(id,user_id)
);
CREATE TABLE recurring.categorization_targets (
 match_id UUID NOT NULL,user_id UUID NOT NULL,journal_entry_id UUID NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('pending','posted','retry_due','terminal_no_effect','compensating','compensated','review_required')),
 process_generation BIGINT NOT NULL,prior_category_id UUID,prior_annotation_version BIGINT,produced_annotation_version BIGINT,
 attempts INTEGER NOT NULL DEFAULT 0,next_retry_at TIMESTAMPTZ,last_error VARCHAR(500),
 lease_holder TEXT,lease_expires_at TIMESTAMPTZ,lease_token BIGINT NOT NULL DEFAULT 0,
 updated_at TIMESTAMPTZ NOT NULL,PRIMARY KEY(match_id,user_id,journal_entry_id),
 FOREIGN KEY(match_id,user_id,journal_entry_id) REFERENCES recurring.match_allocations(match_id,user_id,journal_entry_id),
 CHECK((lease_holder IS NULL)=(lease_expires_at IS NULL))
);
CREATE INDEX recurring_categorization_due ON recurring.categorization_targets(next_retry_at,updated_at,match_id,journal_entry_id)
 WHERE state IN ('pending','retry_due','compensating');
CREATE TABLE recurring.command_receipts (
 user_id UUID NOT NULL,command_scope TEXT NOT NULL,idempotency_key TEXT NOT NULL,command_name TEXT NOT NULL,target_id UUID,request_hash BYTEA NOT NULL CHECK(octet_length(request_hash)=32),
 status TEXT NOT NULL CHECK(status IN ('processing','succeeded','rejected','failed')),http_status SMALLINT,response_body JSONB,aggregate_id UUID,aggregate_version BIGINT,
 created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),completed_at TIMESTAMPTZ,PRIMARY KEY(user_id,command_scope,idempotency_key),
 CHECK((status='processing')=(completed_at IS NULL))
);
CREATE TABLE recurring.consumed_events (consumer_name TEXT NOT NULL,event_id UUID NOT NULL,event_type TEXT NOT NULL,sequence BIGINT,payload_digest BYTEA NOT NULL CHECK(octet_length(payload_digest)=32),processed_at TIMESTAMPTZ NOT NULL,PRIMARY KEY(consumer_name,event_id));
CREATE TRIGGER charge_evidence_immutable BEFORE UPDATE OR DELETE ON recurring.charge_evidence FOR EACH ROW EXECUTE FUNCTION recurring.reject_immutable_fact_mutation();
CREATE TRIGGER match_records_immutable BEFORE UPDATE OR DELETE ON recurring.match_records FOR EACH ROW EXECUTE FUNCTION recurring.reject_immutable_fact_mutation();
CREATE TRIGGER match_allocations_immutable BEFORE UPDATE OR DELETE ON recurring.match_allocations FOR EACH ROW EXECUTE FUNCTION recurring.reject_immutable_fact_mutation();
CREATE TRIGGER rejections_immutable BEFORE UPDATE OR DELETE ON recurring.rejections FOR EACH ROW EXECUTE FUNCTION recurring.reject_immutable_fact_mutation();
CREATE TRIGGER unmatches_immutable BEFORE UPDATE OR DELETE ON recurring.unmatches FOR EACH ROW EXECUTE FUNCTION recurring.reject_immutable_fact_mutation();
