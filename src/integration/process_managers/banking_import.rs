//! Causal, idempotent provider-revision import through public context contracts.

use sha2::{Digest, Sha256};

use crate::{
    contexts::{
        banking::public::{
            BankingError, BankingFacade, ProviderEventId, ProviderImportOutcome,
            ProviderTransactionState as BankingTransactionState,
        },
        ledger::public::{
            AnnotationChanges, ApplyCategoryAssignment, AssignmentOrigin, AutomationState,
            CategoryReference, ImportProviderTransaction, InternalCommandMetadata, LedgerFacade,
            ProviderTransactionState, ReverseProviderTransaction, SourceReference,
            UpdateTransactionAnnotation,
        },
    },
    shared_kernel::{CorrelationId, IdempotencyKey},
};

pub async fn import_provider_revision(
    banking: &BankingFacade,
    ledger: &LedgerFacade,
    user_id: crate::shared_kernel::UserId,
    event_id: ProviderEventId,
) -> Result<ProviderImportOutcome, BankingError> {
    let Some(mut work) = banking.claim_provider_import(user_id, event_id).await? else {
        return Ok(ProviderImportOutcome {
            provider_event_id: event_id,
            state: "waiting_or_complete".to_owned(),
            ledger_journal_entry_id: None,
            replayed: true,
            lease_holder: None,
            fencing_token: None,
        });
    };
    let source = SourceReference::new(
        "banking",
        format!("{}:{}", work.connection_id, work.resource_id),
        format!("{}:{}", work.external_event_id, work.revision),
    )
    .map_err(|_| BankingError::InvalidValue("invalid provider source reference"))?;

    let changed = work.state == BankingTransactionState::Reversed
        || work
            .previous_money
            .as_ref()
            .is_some_and(|m| m != &work.operation_money);
    let resolved = ledger
        .transfer_conversion(
            work.user_id,
            crate::contexts::ledger::public::ConversionAction::ResolveProvider {
                previous: work.previous_journal_id,
                stream: source.stream_id().into(),
                item: source.item_id().into(),
                changed,
            },
        )
        .await
        .map_err(|_| BankingError::InvalidState)?;
    if let crate::contexts::ledger::public::ConversionResponse::Reference { journal_id } = resolved
    {
        work.previous_journal_id = journal_id;
    }
    let correlation_id = CorrelationId::new(work.provider_event_id.into_uuid());
    let metadata = |operation: &str| -> Result<InternalCommandMetadata, BankingError> {
        Ok(InternalCommandMetadata {
            user_id: work.user_id,
            source: source.clone(),
            correlation_id,
            causation_id: None,
            idempotency_key: key(operation, &work)?,
            occurred_at: work.effective_at,
        })
    };
    let previous_journal = match work.previous_journal_id {
        Some(previous) => Some(
            ledger
                .get_journal(work.user_id, previous)
                .await
                .map_err(|_| BankingError::InvalidState)?,
        ),
        None => None,
    };
    let no_change = work.previous_money.as_ref() == Some(&work.operation_money)
        && work.state == BankingTransactionState::Settled;
    let inherited_annotation = if !no_change && work.state != BankingTransactionState::Reversed {
        previous_journal.and_then(|journal| journal.annotation)
    } else {
        None
    };
    let journal_id = if no_change {
        work.previous_journal_id
    } else if work.state == BankingTransactionState::Reversed {
        let Some(previous) = work.previous_journal_id else {
            return banking
                .complete_provider_import(ProviderImportOutcome {
                    provider_event_id: event_id,
                    state: "no_financial_change".into(),
                    ledger_journal_entry_id: None,
                    replayed: false,
                    lease_holder: Some(work.lease_holder),
                    fencing_token: Some(work.fencing_token),
                })
                .await;
        };
        ledger
            .reverse_provider_transaction(ReverseProviderTransaction {
                metadata: metadata("reverse")?,
                imported_journal_entry_id: previous,
                reason: "provider reversal".to_owned(),
            })
            .await
            .map_err(|_| BankingError::InvalidState)?
            .journal_entry_id
    } else {
        if work
            .previous_money
            .as_ref()
            .is_some_and(|money| money != &work.operation_money)
            && let Some(previous) = work.previous_journal_id
        {
            ledger
                .reverse_provider_transaction(ReverseProviderTransaction {
                    metadata: metadata("correct-reverse")?,
                    imported_journal_entry_id: previous,
                    reason: "provider monetary correction".to_owned(),
                })
                .await
                .map_err(|_| BankingError::InvalidState)?;
        }
        let imported = ledger
            .import_provider_transaction(ImportProviderTransaction {
                metadata: metadata("post")?,
                user_account_id: work.ledger_account_id,
                amount: work.operation_money.clone(),
                state: ProviderTransactionState::Posted,
                description: work.description.clone(),
            })
            .await
            .map_err(|_| BankingError::InvalidState)?;
        let Some(imported_journal_id) = imported.journal_entry_id else {
            return banking
                .complete_provider_import(ProviderImportOutcome {
                    provider_event_id: event_id,
                    state: "no_financial_change".into(),
                    ledger_journal_entry_id: None,
                    replayed: false,
                    lease_holder: Some(work.lease_holder),
                    fencing_token: Some(work.fencing_token),
                })
                .await;
        };
        if let Some(previous) = inherited_annotation {
            inherit_provider_correction_annotation(ledger, &work, imported_journal_id, previous)
                .await?;
        }
        Some(imported_journal_id)
    };
    banking
        .complete_provider_import(ProviderImportOutcome {
            provider_event_id: event_id,
            state: if no_change {
                "no_financial_change".to_owned()
            } else {
                "posted".to_owned()
            },
            ledger_journal_entry_id: journal_id,
            replayed: false,
            lease_holder: Some(work.lease_holder),
            fencing_token: Some(work.fencing_token),
        })
        .await
}

