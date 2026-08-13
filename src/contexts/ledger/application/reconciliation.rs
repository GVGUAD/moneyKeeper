//! Provider-neutral, version-fenced balance reconciliation.

use rust_decimal::Decimal;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::shared_kernel::{Clock, EventId, Money};

use super::super::{
    domain::{
        Actor, BalanceObservation, BalanceVersion, JournalEntry, JournalEntryId, JournalRelations,
        JournalSource, LedgerAccount, LedgerAccountId, LedgerError, Posting, PostingId,
        PostingPurpose, ReconciliationCase, ReconciliationStatus, SystemAccountRole,
    },
    public::{
        AccountEffect, ApproveReconciliation, DismissReconciliation, ObserveProviderBalance,
        ReconciliationResult, ReconciliationView,
    },
};
use super::{
    accounts::{LedgerFacade, integration_event},
    commit::commit_journal,
    ports::{
        AuditRecord, AuditStore, CommandReceiptStore, CorrectionDetail, CorrectionStore,
        LedgerAccountStore, LedgerOutboxStore, LedgerUnitOfWork, ProjectionStore,
        ReconciliationStore, ReconciliationStream, TransactionControl,
    },
};

impl LedgerFacade {
    pub async fn observe_provider_balance(
        &self,
        command: ObserveProviderBalance,
    ) -> Result<ReconciliationResult, LedgerError> {
        observe(&self.uow, self.clock.as_ref(), command).await
    }

    pub async fn approve_reconciliation(
        &self,
        command: ApproveReconciliation,
    ) -> Result<ReconciliationResult, LedgerError> {
        approve(&self.uow, self.clock.as_ref(), command).await
    }

    pub async fn dismiss_reconciliation(
        &self,
        command: DismissReconciliation,
    ) -> Result<ReconciliationResult, LedgerError> {
        dismiss(&self.uow, self.clock.as_ref(), command).await
    }

    pub async fn list_reconciliations(
        &self,
        user_id: crate::shared_kernel::UserId,
    ) -> Result<Vec<ReconciliationView>, LedgerError> {
        self.queries.list_reconciliations(user_id).await
    }

    pub async fn get_reconciliation(
        &self,
        user_id: crate::shared_kernel::UserId,
        id: super::super::domain::ReconciliationCaseId,
    ) -> Result<ReconciliationView, LedgerError> {
        self.queries.get_reconciliation(user_id, id).await
    }
}

