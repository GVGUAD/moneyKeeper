//! Versioned transaction annotation commands.

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::contexts::classification::public::{CategoryCatalog, CategoryId, CategoryKind};
use crate::shared_kernel::{Clock, CorrelationId, EventId};

use super::super::{
    domain::{Actor, AssignmentOrigin, CategoryReference, LedgerError, TransactionAnnotation},
    public::{
        AnnotationResult, ApplyCategoryAssignment, CategoryAssignmentDisposition,
        CategoryAssignmentResult, EnableAutomaticClassification, JournalView,
        RestoreCategoryAssignment, UpdateTransactionAnnotation,
    },
};
use super::{
    accounts::{LedgerApplication, integration_event},
    ports::{
        AnnotationStore, AuditRecord, AuditStore, CommandReceiptStore, LedgerOutboxStore,
        LedgerQueryPort, LedgerUnitOfWork, TransactionControl,
    },
};

impl<U: LedgerUnitOfWork, Q: LedgerQueryPort, P> LedgerApplication<U, Q, P> {
    /// Updates transaction metadata without mutating its journal or postings.
    pub async fn update_annotation(
        &self,
        command: UpdateTransactionAnnotation,
    ) -> Result<AnnotationResult, LedgerError> {
        let categories = self.categories.as_ref().ok_or_else(|| {
            LedgerError::persistence("Classification catalog is not configured for Ledger")
        })?;
        let journal = self
            .queries
            .get_journal(command.user_id, command.journal_entry_id)
            .await?;
        update_annotation(
            &self.uow,
            self.clock.as_ref(),
            categories,
            &journal,
            command,
        )
        .await
    }

    /// Applies an AI or Recurring category without overriding stronger intent.
    pub async fn apply_category_assignment(
        &self,
        command: ApplyCategoryAssignment,
    ) -> Result<CategoryAssignmentResult, LedgerError> {
        let categories = self.categories.as_ref().ok_or_else(|| {
            LedgerError::persistence("Classification catalog is not configured for Ledger")
        })?;
        let journal = self
            .queries
            .get_journal(command.user_id, command.journal_entry_id)
            .await?;
        apply_category_assignment(
            &self.uow,
            self.clock.as_ref(),
            categories,
            &journal,
            command,
        )
        .await
    }

    /// Restores the full assignment snapshot captured by Recurring.
    pub async fn restore_category_assignment(
        &self,
        command: RestoreCategoryAssignment,
    ) -> Result<CategoryAssignmentResult, LedgerError> {
        restore_category_assignment(&self.uow, self.clock.as_ref(), command).await
    }

    /// Re-enables automatic classification after explicit user intent.
    pub async fn enable_automatic_classification(
        &self,
        command: EnableAutomaticClassification,
    ) -> Result<CategoryAssignmentResult, LedgerError> {
        enable_automatic_classification(&self.uow, self.clock.as_ref(), command).await
    }
}

