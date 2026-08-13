//! Shared immutable-journal commit pipeline.

use super::super::domain::{JournalEntry, LedgerError, TransactionAnnotation};
use super::{
    accounts::append_journal_facts,
    ports::{AnnotationStore, AuditStore, JournalStore, LedgerOutboxStore, ProjectionStore},
};

/// Persists a complete journal aggregate, optional metadata, projections,
/// audit, and outbox facts without accepting a caller-mutated balance.
pub(super) async fn commit_journal<T>(
    tx: &mut T,
    command_name: &str,
    journal: &JournalEntry,
    annotation: Option<&TransactionAnnotation>,
    event_type: &'static str,
) -> Result<i64, LedgerError>
where
    T: JournalStore + AnnotationStore + ProjectionStore + AuditStore + LedgerOutboxStore,
{
    let sequence = tx.insert_journal(command_name, journal).await?;
    if let Some(annotation) = annotation {
        tx.insert_annotation(annotation).await?;
    }
    tx.apply_postings(journal).await?;
    append_journal_facts(tx, journal, event_type).await?;
    Ok(sequence)
}