async fn observe<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    command: ObserveProviderBalance,
) -> Result<ReconciliationResult, LedgerError> {
    if command.provider_reported.currency()
        != command
            .available
            .as_ref()
            .map(Money::currency)
            .unwrap_or(command.provider_reported.currency())
    {
        return Err(LedgerError::currency_mismatch());
    }
    let request_hash = hash(&json!({
        "account_id": command.account_id, "observation_id": command.observation_id,
        "source": command.source, "provider_reported": command.provider_reported,
        "available": command.available, "observed_at": command.observed_at,
        "source_sequence": command.source_sequence,
    }))?;
    let mut tx = uow.begin().await?;
    if let Some(receipt) = tx
        .find_receipt(
            command.user_id,
            "observe_provider_balance",
            &command.idempotency_key,
            true,
        )
        .await?
    {
        if receipt.request_hash != request_hash {
            return Err(LedgerError::idempotency_conflict());
        }
        let case = tx
            .find_reconciliation_by_observation(command.user_id, command.observation_id)
            .await?
            .ok_or_else(LedgerError::not_found)?;
        tx.rollback().await?;
        return Ok(result(case, None, Vec::new(), true));
    }
    if let Some(case) = tx
        .find_reconciliation_by_observation(command.user_id, command.observation_id)
        .await?
    {
        tx.rollback().await?;
        return Ok(result(case, None, Vec::new(), true));
    }
    let account = tx
        .find_account(command.user_id, command.account_id, true)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    if account.currency() != command.provider_reported.currency() {
        return Err(LedgerError::currency_mismatch());
    }
    let stream = tx
        .lock_reconciliation_stream(command.user_id, command.account_id, &command.source)
        .await?;
    let (signed, balance_version) = tx
        .signed_balance(command.user_id, command.account_id, true)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    let captured = Money::new(
        signed * Decimal::from(account.normal_sign()),
        account.currency().clone(),
        8,
    )
    .map_err(|error| LedgerError::invalid_observation(error.to_string()))?;
    let observation = BalanceObservation::new(
        command.observation_id,
        command.source.clone(),
        command.provider_reported,
        command.available,
        command.observed_at,
        command.source_sequence,
        clock.now(),
    )?;
    let incoming = (
        observation.observed_at(),
        observation.source_sequence(),
        observation.id(),
    );
    let is_newer = stream.as_ref().is_none_or(|current| {
        incoming
            > (
                current.latest_observed_at,
                current.latest_source_sequence,
                current.latest_observation_id,
            )
    });
    let actor = Actor::External {
        source_kind: observation.source().source_kind().to_owned(),
        source_reference: observation.source().item_id().to_owned(),
    };
    let case_id = super::super::domain::ReconciliationCaseId::generate();
    let case = if is_newer {
        if let Some(active_id) = stream.as_ref().and_then(|value| value.active_case_id)
            && let Some(mut previous) = tx
                .find_reconciliation_case(command.user_id, active_id, true)
                .await?
            && previous.status() == ReconciliationStatus::Pending
        {
            previous.mark_superseded(clock.now())?;
            tx.save_reconciliation_case(&previous).await?;
            append_fact(
                &mut tx,
                &previous,
                "ledger.reconciliation-superseded.v1",
                command.correlation_id,
                command.observed_at,
                clock.now(),
            )
            .await?;
        }
        ReconciliationCase::observe(
            case_id,
            command.user_id,
            command.account_id,
            observation,
            captured,
            BalanceVersion::new(balance_version)?,
            actor,
            clock.now(),
        )?
    } else {
        ReconciliationCase::observe_ignored(
            case_id,
            command.user_id,
            command.account_id,
            observation,
            captured,
            BalanceVersion::new(balance_version)?,
            actor,
            clock.now(),
        )?
    };
    tx.insert_reconciliation_case(&case).await?;
    if is_newer {
        tx.save_reconciliation_stream(
            command.user_id,
            command.account_id,
            case.observation().source(),
            &ReconciliationStream {
                latest_observed_at: case.observation().observed_at(),
                latest_source_sequence: case.observation().source_sequence(),
                latest_observation_id: case.observation().id(),
                active_case_id: Some(case.id()),
            },
            clock.now(),
        )
        .await?;
    }
    let event_type = match case.status() {
        ReconciliationStatus::Matched => "ledger.reconciliation-matched.v1",
        ReconciliationStatus::IgnoredOlder => "ledger.reconciliation-ignored-older.v1",
        _ => "ledger.reconciliation-observed.v1",
    };
    append_fact(
        &mut tx,
        &case,
        event_type,
        command.correlation_id,
        command.observed_at,
        clock.now(),
    )
    .await?;
    let result = result(case, None, Vec::new(), false);
    store(
        &mut tx,
        command.user_id,
        "observe_provider_balance",
        &command.idempotency_key,
        &request_hash,
        &result,
        clock.now(),
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

async fn approve<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    command: ApproveReconciliation,
) -> Result<ReconciliationResult, LedgerError> {
    let request_hash = hash(
        &json!({"case_id":command.case_id,"expected_version":command.expected_version,
        "expected_balance_version":command.expected_balance_version,"reason":command.reason,"occurred_at":command.occurred_at}),
    )?;
    let mut tx = uow.begin().await?;
    if let Some(receipt) = tx
        .find_receipt(
            command.user_id,
            "approve_reconciliation",
            &command.idempotency_key,
            true,
        )
        .await?
    {
        if receipt.request_hash != request_hash {
            return Err(LedgerError::idempotency_conflict());
        }
        let case = tx
            .find_reconciliation_case(command.user_id, command.case_id, false)
            .await?
            .ok_or_else(LedgerError::not_found)?;
        tx.rollback().await?;
        return Ok(result(case, None, Vec::new(), true));
    }
    let mut case = tx
        .find_reconciliation_case(command.user_id, command.case_id, true)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    if case.version() != command.expected_version {
        return Err(LedgerError::version_conflict());
    }
    if case.captured_balance_version() != command.expected_balance_version {
        return Err(LedgerError::stale_observed_balance());
    }
    let stream = tx
        .lock_reconciliation_stream(
            command.user_id,
            case.account_id(),
            case.observation().source(),
        )
        .await?
        .ok_or_else(|| LedgerError::invalid_state("reconciliation stream is missing"))?;
    if stream.active_case_id != Some(case.id()) {
        return Err(LedgerError::invalid_state(
            "reconciliation case is superseded",
        ));
    }
    let account = tx
        .find_account(command.user_id, case.account_id(), true)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    let (signed_before, current_version) = tx
        .signed_balance(command.user_id, account.id(), true)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    let current_version = BalanceVersion::new(current_version)?;
    if current_version != command.expected_balance_version {
        return Err(LedgerError::stale_observed_balance());
    }
    let equity = ensure_system(&mut tx, command.user_id, &account, clock).await?;
    let signed_delta = case.delta().amount() * Decimal::from(account.normal_sign());
    let journal = JournalEntry::post(
        JournalEntryId::generate(),
        command.user_id,
        &command.reason,
        PostingPurpose::ApprovedReconciliation,
        JournalSource::Reconciliation,
        Actor::User(command.user_id),
        command.occurred_at,
        clock.now(),
        command.correlation_id,
        command.causation_id,
        command.idempotency_key.clone(),
        JournalRelations::none(),
        vec![
            Posting::for_account(
                PostingId::generate(),
                &account,
                signed_delta,
                PostingPurpose::ApprovedReconciliation,
            )?,
            Posting::for_account(
                PostingId::generate(),
                &equity,
                -signed_delta,
                PostingPurpose::ApprovedReconciliation,
            )?,
        ],
    )?;
    commit_journal(
        &mut tx,
        "approve_reconciliation",
        &journal,
        None,
        "ledger.entry-posted.v1",
    )
    .await?;
    tx.insert_correction_detail(CorrectionDetail {
        journal_entry_id: journal.id(),
        user_id: command.user_id,
        account_id: account.id(),
        currency: account.currency(),
        before_display_balance: signed_before * Decimal::from(account.normal_sign()),
        target_display_balance: case.observation().provider_reported().amount(),
        display_delta: case.delta().amount(),
        observed_balance_version: current_version.get(),
        reason: &command.reason,
        observed_at: case.observation().observed_at(),
        recorded_at: journal.recorded_at(),
    })
    .await?;
    case.approve(
        command.expected_version,
        current_version,
        journal.id(),
        Actor::User(command.user_id),
        clock.now(),
    )?;
    tx.save_reconciliation_case(&case).await?;
    append_fact(
        &mut tx,
        &case,
        "ledger.reconciliation-approved.v1",
        command.correlation_id,
        command.occurred_at,
        clock.now(),
    )
    .await?;
    let (signed_balance, version) = tx
        .signed_balance(command.user_id, account.id(), false)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    let effect = AccountEffect {
        account_id: account.id(),
        currency: account.currency().clone(),
        signed_amount: signed_delta,
        display_effect: case.delta().amount(),
        signed_balance,
        display_balance: signed_balance * Decimal::from(account.normal_sign()),
        balance_version: version,
    };
    let result = result(case, Some(journal.id()), vec![effect], false);
    store(
        &mut tx,
        command.user_id,
        "approve_reconciliation",
        &command.idempotency_key,
        &request_hash,
        &result,
        clock.now(),
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

async fn dismiss<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    command: DismissReconciliation,
) -> Result<ReconciliationResult, LedgerError> {
    let request_hash = hash(
        &json!({"case_id":command.case_id,"expected_version":command.expected_version,"reason":command.reason,"occurred_at":command.occurred_at}),
    )?;
    let mut tx = uow.begin().await?;
    if let Some(receipt) = tx
        .find_receipt(
            command.user_id,
            "dismiss_reconciliation",
            &command.idempotency_key,
            true,
        )
        .await?
    {
        if receipt.request_hash != request_hash {
            return Err(LedgerError::idempotency_conflict());
        }
        let case = tx
            .find_reconciliation_case(command.user_id, command.case_id, false)
            .await?
            .ok_or_else(LedgerError::not_found)?;
        tx.rollback().await?;
        return Ok(result(case, None, Vec::new(), true));
    }
    let mut case = tx
        .find_reconciliation_case(command.user_id, command.case_id, true)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    case.dismiss(
        command.expected_version,
        &command.reason,
        Actor::User(command.user_id),
        clock.now(),
    )?;
    tx.save_reconciliation_case(&case).await?;
    append_fact(
        &mut tx,
        &case,
        "ledger.reconciliation-dismissed.v1",
        command.correlation_id,
        command.occurred_at,
        clock.now(),
    )
    .await?;
    let result = result(case, None, Vec::new(), false);
    store(
        &mut tx,
        command.user_id,
        "dismiss_reconciliation",
        &command.idempotency_key,
        &request_hash,
        &result,
        clock.now(),
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

async fn ensure_system<T: LedgerAccountStore>(
    tx: &mut T,
    user_id: crate::shared_kernel::UserId,
    account: &LedgerAccount,
    clock: &dyn Clock,
) -> Result<LedgerAccount, LedgerError> {
    if let Some(value) = tx
        .find_system_account(
            user_id,
            account.currency(),
            SystemAccountRole::BalanceAdjustmentEquity,
            None,
        )
        .await?
    {
        return Ok(value);
    }
    let value = LedgerAccount::open_system(
        LedgerAccountId::generate(),
        user_id,
        account.currency().clone(),
        SystemAccountRole::BalanceAdjustmentEquity,
        clock,
    );
    tx.insert_account(&value).await?;
    Ok(value)
}

fn view(case: &ReconciliationCase) -> ReconciliationView {
    ReconciliationView {
        id: case.id(),
        account_id: case.account_id(),
        observation_id: case.observation().id(),
        source: case.observation().source().clone(),
        observed_at: case.observation().observed_at(),
        source_sequence: case.observation().source_sequence(),
        provider_reported: case.observation().provider_reported().clone(),
        available: case.observation().available().cloned(),
        captured_ledger_balance: case.captured_ledger_balance().clone(),
        captured_balance_version: case.captured_balance_version(),
        delta: case.delta().clone(),
        status: case.status(),
        version: case.version(),
        approval_journal_id: case.approval_journal_id(),
        reason: case.reason().map(str::to_owned),
        created_at: case.created_at(),
        updated_at: case.updated_at(),
    }
}

fn result(
    case: ReconciliationCase,
    journal_entry_id: Option<JournalEntryId>,
    effects: Vec<AccountEffect>,
    replayed: bool,
) -> ReconciliationResult {
    ReconciliationResult {
        case: view(&case),
        journal_entry_id,
        effects,
        replayed,
    }
}

async fn append_fact<T: AuditStore + LedgerOutboxStore>(
    tx: &mut T,
    case: &ReconciliationCase,
    event_type: &'static str,
    correlation_id: crate::shared_kernel::CorrelationId,
    occurred_at: chrono::DateTime<chrono::Utc>,
    recorded_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), LedgerError> {
    let event_id = EventId::generate();
    let payload = json!({"case_id":case.id(),"account_id":case.account_id(),"observation_id":case.observation().id(),"status":case.status(),"version":case.version(),"delta":case.delta()});
    tx.append_audit(&AuditRecord {
        event_id,
        user_id: case.user_id(),
        aggregate_kind: "reconciliation_case",
        aggregate_id: case.id().into_uuid(),
        event_type,
        actor_kind: "system",
        actor_reference: None,
        correlation_id: correlation_id.into_uuid(),
        payload: payload.clone(),
        occurred_at,
        recorded_at,
    })
    .await?;
    tx.append_outbox(&integration_event(
        event_id,
        case.user_id(),
        case.id().to_string(),
        case.version().get() as u64,
        event_type,
        occurred_at,
        correlation_id,
        None,
        payload,
    )?)
    .await
}

async fn store<T: CommandReceiptStore>(
    tx: &mut T,
    user_id: crate::shared_kernel::UserId,
    scope: &str,
    key: &crate::shared_kernel::IdempotencyKey,
    request_hash: &[u8; 32],
    result: &ReconciliationResult,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), LedgerError> {
    let value = serde_json::to_value(result)
        .map_err(|error| LedgerError::persistence(error.to_string()))?;
    tx.insert_receipt(user_id, scope, key, request_hash, &value, now)
        .await
}

fn hash(value: &Value) -> Result<[u8; 32], LedgerError> {
    Ok(Sha256::digest(
        serde_json::to_vec(value).map_err(|error| LedgerError::persistence(error.to_string()))?,
    )
    .into())
}