async fn update_annotation<U: LedgerUnitOfWork, C: CategoryCatalog>(
    uow: &U,
    clock: &dyn Clock,
    categories: &C,
    journal: &JournalView,
    command: UpdateTransactionAnnotation,
) -> Result<AnnotationResult, LedgerError> {
    let category_touched = command.changes.category.is_some();
    let request = json!({
        "journal_entry_id": command.journal_entry_id,
        "description": command.changes.description,
        "category": command.changes.category.map(|value| value.map(|id| id.into_uuid())),
        "note": command.changes.note,
        "tags": command.changes.tags,
        "budget_visibility": command.changes.budget_visibility,
        "expected_version": command.expected_version,
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
    if let Some(Some(reference)) = command.changes.category {
        validate_assignable(categories, journal, CategoryId::new(reference.into_uuid())).await?;
    }
    let previous_assignment = annotation.assignment_snapshot();
    let changed = annotation.update(
        command.changes,
        command.expected_version,
        Actor::User(command.user_id),
        clock.now(),
    )?;
    if changed {
        tx.save_annotation(&annotation).await?;
        let event =
            annotation.audit_events().last().cloned().ok_or_else(|| {
                LedgerError::invalid_annotation("annotation update made no change")
            })?;
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
        if category_touched {
            append_category_assignment_event(
                &mut tx,
                &annotation,
                previous_assignment,
                command.correlation_id,
                command.occurred_at,
            )
            .await?;
        }
    }
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

async fn validate_assignable<C: CategoryCatalog>(
    categories: &C,
    journal: &JournalView,
    category_id: CategoryId,
) -> Result<(), LedgerError> {
    let kind = cash_flow_kind(journal)?;
    categories
        .require_assignable(journal.user_id, category_id, kind)
        .await
        .map_err(|_| {
            LedgerError::invalid_annotation(
                "category is missing, archived, not a leaf, or incompatible",
            )
        })?;
    Ok(())
}

fn cash_flow_kind(journal: &JournalView) -> Result<CategoryKind, LedgerError> {
    if journal.purpose != super::super::domain::PostingPurpose::Ordinary {
        return Err(LedgerError::invalid_annotation(
            "categories can only be assigned to ordinary cash-flow transactions",
        ));
    }
    let mut kind = None;
    for posting in &journal.postings {
        let next = match posting.account_nature {
            super::super::domain::AccountNature::Income => Some(CategoryKind::Income),
            super::super::domain::AccountNature::Expense => Some(CategoryKind::Expense),
            _ => None,
        };
        if let Some(next) = next {
            if kind.is_some_and(|current| current != next) {
                return Err(LedgerError::invalid_annotation(
                    "transaction has ambiguous cash-flow kind",
                ));
            }
            kind = Some(next);
        }
    }
    let kind = kind.ok_or_else(|| {
        LedgerError::invalid_annotation("transaction is not an income or expense cash flow")
    })?;
    Ok(kind)
}

async fn apply_category_assignment<U: LedgerUnitOfWork, C: CategoryCatalog>(
    uow: &U,
    clock: &dyn Clock,
    categories: &C,
    journal: &JournalView,
    command: ApplyCategoryAssignment,
) -> Result<CategoryAssignmentResult, LedgerError> {
    let valid_shape = match command.origin {
        AssignmentOrigin::Manual => command.classification_decision_id.is_some(),
        AssignmentOrigin::Ai => {
            command.category_id.is_some() && command.classification_decision_id.is_some()
        }
        AssignmentOrigin::Recurring => {
            command.category_id.is_some() && command.classification_decision_id.is_none()
        }
    };
    if !valid_shape {
        return Err(LedgerError::invalid_annotation(
            "automatic category assignment shape is invalid",
        ));
    }
    let request = json!({
        "journal_entry_id": command.journal_entry_id,
        "category_id": command.category_id,
        "origin": command.origin,
        "classification_decision_id": command.classification_decision_id,
        "expected_version": command.expected_version,
        "occurred_at": command.occurred_at,
    });
    let hash = request_hash(&request)?;
    let mut tx = uow.begin().await?;
    if let Some(mut result) = replay_assignment(
        &mut tx,
        command.user_id,
        "apply_category_assignment",
        &command.idempotency_key,
        &hash,
    )
    .await?
    {
        tx.rollback().await?;
        result.replayed = true;
        return Ok(result);
    }
    let mut annotation = tx
        .find_annotation(command.user_id, command.journal_entry_id, true)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    if let Some(category_id) = command.category_id {
        validate_assignable(categories, journal, category_id).await?;
    } else {
        cash_flow_kind(journal)?;
    }
    let previous_assignment = annotation.assignment_snapshot();
    let changed = annotation.apply_system_assignment(
        command
            .category_id
            .map(CategoryId::into_uuid)
            .map(CategoryReference::new),
        command.origin,
        command.classification_decision_id,
        command.expected_version,
        clock.now(),
    )?;
    if changed {
        tx.save_annotation(&annotation).await?;
        append_annotation_events(
            &mut tx,
            &annotation,
            previous_assignment,
            "system",
            Some(command.origin.as_str().to_owned()),
            command.correlation_id,
            command.occurred_at,
        )
        .await?;
    }
    let result = CategoryAssignmentResult {
        journal_entry_id: annotation.journal_entry_id(),
        version: annotation.version(),
        disposition: if changed {
            CategoryAssignmentDisposition::Applied
        } else {
            CategoryAssignmentDisposition::NoEffect
        },
        replayed: false,
    };
    store_assignment_receipt(
        &mut tx,
        command.user_id,
        "apply_category_assignment",
        &command.idempotency_key,
        &hash,
        &result,
        clock.now(),
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

async fn restore_category_assignment<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    command: RestoreCategoryAssignment,
) -> Result<CategoryAssignmentResult, LedgerError> {
    let request = json!({
        "journal_entry_id": command.journal_entry_id,
        "snapshot": command.snapshot,
        "expected_version": command.expected_version,
        "occurred_at": command.occurred_at,
    });
    let hash = request_hash(&request)?;
    let mut tx = uow.begin().await?;
    if let Some(mut result) = replay_assignment(
        &mut tx,
        command.user_id,
        "restore_category_assignment",
        &command.idempotency_key,
        &hash,
    )
    .await?
    {
        tx.rollback().await?;
        result.replayed = true;
        return Ok(result);
    }
    let mut annotation = tx
        .find_annotation(command.user_id, command.journal_entry_id, true)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    let previous_assignment = annotation.assignment_snapshot();
    let changed =
        annotation.restore_assignment(command.snapshot, command.expected_version, clock.now())?;
    if changed {
        tx.save_annotation(&annotation).await?;
        append_annotation_events(
            &mut tx,
            &annotation,
            previous_assignment,
            "system",
            Some("recurring_compensation".to_owned()),
            command.correlation_id,
            command.occurred_at,
        )
        .await?;
    }
    let result = CategoryAssignmentResult {
        journal_entry_id: annotation.journal_entry_id(),
        version: annotation.version(),
        disposition: if changed {
            CategoryAssignmentDisposition::Applied
        } else {
            CategoryAssignmentDisposition::NoEffect
        },
        replayed: false,
    };
    store_assignment_receipt(
        &mut tx,
        command.user_id,
        "restore_category_assignment",
        &command.idempotency_key,
        &hash,
        &result,
        clock.now(),
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

async fn enable_automatic_classification<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    command: EnableAutomaticClassification,
) -> Result<CategoryAssignmentResult, LedgerError> {
    let request = json!({
        "journal_entry_id": command.journal_entry_id,
        "expected_version": command.expected_version,
    });
    let hash = request_hash(&request)?;
    let mut tx = uow.begin().await?;
    if let Some(mut result) = replay_assignment(
        &mut tx,
        command.user_id,
        "enable_automatic_classification",
        &command.idempotency_key,
        &hash,
    )
    .await?
    {
        tx.rollback().await?;
        result.replayed = true;
        return Ok(result);
    }
    let mut annotation = tx
        .find_annotation(command.user_id, command.journal_entry_id, true)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    let previous_assignment = annotation.assignment_snapshot();
    let changed = annotation.enable_automatic_classification(
        command.expected_version,
        Actor::User(command.user_id),
        clock.now(),
    )?;
    if changed {
        tx.save_annotation(&annotation).await?;
        append_annotation_events(
            &mut tx,
            &annotation,
            previous_assignment,
            "user",
            Some(command.user_id.to_string()),
            command.correlation_id,
            command.occurred_at,
        )
        .await?;
    }
    let result = CategoryAssignmentResult {
        journal_entry_id: annotation.journal_entry_id(),
        version: annotation.version(),
        disposition: if changed {
            CategoryAssignmentDisposition::Applied
        } else {
            CategoryAssignmentDisposition::NoEffect
        },
        replayed: false,
    };
    store_assignment_receipt(
        &mut tx,
        command.user_id,
        "enable_automatic_classification",
        &command.idempotency_key,
        &hash,
        &result,
        clock.now(),
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

async fn append_annotation_events<T>(
    tx: &mut T,
    annotation: &TransactionAnnotation,
    previous_assignment: super::super::domain::CategoryAssignmentSnapshot,
    actor_kind: &'static str,
    actor_reference: Option<String>,
    correlation_id: CorrelationId,
    occurred_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), LedgerError>
where
    T: AuditStore + LedgerOutboxStore,
{
    let event = annotation
        .audit_events()
        .last()
        .ok_or_else(|| LedgerError::invalid_annotation("annotation update made no change"))?;
    let event_id = EventId::generate();
    let payload = json!({
        "journal_entry_id": annotation.journal_entry_id(),
        "annotation_id": annotation.id(),
        "version": annotation.version(),
    });
    tx.append_audit(&AuditRecord {
        event_id,
        user_id: annotation.user_id(),
        aggregate_kind: "transaction_annotation",
        aggregate_id: annotation.id().into_uuid(),
        event_type: "ledger.annotation-changed.v1",
        actor_kind,
        actor_reference,
        correlation_id: correlation_id.into_uuid(),
        payload: payload.clone(),
        occurred_at,
        recorded_at: event.changed_at,
    })
    .await?;
    tx.append_outbox(&integration_event(
        event_id,
        annotation.user_id(),
        annotation.id().to_string(),
        annotation.version().get() as u64,
        "ledger.annotation-changed.v1",
        occurred_at,
        correlation_id,
        None,
        payload,
    )?)
    .await?;
    append_category_assignment_event(
        tx,
        annotation,
        previous_assignment,
        correlation_id,
        occurred_at,
    )
    .await
}

async fn append_category_assignment_event<T: LedgerOutboxStore>(
    tx: &mut T,
    annotation: &TransactionAnnotation,
    previous_assignment: super::super::domain::CategoryAssignmentSnapshot,
    correlation_id: CorrelationId,
    occurred_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), LedgerError> {
    let payload = json!({
        "journal_entry_id": annotation.journal_entry_id(),
        "annotation_id": annotation.id(),
        "annotation_version": annotation.version(),
        "category_id": annotation.category().map(CategoryReference::into_uuid),
        "assignment_origin": annotation.assignment_origin(),
        "classification_decision_id": annotation.classification_decision_id(),
        "automation_state": annotation.automation_state(),
        "previous_category_id": previous_assignment.category.map(CategoryReference::into_uuid),
        "previous_assignment_origin": previous_assignment.origin,
        "previous_classification_decision_id": previous_assignment.classification_decision_id,
        "previous_automation_state": previous_assignment.automation_state,
    });
    tx.append_outbox(&integration_event(
        EventId::generate(),
        annotation.user_id(),
        annotation.id().to_string(),
        annotation.version().get() as u64,
        super::super::public::CATEGORY_ASSIGNMENT_CHANGED_V1,
        occurred_at,
        correlation_id,
        None,
        payload,
    )?)
    .await
}

fn request_hash(value: &serde_json::Value) -> Result<[u8; 32], LedgerError> {
    Ok(Sha256::digest(
        serde_json::to_vec(value).map_err(|error| LedgerError::persistence(error.to_string()))?,
    )
    .into())
}

async fn replay_assignment<T: CommandReceiptStore>(
    tx: &mut T,
    user_id: crate::shared_kernel::UserId,
    scope: &str,
    idempotency_key: &crate::shared_kernel::IdempotencyKey,
    hash: &[u8; 32],
) -> Result<Option<CategoryAssignmentResult>, LedgerError> {
    let Some(receipt) = tx
        .find_receipt(user_id, scope, idempotency_key, true)
        .await?
    else {
        return Ok(None);
    };
    if &receipt.request_hash != hash {
        return Err(LedgerError::idempotency_conflict());
    }
    serde_json::from_value(receipt.result)
        .map(Some)
        .map_err(|error| LedgerError::persistence(error.to_string()))
}

#[allow(clippy::too_many_arguments)]
async fn store_assignment_receipt<T: CommandReceiptStore>(
    tx: &mut T,
    user_id: crate::shared_kernel::UserId,
    scope: &str,
    idempotency_key: &crate::shared_kernel::IdempotencyKey,
    hash: &[u8; 32],
    result: &CategoryAssignmentResult,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), LedgerError> {
    let value = serde_json::to_value(result)
        .map_err(|error| LedgerError::persistence(error.to_string()))?;
    tx.insert_receipt(user_id, scope, idempotency_key, hash, &value, now)
        .await
}
