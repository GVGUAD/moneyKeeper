//! Manual income and expense command orchestration.

use rust_decimal::Decimal;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::contexts::classification::public::{CategoryCatalog, CategoryKind};
use crate::shared_kernel::Clock;

use super::super::{
    domain::{
        Actor, AnnotationId, CategoryReference, JournalEntry, JournalEntryId, JournalRelations,
        JournalSource, LedgerAccount, LedgerAccountId, LedgerError, Posting, PostingId,
        PostingPurpose, SystemAccountRole, TransactionAnnotation,
    },
    public::{AccountEffect, ManualTransactionKind, RecordManualTransaction, TransactionResult},
};
use super::{
    accounts::LedgerFacade,
    commit::commit_journal,
    ports::{
        CommandReceiptStore, LedgerAccountStore, LedgerUnitOfWork, ProjectionStore,
        TransactionControl,
    },
};

impl LedgerFacade {
    /// Records manual income or expense through a controlled journal recipe.
    pub async fn record_manual_transaction(
        &self,
        command: RecordManualTransaction,
    ) -> Result<TransactionResult, LedgerError> {
        let categories = self.categories.as_ref().ok_or_else(|| {
            LedgerError::persistence("Classification catalog is not configured for Ledger")
        })?;
        record_manual_transaction(&self.uow, self.clock.as_ref(), categories, command).await
    }
}

async fn record_manual_transaction<U: LedgerUnitOfWork, C: CategoryCatalog>(
    uow: &U,
    clock: &dyn Clock,
    categories: &C,
    command: RecordManualTransaction,
) -> Result<TransactionResult, LedgerError> {
    if command.amount.is_zero() || command.amount.amount().is_sign_negative() {
        return Err(LedgerError::invalid_money(
            "manual transaction amount must be positive",
        ));
    }
    let category = match command.category_id {
        Some(id) => {
            let category = categories
                .require_active(command.user_id, id)
                .await
                .map_err(|_| LedgerError::invalid_annotation("category is missing or archived"))?;
            let allowed = matches!(category.kind, CategoryKind::Both)
                || matches!(
                    (command.kind, category.kind),
                    (ManualTransactionKind::Income, CategoryKind::Income)
                        | (ManualTransactionKind::Expense, CategoryKind::Expense)
                );
            if !allowed {
                return Err(LedgerError::invalid_annotation(
                    "category kind is incompatible with the transaction",
                ));
            }
            Some(CategoryReference::new(id.into_uuid()))
        }
        None => None,
    };
    let request = json!({
        "account_id": command.account_id,
        "kind": command.kind,
        "amount": command.amount,
        "description": command.description,
        "category_id": command.category_id,
        "note": command.note,
        "tags": command.tags,
        "budget_visibility": command.budget_visibility,
        "occurred_at": command.occurred_at,
    });
    let request_hash: [u8; 32] = Sha256::digest(
        serde_json::to_vec(&request)
            .map_err(|error| LedgerError::persistence(error.to_string()))?,
    )
    .into();
    let mut tx = uow.begin().await?;
    if let Some(result) = replay(
        &mut tx,
        command.user_id,
        command.kind.command_name(),
        &command.idempotency_key,
        &request_hash,
    )
    .await?
    {
        tx.rollback().await?;
        return Ok(result);
    }

    let outcome = async {
        let account = tx
            .find_account(command.user_id, command.account_id, true)
            .await?
            .ok_or_else(LedgerError::not_found)?;
        if account.currency() != command.amount.currency() {
            return Err(LedgerError::currency_mismatch());
        }
        account.require_posting_allowed(PostingPurpose::Ordinary)?;
        let role = match command.kind {
            ManualTransactionKind::Income => SystemAccountRole::UncategorizedIncome,
            ManualTransactionKind::Expense => SystemAccountRole::UncategorizedExpense,
        };
        let system = match tx
            .find_system_account(command.user_id, account.currency(), role, None)
            .await?
        {
            Some(account) => account,
            None => {
                let system = LedgerAccount::open_system(
                    LedgerAccountId::generate(),
                    command.user_id,
                    account.currency().clone(),
                    role,
                    clock,
                );
                tx.insert_account(&system).await?;
                system
            }
        };
        let amount = command.amount.amount();
        let (user_signed, system_signed) = match command.kind {
            ManualTransactionKind::Expense => (-amount, amount),
            ManualTransactionKind::Income => (amount, -amount),
        };
        let journal = JournalEntry::post(
            JournalEntryId::generate(),
            command.user_id,
            &command.description,
            PostingPurpose::Ordinary,
            JournalSource::Manual,
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
                    user_signed,
                    PostingPurpose::Ordinary,
                )?,
                Posting::for_account(
                    PostingId::generate(),
                    &system,
                    system_signed,
                    PostingPurpose::Ordinary,
                )?,
            ],
        )?;
        let annotation = TransactionAnnotation::new(
            AnnotationId::generate(),
            journal.id(),
            command.user_id,
            &command.description,
            category,
            command.note.clone(),
            command.tags.clone(),
            command.budget_visibility,
            journal.recorded_at(),
        )?;
        commit_journal(
            &mut tx,
            command.kind.command_name(),
            &journal,
            Some(&annotation),
            "ledger.journal-posted.v1",
        )
        .await?;
        let (signed_balance, balance_version) = tx
            .signed_balance(command.user_id, account.id(), false)
            .await?
            .ok_or_else(LedgerError::not_found)?;
        let effect = AccountEffect {
            account_id: account.id(),
            currency: account.currency().clone(),
            signed_amount: user_signed,
            display_effect: user_signed * Decimal::from(account.normal_sign()),
            signed_balance,
            display_balance: signed_balance * Decimal::from(account.normal_sign()),
            balance_version,
        };
        let result = TransactionResult {
            journal_entry_id: journal.id(),
            effects: vec![effect],
            annotation_version: annotation.version(),
            replayed: false,
        };
        let result_json = serde_json::to_value(&result)
            .map_err(|error| LedgerError::persistence(error.to_string()))?;
        tx.insert_receipt(
            command.user_id,
            command.kind.command_name(),
            &command.idempotency_key,
            &request_hash,
            &result_json,
            journal.recorded_at(),
        )
        .await?;
        Ok::<_, LedgerError>(result)
    }
    .await;

    match outcome {
        Ok(result) => match tx.commit().await {
            Ok(()) => Ok(result),
            Err(error) => {
                replay_after_failure(
                    uow,
                    command.user_id,
                    command.kind.command_name(),
                    &command.idempotency_key,
                    &request_hash,
                    error,
                )
                .await
            }
        },
        Err(error) => {
            tx.rollback().await?;
            replay_after_failure(
                uow,
                command.user_id,
                command.kind.command_name(),
                &command.idempotency_key,
                &request_hash,
                error,
            )
            .await
        }
    }
}

