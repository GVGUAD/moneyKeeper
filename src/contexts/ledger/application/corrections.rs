//! Visible balance correction and exact reversal commands.

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::shared_kernel::Clock;
use crate::contexts::classification::public::{CategoryCatalog, CategoryKind};

use super::{
    accounts::LedgerFacade,
    commit::commit_journal,
    ports::{
        CommandReceiptStore, CorrectionDetail, CorrectionStore, JournalStore, LedgerAccountStore,
        LedgerUnitOfWork, ProjectionStore, TransactionControl,
    },
};
use super::super::{
    domain::{
        Actor, AnnotationId, CategoryReference, JournalEntry, JournalEntryId, JournalRelations,
        JournalSource, LedgerAccount, LedgerAccountId, LedgerError, Posting, PostingId,
        PostingPurpose, SystemAccountRole, TransactionAnnotation,
    },
    public::{
        AccountEffect, CorrectBalance, FinancialChangeResult, ManualTransactionKind,
        ReplaceTransaction, ReplacementResult, ReverseTransaction,
    },
};

impl LedgerFacade {
    /// Posts the exact delta from current to target display balance.
    pub async fn correct_balance(&self, command: CorrectBalance) -> Result<FinancialChangeResult, LedgerError> {
        correct_balance(&self.uow, self.clock.as_ref(), command).await
    }

    /// Posts an exact inverse journal without modifying the original.
    pub async fn reverse_transaction(&self, command: ReverseTransaction) -> Result<FinancialChangeResult, LedgerError> {
        reverse_transaction(&self.uow, self.clock.as_ref(), command).await
    }

    /// Reverses and replaces a manual transaction inside one unit of work.
    pub async fn replace_transaction(&self, command: ReplaceTransaction) -> Result<ReplacementResult, LedgerError> {
        let categories = self.categories.as_ref().ok_or_else(|| {
            LedgerError::persistence("Classification catalog is not configured for Ledger")
        })?;
        replace_transaction(&self.uow, self.clock.as_ref(), categories, command).await
    }
}

async fn correct_balance<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    command: CorrectBalance,
) -> Result<FinancialChangeResult, LedgerError> {
    if command.expected_balance_version < 1 { return Err(LedgerError::invalid_version()) }
    if command.reason.trim().is_empty() || command.reason.chars().count() > 500 {
        return Err(LedgerError::invalid_annotation("correction reason is invalid"));
    }
    let request = json!({
        "account_id": command.account_id, "target": command.target_display_balance,
        "expected_balance_version": command.expected_balance_version,
        "reason": command.reason, "observed_at": command.observed_at,
        "occurred_at": command.occurred_at,
    });
    let request_hash = hash(&request)?;
    let mut tx = uow.begin().await?;
    if let Some(result) = replay::<_, FinancialChangeResult>(
        &mut tx, command.user_id, "correct_balance", &command.idempotency_key, &request_hash,
    ).await? {
        tx.rollback().await?;
        return Ok(result);
    }
    let outcome = async {
        let account = tx.find_account(command.user_id, command.account_id, true).await?
            .ok_or_else(LedgerError::not_found)?;
        if account.currency() != command.target_display_balance.currency() {
            return Err(LedgerError::currency_mismatch());
        }
        let (signed_before, balance_version) = tx.signed_balance(command.user_id, account.id(), true).await?
            .ok_or_else(LedgerError::not_found)?;
        if balance_version != command.expected_balance_version {
            return Err(LedgerError::version_conflict());
        }
        let display_before = signed_before * Decimal::from(account.normal_sign());
        let display_delta = command.target_display_balance.amount() - display_before;
        if display_delta.is_zero() {
            return Err(LedgerError::invalid_money("zero-delta correction is not a financial event"));
        }
        let signed_delta = display_delta * Decimal::from(account.normal_sign());
        let equity = ensure_system(&mut tx, command.user_id, &account, SystemAccountRole::BalanceAdjustmentEquity, clock).await?;
        let journal = JournalEntry::post(
            JournalEntryId::generate(), command.user_id, &command.reason,
            PostingPurpose::Correction, JournalSource::Correction, Actor::User(command.user_id),
            command.occurred_at, clock.now(), command.correlation_id, command.causation_id,
            command.idempotency_key.clone(), JournalRelations::none(),
            vec![
                Posting::for_account(PostingId::generate(), &account, signed_delta, PostingPurpose::Correction)?,
                Posting::for_account(PostingId::generate(), &equity, -signed_delta, PostingPurpose::Correction)?,
            ],
        )?;
        commit_journal(&mut tx, "correct_balance", &journal, None, "ledger.entry-posted.v1").await?;
        tx.insert_correction_detail(CorrectionDetail {
            journal_entry_id: journal.id(), user_id: command.user_id, account_id: account.id(),
            currency: account.currency(), before_display_balance: display_before,
            target_display_balance: command.target_display_balance.amount(), display_delta,
            observed_balance_version: balance_version, reason: &command.reason,
            observed_at: command.observed_at, recorded_at: journal.recorded_at(),
        }).await?;
        let effect = current_effect(&mut tx, &account, signed_delta).await?;
        let result = FinancialChangeResult { journal_entry_id: journal.id(), effects: vec![effect], replayed: false };
        store_result(&mut tx, command.user_id, "correct_balance", &command.idempotency_key, &request_hash, &result, journal.recorded_at()).await?;
        Ok::<_, LedgerError>(result)
    }.await;
    finish(uow, tx, outcome, command.user_id, "correct_balance", &command.idempotency_key, &request_hash).await
}

