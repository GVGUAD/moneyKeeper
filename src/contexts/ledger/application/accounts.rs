//! Account lifecycle commands and journal-based opening balances.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::integration::IntegrationEvent;
use crate::shared_kernel::{
    Clock, EventEnvelope, EventId, SystemClock,
};

use super::ports::{
    AuditRecord, AuditStore, CommandReceiptStore, JournalStore, LedgerAccountStore,
    LedgerOutboxStore, LedgerUnitOfWork, ProjectionStore, TransactionControl,
};
use super::super::{
    domain::{
        Actor, JournalEntry, JournalEntryId, JournalRelations, JournalSource, LedgerAccount,
        LedgerAccountId, LedgerError, Posting, PostingId, PostingPurpose, SystemAccountRole,
    },
    infrastructure::PgLedgerUnitOfWork,
    public::{
        AccountResult, AccountView, ArchiveAccount, OpenAccount, RenameAccount, RestoreAccount,
    },
};

/// Public command facade with private PostgreSQL composition.
#[derive(Clone)]
pub struct LedgerFacade {
    uow: PgLedgerUnitOfWork,
    clock: Arc<dyn Clock>,
}

impl LedgerFacade {
    pub(crate) fn new(uow: PgLedgerUnitOfWork) -> Self {
        Self { uow, clock: Arc::new(SystemClock) }
    }

    #[cfg(test)]
    pub(crate) fn with_clock(uow: PgLedgerUnitOfWork, clock: Arc<dyn Clock>) -> Self {
        Self { uow, clock }
    }

    /// Opens an account and records any non-zero opening balance as a journal.
    pub async fn open_account(&self, command: OpenAccount) -> Result<AccountResult, LedgerError> {
        open_account(&self.uow, self.clock.as_ref(), command).await
    }

    /// Renames account metadata using optimistic concurrency.
    pub async fn rename_account(&self, command: RenameAccount) -> Result<AccountResult, LedgerError> {
        rename_account(&self.uow, self.clock.as_ref(), command).await
    }

    /// Archives an account without hiding history or balance.
    pub async fn archive_account(&self, command: ArchiveAccount) -> Result<AccountResult, LedgerError> {
        change_lifecycle(&self.uow, self.clock.as_ref(), LifecycleCommand::Archive(command)).await
    }

    /// Restores an archived account.
    pub async fn restore_account(&self, command: RestoreAccount) -> Result<AccountResult, LedgerError> {
        change_lifecycle(&self.uow, self.clock.as_ref(), LifecycleCommand::Restore(command)).await
    }
}

