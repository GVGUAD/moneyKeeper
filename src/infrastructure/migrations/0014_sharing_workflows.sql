-- Durable Sharing accounting fan-out and Ledger-backed transaction allocation.

CREATE TABLE ledger.reclassification_details (
    journal_entry_id UUID NOT NULL,
    user_id UUID NOT NULL,
    source_journal_entry_id UUID NOT NULL,
    source_nature TEXT NOT NULL CHECK (source_nature IN ('expense', 'income')),
    amount ledger.numeric_28_8 NOT NULL CHECK (amount > 0),
    currency VARCHAR(3) NOT NULL CHECK (currency COLLATE "C" ~ '^[A-Z]{3}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (journal_entry_id, user_id),
    FOREIGN KEY (journal_entry_id, user_id)
        REFERENCES ledger.journal_entries(id, user_id),
    FOREIGN KEY (source_journal_entry_id, user_id)
        REFERENCES ledger.journal_entries(id, user_id)
);
CREATE INDEX ledger_reclassification_source
    ON ledger.reclassification_details(user_id, source_journal_entry_id, source_nature);
CREATE TRIGGER reclassification_details_are_immutable
BEFORE UPDATE OR DELETE ON ledger.reclassification_details
FOR EACH ROW EXECUTE FUNCTION ledger.reject_immutable_financial_mutation();

CREATE TABLE sharing.bill_revision_accounting_journals (
    bill_id UUID NOT NULL,
    user_id UUID NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    position INTEGER NOT NULL CHECK (position >= 0),
    ledger_journal_id UUID NOT NULL,
    ledger_reversal_journal_id UUID,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (bill_id, user_id, revision, position),
    UNIQUE (user_id, ledger_journal_id),
    FOREIGN KEY (bill_id, user_id, revision)
        REFERENCES sharing.bill_revisions(bill_id, user_id, revision)
);

-- Preserve accounting written before this migration.
INSERT INTO sharing.bill_revision_accounting_journals(
    bill_id, user_id, revision, position, ledger_journal_id,
    ledger_reversal_journal_id
)
SELECT bill_id, user_id, revision, 0, accounting_journal_id,
       accounting_reversal_journal_id
FROM sharing.bill_revisions
WHERE accounting_journal_id IS NOT NULL;

CREATE INDEX sharing_bill_accounting_journals_revision
    ON sharing.bill_revision_accounting_journals(user_id, bill_id, revision, position);
CREATE UNIQUE INDEX sharing_bill_accounting_reversal_journal
    ON sharing.bill_revision_accounting_journals(user_id, ledger_reversal_journal_id)
    WHERE ledger_reversal_journal_id IS NOT NULL;