async fn inherit_provider_correction_annotation(
    ledger: &LedgerFacade,
    work: &crate::contexts::banking::public::ProviderImportWork,
    journal_entry_id: crate::contexts::ledger::public::JournalEntryId,
    previous: crate::contexts::ledger::public::JournalAnnotationView,
) -> Result<(), BankingError> {
    let user_id = work.user_id;
    match previous.assignment_origin {
        Some(AssignmentOrigin::Manual) => {
            let requested = previous
                .category_id
                .map(|id| CategoryReference::new(id.into_uuid()));
            let command = |category,
                           operation|
             -> Result<UpdateTransactionAnnotation, BankingError> {
                Ok(UpdateTransactionAnnotation {
                    user_id,
                    journal_entry_id,
                    changes: AnnotationChanges {
                        category: Some(category),
                        ..AnnotationChanges::default()
                    },
                    expected_version: crate::contexts::ledger::public::AnnotationVersion::INITIAL,
                    idempotency_key: key(operation, work)?,
                    correlation_id: CorrelationId::new(work.provider_event_id.into_uuid()),
                    occurred_at: work.effective_at,
                })
            };
            match ledger
                .update_annotation(command(requested, "inherit-manual")?)
                .await
            {
                Ok(_) => Ok(()),
                Err(error) if error.is_invalid_annotation() => {
                    // A kind change can make the old leaf incompatible. Preserve the
                    // user's suppression while leaving the corrected cash flow uncategorized.
                    ledger
                        .update_annotation(command(None, "inherit-incompatible-clear")?)
                        .await
                        .map(|_| ())
                        .map_err(|_| BankingError::InvalidState)
                }
                Err(_) => Err(BankingError::InvalidState),
            }
        }
        Some(AssignmentOrigin::Recurring) => {
            let Some(category_id) = previous.category_id else {
                return Ok(());
            };
            match ledger
                .apply_category_assignment(ApplyCategoryAssignment {
                    user_id,
                    journal_entry_id,
                    category_id: Some(category_id),
                    origin: AssignmentOrigin::Recurring,
                    classification_decision_id: None,
                    expected_version: crate::contexts::ledger::public::AnnotationVersion::INITIAL,
                    idempotency_key: key("inherit-recurring", work)?,
                    correlation_id: CorrelationId::new(work.provider_event_id.into_uuid()),
                    occurred_at: work.effective_at,
                })
                .await
            {
                Ok(_) => Ok(()),
                Err(error) if error.is_invalid_annotation() => ledger
                    .update_annotation(UpdateTransactionAnnotation {
                        user_id,
                        journal_entry_id,
                        changes: AnnotationChanges {
                            category: Some(None),
                            ..AnnotationChanges::default()
                        },
                        expected_version:
                            crate::contexts::ledger::public::AnnotationVersion::INITIAL,
                        idempotency_key: key("inherit-incompatible-recurring-clear", work)?,
                        correlation_id: CorrelationId::new(work.provider_event_id.into_uuid()),
                        occurred_at: work.effective_at,
                    })
                    .await
                    .map(|_| ())
                    .map_err(|_| BankingError::InvalidState),
                Err(_) => Err(BankingError::InvalidState),
            }
        }
        Some(AssignmentOrigin::Ai) => Ok(()),
        None if previous.automation_state != AutomationState::Eligible => ledger
            .update_annotation(UpdateTransactionAnnotation {
                user_id,
                journal_entry_id,
                changes: AnnotationChanges {
                    category: Some(None),
                    ..AnnotationChanges::default()
                },
                expected_version: crate::contexts::ledger::public::AnnotationVersion::INITIAL,
                idempotency_key: key("inherit-protected-clear", work)?,
                correlation_id: CorrelationId::new(work.provider_event_id.into_uuid()),
                occurred_at: work.effective_at,
            })
            .await
            .map(|_| ())
            .map_err(|_| BankingError::InvalidState),
        None => Ok(()),
    }
}

fn key(
    operation: &str,
    work: &crate::contexts::banking::public::ProviderImportWork,
) -> Result<IdempotencyKey, BankingError> {
    let digest = Sha256::digest(format!(
        "{operation}|{}|{}|{}|{}|{}",
        work.connection_id,
        work.resource_id,
        work.external_event_id,
        work.revision,
        work.provider_event_id
    ));
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    IdempotencyKey::new(format!("banking-import-{operation}-{encoded}"))
        .map_err(|_| BankingError::InvalidValue("invalid import idempotency key"))
}