async fn open_account<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    command: OpenAccount,
) -> Result<AccountResult, LedgerError> {
    if command.opening_balance.currency() != &command.currency {
        return Err(LedgerError::currency_mismatch());
    }
    let request_hash = hash(&json!({
        "name": command.name,
        "currency": command.currency,
        "kind": command.kind,
        "nature": command.nature,
        "opening_balance": command.opening_balance,
        "occurred_at": command.occurred_at,
    }))?;
    let mut tx = uow.begin().await?;
    if let Some(result) = replay(
        &mut tx,
        command.user_id,
        "open_account",
        &command.idempotency_key,
        &request_hash,
    ).await? {
        tx.rollback().await?;
        return Ok(result);
    }

    let now = clock.now();
    let account = LedgerAccount::open_manual(
        LedgerAccountId::generate(),
        command.user_id,
        &command.name,
        command.currency.clone(),
        command.kind,
        command.nature,
        clock,
    )?;

    let outcome = async {
        tx.insert_account(&account).await?;
        let journal_id = if command.opening_balance.is_zero() {
            None
        } else {
            let system = match tx
                .find_system_account(
                    command.user_id,
                    &command.currency,
                    SystemAccountRole::OpeningBalanceEquity,
                    None,
                )
                .await?
            {
                Some(account) => account,
                None => {
                    let account = LedgerAccount::open_system(
                        LedgerAccountId::generate(),
                        command.user_id,
                        command.currency.clone(),
                        SystemAccountRole::OpeningBalanceEquity,
                        clock,
                    );
                    tx.insert_account(&account).await?;
                    account
                }
            };
            let signed = command.opening_balance.amount()
                * Decimal::from(account.normal_sign());
            let journal = JournalEntry::post(
                JournalEntryId::generate(),
                command.user_id,
                format!("Opening balance for {}", account.name()),
                PostingPurpose::Ordinary,
                JournalSource::Manual,
                Actor::User(command.user_id),
                command.occurred_at,
                now,
                command.correlation_id,
                command.causation_id,
                command.idempotency_key.clone(),
                JournalRelations::none(),
                vec![
                    Posting::for_account(
                        PostingId::generate(), &account, signed, PostingPurpose::Ordinary,
                    )?,
                    Posting::for_account(
                        PostingId::generate(), &system, -signed, PostingPurpose::Ordinary,
                    )?,
                ],
            )?;
            tx.insert_journal("open_account", &journal).await?;
            tx.apply_postings(&journal).await?;
            append_journal_facts(&mut tx, &journal, "ledger.entry-posted.v1").await?;
            Some(journal.id())
        };

        let (signed_balance, balance_version) = tx
            .signed_balance(command.user_id, account.id(), false)
            .await?
            .ok_or_else(LedgerError::not_found)?;
        let result = AccountResult {
            account: view(&account, signed_balance, balance_version, now),
            opening_journal_id: journal_id,
            replayed: false,
        };
        append_account_facts(&mut tx, &account, &command, now).await?;
        let result_json = serde_json::to_value(&result)
            .map_err(|error| LedgerError::persistence(error.to_string()))?;
        tx.insert_receipt(
            command.user_id,
            "open_account",
            &command.idempotency_key,
            &request_hash,
            &result_json,
            now,
        ).await?;
        Ok::<_, LedgerError>(result)
    }.await;

    match outcome {
        Ok(result) => match tx.commit().await {
            Ok(()) => Ok(result),
            Err(error) => replay_after_failure(
                uow, command.user_id, "open_account", &command.idempotency_key, &request_hash, error,
            ).await,
        },
        Err(error) => {
            tx.rollback().await?;
            replay_after_failure(
                uow, command.user_id, "open_account", &command.idempotency_key, &request_hash, error,
            ).await
        }
    }
}

async fn rename_account<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    command: RenameAccount,
) -> Result<AccountResult, LedgerError> {
    mutate_account(
        uow,
        clock,
        command.user_id,
        command.account_id,
        "rename_account",
        command.idempotency_key,
        json!({"account_id": command.account_id, "name": command.name, "expected_version": command.expected_version}),
        command.correlation_id,
        command.occurred_at,
        |account, clock| account.rename(command.name, command.expected_version, clock),
    ).await
}

enum LifecycleCommand {
    Archive(ArchiveAccount),
    Restore(RestoreAccount),
}

