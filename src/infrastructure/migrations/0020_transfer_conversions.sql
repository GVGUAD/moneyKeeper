-- Ledger owns conversion state; provider evidence remains in Banking.
CREATE TABLE ledger.transfer_conversions (
 id UUID NOT NULL, user_id UUID NOT NULL, version BIGINT NOT NULL CHECK(version>0),
 active BOOLEAN NOT NULL, document JSONB NOT NULL,
 PRIMARY KEY(id,user_id)
);
CREATE TABLE ledger.transfer_conversion_journals (
 user_id UUID NOT NULL, journal_id UUID NOT NULL, conversion_id UUID NOT NULL,
 role TEXT NOT NULL CHECK(role IN ('source','transfer','reversal','restoration')),
 PRIMARY KEY(user_id,journal_id,conversion_id),
 FOREIGN KEY(journal_id,user_id) REFERENCES ledger.journal_entries(id,user_id),
 FOREIGN KEY(conversion_id,user_id) REFERENCES ledger.transfer_conversions(id,user_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE UNIQUE INDEX one_transfer_conversion_owner ON ledger.transfer_conversion_journals(user_id,journal_id) WHERE role<>'restoration';
CREATE INDEX transfer_conversion_journal_lookup ON ledger.transfer_conversion_journals(user_id,conversion_id);
CREATE TABLE ledger.transfer_restorations (
 user_id UUID NOT NULL, original_id UUID NOT NULL, restored_id UUID NOT NULL,
 PRIMARY KEY(user_id,original_id),
 FOREIGN KEY(original_id,user_id) REFERENCES ledger.journal_entries(id,user_id),
 FOREIGN KEY(restored_id,user_id) REFERENCES ledger.journal_entries(id,user_id)
);

CREATE TABLE ledger.conversion_import_reviews (
 user_id UUID NOT NULL,id UUID NOT NULL,version BIGINT NOT NULL,state TEXT NOT NULL CHECK(state IN ('pending_review','confirmed','dismissed','cancelled')),
 document JSONB NOT NULL,PRIMARY KEY(user_id,id)
);

-- Ledger's transaction claim contract is also enforced at the persistence boundary,
-- so queued work and competing context commits cannot bypass conversion ownership.
CREATE TABLE ledger.workflow_journal_claims (
 user_id UUID NOT NULL,journal_id UUID NOT NULL,workflow TEXT NOT NULL,reference_id UUID NOT NULL,
 PRIMARY KEY(user_id,journal_id,workflow,reference_id)
);
CREATE FUNCTION ledger.claim_workflow_journal() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE j UUID; ref UUID;
BEGIN
 j := (to_jsonb(NEW)->>TG_ARGV[0])::uuid;
 ref := (to_jsonb(NEW)->>TG_ARGV[1])::uuid;
 IF j IS NULL THEN RETURN NEW; END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended(NEW.user_id::text,20420));
 IF EXISTS(SELECT 1 FROM ledger.transfer_conversion_journals c WHERE c.user_id=NEW.user_id AND c.journal_id=j AND c.role<>'restoration') THEN
   RAISE EXCEPTION 'transaction is managed by a transfer conversion' USING ERRCODE='23514';
 END IF;
 INSERT INTO ledger.workflow_journal_claims VALUES(NEW.user_id,j,TG_ARGV[2],ref) ON CONFLICT DO NOTHING;
 RETURN NEW;
END $$;
CREATE TRIGGER recurring_claim_ledger_transaction BEFORE INSERT ON recurring.match_allocations
 FOR EACH ROW EXECUTE FUNCTION ledger.claim_workflow_journal('journal_entry_id','match_id','recurring');
CREATE TRIGGER sharing_claim_ledger_transaction BEFORE INSERT ON sharing.contribution_journal_allocations
 FOR EACH ROW EXECUTE FUNCTION ledger.claim_workflow_journal('ledger_journal_id','contribution_id','sharing');
CREATE TRIGGER sharing_settlement_claim BEFORE INSERT OR UPDATE OF ledger_journal_id ON sharing.settlements
 FOR EACH ROW EXECUTE FUNCTION ledger.claim_workflow_journal('ledger_journal_id','id','sharing_settlement');
INSERT INTO ledger.workflow_journal_claims SELECT a.user_id,a.journal_entry_id,'recurring',a.match_id
 FROM recurring.match_allocations a WHERE NOT EXISTS(SELECT 1 FROM recurring.unmatches u WHERE u.user_id=a.user_id AND u.match_id=a.match_id) OR EXISTS(SELECT 1 FROM recurring.categorization_targets t WHERE t.user_id=a.user_id AND t.match_id=a.match_id AND t.journal_entry_id=a.journal_entry_id AND t.state IN ('pending','posted','retry_due','compensating'));
INSERT INTO ledger.workflow_journal_claims SELECT user_id,ledger_journal_id,'sharing',contribution_id FROM sharing.contribution_journal_allocations ON CONFLICT DO NOTHING;
INSERT INTO ledger.workflow_journal_claims SELECT user_id,ledger_journal_id,'sharing_settlement',id FROM sharing.settlements WHERE ledger_journal_id IS NOT NULL ON CONFLICT DO NOTHING;
CREATE FUNCTION ledger.release_recurring_claim() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 PERFORM pg_advisory_xact_lock(hashtextextended(NEW.user_id::text,20420));
 DELETE FROM ledger.workflow_journal_claims w WHERE w.user_id=NEW.user_id AND w.reference_id=NEW.match_id AND w.workflow='recurring' AND NOT EXISTS(SELECT 1 FROM recurring.categorization_targets t WHERE t.user_id=w.user_id AND t.match_id=w.reference_id AND t.journal_entry_id=w.journal_id AND t.state IN ('pending','posted','retry_due','compensating'));
 RETURN NEW;
END $$;
CREATE TRIGGER recurring_release_ledger_transaction AFTER INSERT ON recurring.unmatches
 FOR EACH ROW EXECUTE FUNCTION ledger.release_recurring_claim();
CREATE FUNCTION ledger.release_sharing_claims() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 PERFORM pg_advisory_xact_lock(hashtextextended(NEW.user_id::text,20420));
 IF NEW.status IN ('cancelled','active') THEN
   DELETE FROM ledger.workflow_journal_claims w USING sharing.contributions c
   WHERE w.user_id=NEW.user_id AND w.workflow='sharing' AND w.reference_id=c.id AND c.user_id=NEW.user_id AND c.bill_id=NEW.id AND (NEW.status='cancelled' OR c.revision<NEW.current_revision);
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER sharing_release_ledger_transaction AFTER UPDATE OF status ON sharing.bills
 FOR EACH ROW EXECUTE FUNCTION ledger.release_sharing_claims();
CREATE FUNCTION ledger.release_settlement_claim() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 PERFORM pg_advisory_xact_lock(hashtextextended(NEW.user_id::text,20420));
 DELETE FROM ledger.workflow_journal_claims WHERE user_id=NEW.user_id AND reference_id=NEW.settlement_id AND workflow='sharing_settlement';
 RETURN NEW;
END $$;
CREATE TRIGGER sharing_release_settlement_claim AFTER INSERT ON sharing.settlement_reversals
 FOR EACH ROW EXECUTE FUNCTION ledger.release_settlement_claim();
-- Ignore already completed cancellations during bootstrap.
DELETE FROM ledger.workflow_journal_claims w USING sharing.contributions c,sharing.bills b
 WHERE w.user_id=c.user_id AND w.reference_id=c.id AND w.workflow='sharing' AND b.id=c.bill_id AND b.user_id=c.user_id AND (b.status='cancelled' OR (b.status='active' AND c.revision<b.current_revision));
DELETE FROM ledger.workflow_journal_claims w USING sharing.settlement_reversals r
 WHERE w.user_id=r.user_id AND w.reference_id=r.settlement_id AND w.workflow='sharing_settlement';

-- Unmatch can precede the asynchronous restoration of its category assignment.
-- Keep the claim until no queued compensation can mutate the original metadata.
CREATE FUNCTION ledger.release_recurring_target_claim() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.state IN ('compensated','terminal_no_effect','review_required') AND EXISTS(SELECT 1 FROM recurring.unmatches u WHERE u.user_id=NEW.user_id AND u.match_id=NEW.match_id) THEN
   PERFORM pg_advisory_xact_lock(hashtextextended(NEW.user_id::text,20420));
   DELETE FROM ledger.workflow_journal_claims WHERE user_id=NEW.user_id AND journal_id=NEW.journal_entry_id AND reference_id=NEW.match_id AND workflow='recurring';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER recurring_release_completed_target AFTER UPDATE OF state ON recurring.categorization_targets
 FOR EACH ROW EXECUTE FUNCTION ledger.release_recurring_target_claim();
