//! Ledger-owned aggregate identities.

crate::define_uuid_id!(
    /// Identifies a Ledger account.
    pub LedgerAccountId
);

crate::define_uuid_id!(
    /// Identifies an immutable journal entry.
    pub JournalEntryId
);

crate::define_uuid_id!(
    /// Identifies one immutable posting owned by a journal entry.
    pub PostingId
);

crate::define_uuid_id!(
    /// Identifies a transaction annotation aggregate.
    pub AnnotationId
);

crate::define_uuid_id!(
    /// Identifies a provider-neutral reconciliation case.
    pub ReconciliationCaseId
);

crate::define_uuid_id!(
    /// Identifies an external balance observation.
    pub ObservationId
);