async fn change_lifecycle<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    command: LifecycleCommand,
) -> Result<AccountResult, LedgerError> {
    match command {
        LifecycleCommand::Archive(command) => mutate_account(
            uow, clock, command.user_id, command.account_id, "archive_account",
            command.idempotency_key,
            json!({"account_id": command.account_id, "expected_version": command.expected_version}),
            command.correlation_id, command.occurred_at,
            |account, clock| account.archive(command.expected_version, clock),
        ).await,
        LifecycleCommand::Restore(command) => mutate_account(
            uow, clock, command.user_id, command.account_id, "restore_account",
            command.idempotency_key,
            json!({"account_id": command.account_id, "expected_version": command.expected_version}),
            command.correlation_id, command.occurred_at,
            |account, clock| account.restore(command.expected_version, clock),
        ).await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn mutate_account<U, F>(
    uow: &U,
    clock: &dyn Clock,
    user_id: crate::shared_kernel::UserId,
    account_id: LedgerAccountId,
    command_name: &'static str,
    idempotency_key: crate::shared_kernel::IdempotencyKey,
    request: Value,
    correlation_id: crate::shared_kernel::CorrelationId,
    occurred_at: DateTime<Utc>,
    mutate: F,
) -> Result<AccountResult, LedgerError>
where
    U: LedgerUnitOfWork,
    F: FnOnce(&mut LedgerAccount, &dyn Clock) -> Result<bool, LedgerError>,
{
    let request_hash = hash(&request)?;
    let mut tx = uow.begin().await?;
    if let Some(result) = replay(&mut tx, user_id, command_name, &idempotency_key, &request_hash).await? {
        tx.rollback().await?;
        return Ok(result);
    }
    let mut account = tx.find_account(user_id, account_id, true).await?
        .ok_or_else(LedgerError::not_found)?;
    mutate(&mut account, clock)?;
    tx.save_account(&account).await?;
    let now = clock.now();
    let (signed_balance, balance_version) = tx.signed_balance(user_id, account_id, false).await?
        .ok_or_else(LedgerError::not_found)?;
    let result = AccountResult {
        account: view(&account, signed_balance, balance_version, now),
        opening_journal_id: None,
        replayed: false,
    };
    let event_id = EventId::generate();
    let audit = AuditRecord {
        event_id, user_id, aggregate_kind: "account", aggregate_id: account_id.into_uuid(),
        event_type: "ledger.account-lifecycle-changed.v1", actor_kind: "user",
        actor_reference: Some(user_id.to_string()), correlation_id: correlation_id.into_uuid(),
        payload: json!({"account_id": account_id, "version": account.version(), "lifecycle": account.lifecycle(), "name": account.name()}),
        occurred_at, recorded_at: now,
    };
    tx.append_audit(&audit).await?;
    tx.append_outbox(&integration_event(
        event_id, user_id, account_id.to_string(), account.version().get() as u64,
        audit.event_type, occurred_at, correlation_id, None, audit.payload.clone(),
    )?).await?;
    let result_json = serde_json::to_value(&result).map_err(|error| LedgerError::persistence(error.to_string()))?;
    tx.insert_receipt(user_id, command_name, &idempotency_key, &request_hash, &result_json, now).await?;
    tx.commit().await?;
    Ok(result)
}

async fn replay<T>(
    tx: &mut T,
    user_id: crate::shared_kernel::UserId,
    command_name: &str,
    key: &crate::shared_kernel::IdempotencyKey,
    request_hash: &[u8; 32],
) -> Result<Option<AccountResult>, LedgerError>
where
    T: CommandReceiptStore,
{
    let Some(receipt) = tx.find_receipt(user_id, command_name, key, true).await? else {
        return Ok(None);
    };
    if &receipt.request_hash != request_hash {
        return Err(LedgerError::idempotency_conflict());
    }
    let mut result: AccountResult = serde_json::from_value(receipt.result)
        .map_err(|error| LedgerError::persistence(error.to_string()))?;
    result.replayed = true;
    Ok(Some(result))
}

async fn replay_after_failure<U: LedgerUnitOfWork>(
    uow: &U,
    user_id: crate::shared_kernel::UserId,
    command_name: &str,
    key: &crate::shared_kernel::IdempotencyKey,
    request_hash: &[u8; 32],
    original: LedgerError,
) -> Result<AccountResult, LedgerError> {
    let mut tx = uow.begin().await?;
    let result = replay(&mut tx, user_id, command_name, key, request_hash).await;
    tx.rollback().await?;
    match result? {
        Some(result) => Ok(result),
        None => Err(original),
    }
}

fn hash(value: &Value) -> Result<[u8; 32], LedgerError> {
    let canonical = serde_json::to_vec(value)
        .map_err(|error| LedgerError::persistence(error.to_string()))?;
    Ok(Sha256::digest(canonical).into())
}

fn view(
    account: &LedgerAccount,
    signed_balance: Decimal,
    balance_version: i64,
    as_of: DateTime<Utc>,
) -> AccountView {
    AccountView {
        id: account.id(), user_id: account.user_id(), name: account.name().to_owned(),
        currency: account.currency().clone(), nature: account.nature(), kind: account.kind(),
        authority: account.authority(), visibility: account.visibility(), lifecycle: account.lifecycle(),
        version: account.version(), signed_balance,
        display_balance: signed_balance * Decimal::from(account.normal_sign()),
        balance_version, as_of,
    }
}

async fn append_account_facts<T>(
    tx: &mut T,
    account: &LedgerAccount,
    command: &OpenAccount,
    now: DateTime<Utc>,
) -> Result<(), LedgerError>
where
    T: AuditStore + LedgerOutboxStore,
{
    let event_id = EventId::generate();
    let payload = json!({
        "account_id": account.id(), "currency": account.currency(), "nature": account.nature(),
        "kind": account.kind(), "lifecycle": account.lifecycle(), "version": account.version(),
    });
    tx.append_audit(&AuditRecord {
        event_id, user_id: account.user_id(), aggregate_kind: "account",
        aggregate_id: account.id().into_uuid(), event_type: "ledger.account-opened.v1",
        actor_kind: "user", actor_reference: Some(account.user_id().to_string()),
        correlation_id: command.correlation_id.into_uuid(), payload: payload.clone(),
        occurred_at: command.occurred_at, recorded_at: now,
    }).await?;
    tx.append_outbox(&integration_event(
        event_id, account.user_id(), account.id().to_string(), account.version().get() as u64,
        "ledger.account-opened.v1", command.occurred_at, command.correlation_id,
        command.causation_id, payload,
    )?).await
}

async fn append_journal_facts<T>(
    tx: &mut T,
    journal: &JournalEntry,
    event_type: &'static str,
) -> Result<(), LedgerError>
where
    T: AuditStore + LedgerOutboxStore,
{
    let event_id = EventId::generate();
    let payload = json!({
        "journal_entry_id": journal.id(), "source": journal.source(),
        "postings": journal.postings().iter().map(|posting| json!({
            "account_id": posting.account_id(), "currency": posting.currency(),
            "signed_amount": posting.signed_amount().to_string(),
            "display_effect": posting.display_effect().to_string(),
        })).collect::<Vec<_>>(),
    });
    tx.append_audit(&AuditRecord {
        event_id, user_id: journal.user_id(), aggregate_kind: "journal_entry",
        aggregate_id: journal.id().into_uuid(), event_type, actor_kind: "user",
        actor_reference: Some(journal.user_id().to_string()),
        correlation_id: journal.correlation_id().into_uuid(), payload: payload.clone(),
        occurred_at: journal.occurred_at(), recorded_at: journal.recorded_at(),
    }).await?;
    tx.append_outbox(&integration_event(
        event_id, journal.user_id(), journal.id().to_string(), 1, event_type,
        journal.occurred_at(), journal.correlation_id(), journal.causation_id(), payload,
    )?).await
}

#[allow(clippy::too_many_arguments)]
fn integration_event(
    event_id: EventId,
    user_id: crate::shared_kernel::UserId,
    aggregate_id: String,
    aggregate_version: u64,
    event_type: &'static str,
    occurred_at: DateTime<Utc>,
    correlation_id: crate::shared_kernel::CorrelationId,
    causation_id: Option<crate::shared_kernel::CausationId>,
    payload: Value,
) -> Result<IntegrationEvent, LedgerError> {
    let envelope = EventEnvelope::new(
        event_id, "ledger", aggregate_id, aggregate_version, event_type, 1,
        user_id, occurred_at, correlation_id, causation_id,
    ).map_err(|error| LedgerError::persistence(error.to_string()))?;
    Ok(IntegrationEvent::new(envelope, payload))
}