async fn reverse_transaction<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    command: ReverseTransaction,
) -> Result<FinancialChangeResult, LedgerError> {
    let request = json!({"journal_entry_id": command.journal_entry_id, "reason": command.reason, "occurred_at": command.occurred_at});
    let request_hash = hash(&request)?;
    let mut tx = uow.begin().await?;
    if let Some(result) = replay::<_, FinancialChangeResult>(
        &mut tx, command.user_id, "reverse_transaction", &command.idempotency_key, &request_hash,
    ).await? { tx.rollback().await?; return Ok(result) }
    let outcome = async {
        let original = tx.find_journal(command.user_id, command.journal_entry_id, true).await?
            .ok_or_else(LedgerError::not_found)?;
        let account_ids: Vec<_> = original.postings.iter().map(Posting::account_id).collect();
        let accounts = tx.lock_accounts(command.user_id, &account_ids).await?;
        if accounts.len() != account_ids.iter().copied().collect::<std::collections::BTreeSet<_>>().len() {
            return Err(LedgerError::not_found());
        }
        let postings = original.postings.iter().map(|posting| Posting::rehydrate(
            PostingId::generate(), posting.position(), posting.account_id(), posting.user_id(),
            posting.currency().clone(), posting.account_nature(), -posting.signed_amount(),
        )).collect();
        let journal = JournalEntry::post(
            JournalEntryId::generate(), command.user_id, &command.reason,
            PostingPurpose::Reversal, JournalSource::Correction, Actor::User(command.user_id),
            command.occurred_at, clock.now(), command.correlation_id, command.causation_id,
            command.idempotency_key.clone(), JournalRelations::reversal_of(original.id), postings,
        )?;
        commit_journal(&mut tx, "reverse_transaction", &journal, None, "ledger.entry-reversed.v1").await?;
        let nature_by_id: BTreeMap<_, _> = accounts.iter().map(|account| (account.id(), account)).collect();
        let mut amounts = BTreeMap::<_, Decimal>::new();
        for posting in journal.postings() { *amounts.entry(posting.account_id()).or_default() += posting.signed_amount(); }
        let mut effects = Vec::new();
        for (id, signed) in amounts {
            effects.push(current_effect(&mut tx, nature_by_id[&id], signed).await?);
        }
        let result = FinancialChangeResult { journal_entry_id: journal.id(), effects, replayed: false };
        store_result(&mut tx, command.user_id, "reverse_transaction", &command.idempotency_key, &request_hash, &result, journal.recorded_at()).await?;
        Ok::<_, LedgerError>(result)
    }.await;
    finish(uow, tx, outcome, command.user_id, "reverse_transaction", &command.idempotency_key, &request_hash).await
}

