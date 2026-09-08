-- Ledger-owned category assignment provenance and category-only Reporting projections.

ALTER TABLE ledger.transaction_annotations
    ADD COLUMN assignment_origin TEXT,
    ADD COLUMN classification_decision_id UUID,
    ADD COLUMN automation_state TEXT NOT NULL DEFAULT 'legacy_unknown';

ALTER TABLE ledger.transaction_annotations
    ADD CONSTRAINT annotation_assignment_origin_valid
        CHECK (assignment_origin IS NULL OR assignment_origin IN ('manual', 'recurring', 'ai')),
    ADD CONSTRAINT annotation_automation_state_valid
        CHECK (automation_state IN ('eligible', 'suppressed', 'legacy_unknown')),
    ADD CONSTRAINT annotation_ai_decision_consistent
        CHECK (
            (assignment_origin <> 'ai' OR classification_decision_id IS NOT NULL)
            AND (assignment_origin <> 'recurring' OR classification_decision_id IS NULL)
            AND (assignment_origin IS NOT NULL OR classification_decision_id IS NULL)
        );

-- A category that predates provenance is a user choice unless a completed,
-- version-matched Recurring target proves otherwise.
UPDATE ledger.transaction_annotations
SET assignment_origin = 'manual', automation_state = 'suppressed'
WHERE category_id IS NOT NULL;

UPDATE ledger.transaction_annotations a
SET assignment_origin = 'recurring', automation_state = 'eligible'
FROM recurring.categorization_targets t
WHERE t.user_id = a.user_id
  AND t.journal_entry_id = a.journal_entry_id
  AND t.state = 'posted'
  AND t.produced_annotation_version = a.version
  AND a.category_id IS NOT NULL;

-- Install cross-column invariants only after legacy rows have been classified.
ALTER TABLE ledger.transaction_annotations
    ADD CONSTRAINT annotation_assignment_category_consistent
        CHECK (
            (category_id IS NULL AND (assignment_origin IS NULL OR assignment_origin = 'manual'))
            OR (category_id IS NOT NULL AND assignment_origin IS NOT NULL)
        ),
    ADD CONSTRAINT annotation_assignment_automation_consistent
        CHECK (
            assignment_origin IS NULL
            OR (assignment_origin = 'manual' AND automation_state = 'suppressed')
            OR (assignment_origin IN ('recurring','ai') AND automation_state = 'eligible')
        );

-- Existing provider imports were the only ordinary cash-flow journals that did
-- not have mutable metadata. The journal id is a deterministic annotation id.
INSERT INTO ledger.transaction_annotations (
    id, journal_entry_id, user_id, description, category_id, assignment_origin,
    classification_decision_id, automation_state, note, tags, budget_visibility,
    version, created_at, updated_at
)
SELECT
    j.id, j.id, j.user_id, j.description, NULL, NULL, NULL, 'eligible',
    NULL, '{}', 'included', 1, j.recorded_at, j.recorded_at
FROM ledger.journal_entries j
WHERE j.command_name = 'import_provider_transaction'
  AND j.source = 'import'
  AND j.purpose = 'ordinary'
  AND NOT EXISTS (
      SELECT 1
      FROM ledger.transaction_annotations a
      WHERE a.user_id = j.user_id AND a.journal_entry_id = j.id
  );

ALTER TABLE recurring.categorization_targets
    ADD COLUMN prior_assignment_origin TEXT,
    ADD COLUMN prior_classification_decision_id UUID,
    ADD COLUMN prior_automation_state TEXT,
    ADD COLUMN apply_command_occurred_at TIMESTAMPTZ,
    ADD COLUMN compensation_command_occurred_at TIMESTAMPTZ;

ALTER TABLE recurring.categorization_targets
    ADD CONSTRAINT recurring_prior_assignment_origin_valid
        CHECK (prior_assignment_origin IS NULL OR prior_assignment_origin IN ('manual', 'recurring', 'ai')),
    ADD CONSTRAINT recurring_prior_automation_state_valid
        CHECK (prior_automation_state IS NULL OR prior_automation_state IN ('eligible', 'suppressed', 'legacy_unknown'));

ALTER TABLE recurring.categorization_processes
    ADD COLUMN prior_assignment_origin TEXT,
    ADD COLUMN prior_classification_decision_id UUID,
    ADD COLUMN prior_automation_state TEXT;

UPDATE recurring.categorization_targets
SET prior_assignment_origin = CASE WHEN prior_category_id IS NOT NULL THEN 'manual' END,
    prior_automation_state = CASE WHEN prior_category_id IS NOT NULL THEN 'suppressed' ELSE 'legacy_unknown' END
WHERE prior_annotation_version IS NOT NULL;

ALTER TABLE reporting.cashflows
    ADD COLUMN category_annotation_version BIGINT NOT NULL DEFAULT 0 CHECK (category_annotation_version >= 0),
    ADD COLUMN category_source_sequence BIGINT NOT NULL DEFAULT 0 CHECK (category_source_sequence >= 0);

-- Bring rebuildable Reporting category values in line without replaying money.
UPDATE reporting.cashflows c
SET category_id = a.category_id,
    category_annotation_version = a.version
FROM ledger.transaction_annotations a
WHERE a.user_id = c.user_id
  AND a.journal_entry_id = c.journal_entry_id;
