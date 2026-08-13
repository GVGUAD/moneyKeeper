//! Versioned transaction annotation commands.

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::contexts::classification::public::{CategoryCatalog, CategoryId};
use crate::shared_kernel::{Clock, EventId};

use super::super::{
    domain::{Actor, LedgerError},
    public::{AnnotationResult, UpdateTransactionAnnotation},
};
use super::{
    accounts::{LedgerFacade, integration_event},
    ports::{
        AnnotationStore, AuditRecord, AuditStore, CommandReceiptStore, LedgerOutboxStore,
        LedgerUnitOfWork, TransactionControl,
    },
};

impl LedgerFacade {
    /// Updates transaction metadata without mutating its journal or postings.
    pub async fn update_annotation(
        &self,
        command: UpdateTransactionAnnotation,
    ) -> Result<AnnotationResult, LedgerError> {
        let categories = self.categories.as_ref().ok_or_else(|| {
            LedgerError::persistence("Classification catalog is not configured for Ledger")
        })?;
        update_annotation(&self.uow, self.clock.as_ref(), categories, command).await
    }
}

async fn update_annotation<U: LedgerUnitOfWork, C: CategoryCatalog>(
    uow: &U,
    clock: &dyn Clock,
    categories: &C,
    command: UpdateTransactionAnnotation,
) -> Result<AnnotationResult, LedgerError> {
    if let Some(Some(reference)) = command.changes.category {
        categories
            .require_active(command.user_id, CategoryId::new(reference.into_uuid()))
            .await
            .map_err(|_| LedgerError::invalid_annotation("category is missing or archived"))?;
    }
    let request = json!({
        "journal_entry_id": command.journal_entry_id,
        "description": command.changes.description,
        "category": command.changes.category.map(|value| value.map(|id| id.into_uuid())),
        "note": command.changes.note,
        "tags": command.changes.tags,
        "budget_visibility": command.changes.budget_visibility,
        "expected_version": command.expected_version,
        "occurred_at": command.occurred_at,
    });
    let hash: [u8; 32] = Sha256::digest(
        serde_json::to_vec(&request)
            .map_err(|error| LedgerError::persistence(error.to_string()))?,
    )
    .into();
    let mut tx = uow.begin().await?;
    if let Some(receipt) = tx
        .find_receipt(
            command.user_id,
            "update_annotation",
            &command.idempotency_key,
            true,
        )
        .await?
    {
        if receipt.request_hash != hash {
            return Err(LedgerError::idempotency_conflict());
        }
        tx.rollback().await?;
        let mut result: AnnotationResult = serde_json::from_value(receipt.result)
            .map_err(|error| LedgerError::persistence(error.to_string()))?;
        result.replayed = true;
        return Ok(result);
    }
    let mut annotation = tx
        .find_annotation(command.user_id, command.journal_entry_id, true)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    annotation.update(
        command.changes,
        command.expected_version,
        Actor::User(command.user_id),
        clock.now(),
    )?;
    tx.save_annotation(&annotation).await?;
    let event = annotation
        .audit_events()
        .last()
        .cloned()
        .ok_or_else(|| LedgerError::invalid_annotation("annotation update made no change"))?;
    let event_id = EventId::generate();
    let payload = json!({
        "journal_entry_id": annotation.journal_entry_id(),
        "annotation_id": annotation.id(), "version": annotation.version(),
    });
    tx.append_audit(&AuditRecord {
        event_id,
        user_id: command.user_id,
        aggregate_kind: "transaction_annotation",
        aggregate_id: annotation.id().into_uuid(),
        event_type: "ledger.annotation-changed.v1",
        actor_kind: "user",
        actor_reference: Some(command.user_id.to_string()),
        correlation_id: command.correlation_id.into_uuid(),
        payload: payload.clone(),
        occurred_at: command.occurred_at,
        recorded_at: event.changed_at,
    })
    .await?;
    tx.append_outbox(&integration_event(
        event_id,
        command.user_id,
        annotation.id().to_string(),
        annotation.version().get() as u64,
        "ledger.annotation-changed.v1",
        command.occurred_at,
        command.correlation_id,
        None,
        payload,
    )?)
    .await?;
    let result = AnnotationResult {
        journal_entry_id: annotation.journal_entry_id(),
        version: annotation.version(),
        replayed: false,
    };
    let value = serde_json::to_value(&result)
        .map_err(|error| LedgerError::persistence(error.to_string()))?;
    tx.insert_receipt(
        command.user_id,
        "update_annotation",
        &command.idempotency_key,
        &hash,
        &value,
        clock.now(),
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}