async fn replace_transaction<U: LedgerUnitOfWork, C: CategoryCatalog>(
    uow: &U,
    clock: &dyn Clock,
    categories: &C,
    command: ReplaceTransaction,
) -> Result<ReplacementResult, LedgerError> {
    if command.amount.is_zero() || command.amount.amount().is_sign_negative() {
        return Err(LedgerError::invalid_money("replacement amount must be positive"));
    }
    let category = match command.category_id {
        Some(id) => {
            let category = categories.require_active(command.user_id, id).await
                .map_err(|_| LedgerError::invalid_annotation("category is missing or archived"))?;
            let allowed = matches!(category.kind, CategoryKind::Both)
                || matches!((command.kind, category.kind),
                    (ManualTransactionKind::Income, CategoryKind::Income)
                    | (ManualTransactionKind::Expense, CategoryKind::Expense));
            if !allowed { return Err(LedgerError::invalid_annotation("category kind is incompatible")) }
            Some(CategoryReference::new(id.into_uuid()))
        }
        None => None,
    };
    let request = json!({
        "original": command.original_journal_entry_id, "account": command.account_id,
        "kind": command.kind, "amount": command.amount, "description": command.description,
        "category": command.category_id, "note": command.note, "tags": command.tags,
        "budget_visibility": command.budget_visibility, "occurred_at": command.occurred_at,
    });
    let request_hash = hash(&request)?;
    let mut tx = uow.begin().await?;
    if let Some(mut result) = replay::<_, ReplacementResult>(
        &mut tx, command.user_id, "replace_transaction", &command.idempotency_key, &request_hash,
    ).await? {
        result.replayed = true;
        tx.rollback().await?;
        return Ok(result);
    }
    let outcome = async {
        let original = tx.find_journal(command.user_id, command.original_journal_entry_id, true).await?
            .ok_or_else(LedgerError::not_found)?;
        let mut ids: Vec<_> = original.postings.iter().map(Posting::account_id).collect();
        ids.push(command.account_id);
        let accounts = tx.lock_accounts(command.user_id, &ids).await?;
        let replacement_account = accounts.iter().find(|account| account.id() == command.account_id)
            .ok_or_else(LedgerError::not_found)?;
        replacement_account.require_posting_allowed(PostingPurpose::Ordinary)?;
        if replacement_account.currency() != command.amount.currency() {
            return Err(LedgerError::currency_mismatch());
        }
        let reversal_postings = original.postings.iter().map(|posting| Posting::rehydrate(
            PostingId::generate(), posting.position(), posting.account_id(), posting.user_id(),
            posting.currency().clone(), posting.account_nature(), -posting.signed_amount(),
        )).collect();
        let reversal = JournalEntry::post(
            JournalEntryId::generate(), command.user_id, format!("Replace: {}", command.description),
            PostingPurpose::Reversal, JournalSource::Correction, Actor::User(command.user_id),
            command.occurred_at, clock.now(), command.correlation_id, command.causation_id,
            command.idempotency_key.clone(), JournalRelations::reversal_of(original.id), reversal_postings,
        )?;
        let role = match command.kind {
            ManualTransactionKind::Income => SystemAccountRole::UncategorizedIncome,
            ManualTransactionKind::Expense => SystemAccountRole::UncategorizedExpense,
        };
        let system = ensure_system(&mut tx, command.user_id, replacement_account, role, clock).await?;
        let amount = command.amount.amount();
        let (user_signed, counter_signed) = match command.kind {
            ManualTransactionKind::Expense => (-amount, amount),
            ManualTransactionKind::Income => (amount, -amount),
        };
        let replacement = JournalEntry::post(
            JournalEntryId::generate(), command.user_id, &command.description,
            PostingPurpose::Ordinary, JournalSource::Correction, Actor::User(command.user_id),
            command.occurred_at, reversal.recorded_at(), command.correlation_id, command.causation_id,
            command.idempotency_key.clone(), JournalRelations::replacement_of(original.id),
            vec![
                Posting::for_account(PostingId::generate(), replacement_account, user_signed, PostingPurpose::Ordinary)?,
                Posting::for_account(PostingId::generate(), &system, counter_signed, PostingPurpose::Ordinary)?,
            ],
        )?;
        let annotation = TransactionAnnotation::new(
            AnnotationId::generate(), replacement.id(), command.user_id, &command.description,
            category, command.note.clone(), command.tags.clone(), command.budget_visibility,
            replacement.recorded_at(),
        )?;
        commit_journal(&mut tx, "replace_transaction_reversal", &reversal, None, "ledger.entry-reversed.v1").await?;
        commit_journal(&mut tx, "replace_transaction", &replacement, Some(&annotation), "ledger.entry-replaced.v1").await?;
        let nature_by_id: BTreeMap<_, _> = accounts.iter().map(|account| (account.id(), account)).collect();
        let mut totals = BTreeMap::<_, Decimal>::new();
        for posting in reversal.postings().iter().chain(replacement.postings()) {
            if nature_by_id.contains_key(&posting.account_id()) {
                *totals.entry(posting.account_id()).or_default() += posting.signed_amount();
            }
        }
        let mut effects = Vec::new();
        for (id, amount) in totals { effects.push(current_effect(&mut tx, nature_by_id[&id], amount).await?); }
        let result = ReplacementResult {
            reversal_journal_entry_id: reversal.id(), replacement_journal_entry_id: replacement.id(),
            effects, replayed: false,
        };
        store_result(&mut tx, command.user_id, "replace_transaction", &command.idempotency_key, &request_hash, &result, replacement.recorded_at()).await?;
        Ok::<_, LedgerError>(result)
    }.await;
    match outcome {
        Ok(result) => match tx.commit().await {
            Ok(()) => Ok(result),
            Err(error) => replay_replacement_after(uow, &command, &request_hash, error).await,
        },
        Err(error) => { tx.rollback().await?; replay_replacement_after(uow, &command, &request_hash, error).await }
    }
}