impl ManualTransactionKind {
    fn command_name(self) -> &'static str {
        match self {
            Self::Income => "record_income",
            Self::Expense => "record_expense",
        }
    }
}

async fn replay<T: CommandReceiptStore>(
    tx: &mut T,
    user_id: crate::shared_kernel::UserId,
    scope: &str,
    key: &crate::shared_kernel::IdempotencyKey,
    request_hash: &[u8; 32],
) -> Result<Option<TransactionResult>, LedgerError> {
    if let Some(receipt) = tx.find_receipt(user_id, scope, key, true).await? {
        if &receipt.request_hash != request_hash {
            return Err(LedgerError::idempotency_conflict());
        }
        let mut result: TransactionResult = serde_json::from_value(receipt.result)
            .map_err(|error| LedgerError::persistence(error.to_string()))?;
        result.replayed = true;
        return Ok(Some(result));
    }
    Ok(None)
}

async fn replay_after_failure<U: LedgerUnitOfWork>(
    uow: &U,
    user_id: crate::shared_kernel::UserId,
    scope: &str,
    key: &crate::shared_kernel::IdempotencyKey,
    request_hash: &[u8; 32],
    original: LedgerError,
) -> Result<TransactionResult, LedgerError> {
    let mut tx = uow.begin().await?;
    let receipt = tx.find_receipt(user_id, scope, key, false).await?;
    tx.rollback().await?;
    let Some(receipt) = receipt else {
        return Err(original);
    };
    if &receipt.request_hash != request_hash {
        return Err(LedgerError::idempotency_conflict());
    }
    let mut result: TransactionResult = serde_json::from_value(receipt.result)
        .map_err(|error| LedgerError::persistence(error.to_string()))?;
    result.replayed = true;
    Ok(result)
}