async fn replay_replacement_after<U: LedgerUnitOfWork>(
    uow: &U, command: &ReplaceTransaction, hash: &[u8; 32], original: LedgerError,
) -> Result<ReplacementResult, LedgerError> {
    let mut tx = uow.begin().await?;
    let result = replay(&mut tx, command.user_id, "replace_transaction", &command.idempotency_key, hash).await;
    tx.rollback().await?;
    result?.ok_or(original)
}

async fn current_effect<T: ProjectionStore>(tx: &mut T, account: &LedgerAccount, signed: Decimal) -> Result<AccountEffect, LedgerError> {
    let (balance, version) = tx.signed_balance(account.user_id(), account.id(), false).await?
        .ok_or_else(LedgerError::not_found)?;
    Ok(AccountEffect {
        account_id: account.id(), currency: account.currency().clone(), signed_amount: signed,
        display_effect: signed * Decimal::from(account.normal_sign()), signed_balance: balance,
        display_balance: balance * Decimal::from(account.normal_sign()), balance_version: version,
    })
}

async fn ensure_system<T: LedgerAccountStore>(tx: &mut T, user_id: crate::shared_kernel::UserId, account: &LedgerAccount, role: SystemAccountRole, clock: &dyn Clock) -> Result<LedgerAccount, LedgerError> {
    if let Some(system) = tx.find_system_account(user_id, account.currency(), role, None).await? { return Ok(system) }
    let system = LedgerAccount::open_system(LedgerAccountId::generate(), user_id, account.currency().clone(), role, clock);
    tx.insert_account(&system).await?;
    Ok(system)
}

fn hash(value: &serde_json::Value) -> Result<[u8; 32], LedgerError> {
    Ok(Sha256::digest(serde_json::to_vec(value).map_err(|error| LedgerError::persistence(error.to_string()))?).into())
}

async fn replay<T: CommandReceiptStore, R: DeserializeOwned>(tx: &mut T, user_id: crate::shared_kernel::UserId, scope: &str, key: &crate::shared_kernel::IdempotencyKey, hash: &[u8; 32]) -> Result<Option<R>, LedgerError> {
    let Some(receipt) = tx.find_receipt(user_id, scope, key, true).await? else { return Ok(None) };
    if &receipt.request_hash != hash { return Err(LedgerError::idempotency_conflict()) }
    serde_json::from_value(receipt.result).map(Some).map_err(|error| LedgerError::persistence(error.to_string()))
}

async fn store_result<T: CommandReceiptStore, R: Serialize>(tx: &mut T, user_id: crate::shared_kernel::UserId, scope: &str, key: &crate::shared_kernel::IdempotencyKey, hash: &[u8; 32], result: &R, now: chrono::DateTime<chrono::Utc>) -> Result<(), LedgerError> {
    let value = serde_json::to_value(result).map_err(|error| LedgerError::persistence(error.to_string()))?;
    tx.insert_receipt(user_id, scope, key, hash, &value, now).await
}

async fn finish<U: LedgerUnitOfWork>(uow: &U, tx: U::Tx<'_>, outcome: Result<FinancialChangeResult, LedgerError>, user_id: crate::shared_kernel::UserId, scope: &str, key: &crate::shared_kernel::IdempotencyKey, hash: &[u8; 32]) -> Result<FinancialChangeResult, LedgerError> {
    match outcome {
        Ok(result) => match tx.commit().await { Ok(()) => Ok(result), Err(error) => replay_after(uow, user_id, scope, key, hash, error).await },
        Err(error) => { tx.rollback().await?; replay_after(uow, user_id, scope, key, hash, error).await }
    }
}

async fn replay_after<U: LedgerUnitOfWork>(uow: &U, user_id: crate::shared_kernel::UserId, scope: &str, key: &crate::shared_kernel::IdempotencyKey, hash: &[u8; 32], original: LedgerError) -> Result<FinancialChangeResult, LedgerError> {
    let mut tx = uow.begin().await?;
    let result = replay(&mut tx, user_id, scope, key, hash).await;
    tx.rollback().await?;
    result?.ok_or(original)
}
