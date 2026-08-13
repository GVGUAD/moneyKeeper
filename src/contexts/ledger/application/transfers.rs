//! Atomic same-currency and FX transfer orchestration.

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::shared_kernel::Clock;

use super::super::{
    domain::{
        Actor, JournalEntry, JournalEntryId, JournalRelations, JournalSource, LedgerAccount,
        LedgerAccountId, LedgerError, Posting, PostingId, PostingPurpose, SystemAccountRole,
    },
    public::{AccountEffect, TransferFunds, TransferResult},
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
    /// Transfers exact amounts and optional fees in one immutable journal.
    pub async fn transfer(&self, command: TransferFunds) -> Result<TransferResult, LedgerError> {
        transfer(&self.uow, self.clock.as_ref(), command).await
    }
}

async fn transfer<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    command: TransferFunds,
) -> Result<TransferResult, LedgerError> {
    validate(&command)?;
    let request = json!({
        "source_account_id": command.source_account_id,
        "target_account_id": command.target_account_id,
        "source_amount": command.source_amount,
        "target_amount": command.target_amount,
        "fee": command.fee.as_ref().map(|fee| &fee.amount),
        "implied_rate": command.implied_rate.map(|rate| rate.to_string()),
        "description": command.description,
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
        &command.idempotency_key,
        &request_hash,
    )
    .await?
    {
        tx.rollback().await?;
        return Ok(result);
    }

    let outcome = async {
        let locked = tx
            .lock_accounts(
                command.user_id,
                &[command.source_account_id, command.target_account_id],
            )
            .await?;
        if locked.len() != 2 {
            return Err(LedgerError::not_found());
        }
        let source = locked
            .iter()
            .find(|account| account.id() == command.source_account_id)
            .ok_or_else(LedgerError::not_found)?;
        let target = locked
            .iter()
            .find(|account| account.id() == command.target_account_id)
            .ok_or_else(LedgerError::not_found)?;
        source.require_posting_allowed(PostingPurpose::Ordinary)?;
        target.require_posting_allowed(PostingPurpose::Ordinary)?;
        if source.currency() != command.source_amount.currency()
            || target.currency() != command.target_amount.currency()
        {
            return Err(LedgerError::currency_mismatch());
        }

        let mut postings = Vec::new();
        let same_currency = source.currency() == target.currency();
        if same_currency {
            if command.source_amount.amount() != command.target_amount.amount()
                || command.implied_rate.is_some()
            {
                return Err(LedgerError::invalid_money(
                    "same-currency transfer amounts must be equal and have no FX rate",
                ));
            }
            postings.push(Posting::for_account(
                PostingId::generate(),
                source,
                -command.source_amount.amount(),
                PostingPurpose::Ordinary,
            )?);
            postings.push(Posting::for_account(
                PostingId::generate(),
                target,
                command.target_amount.amount(),
                PostingPurpose::Ordinary,
            )?);
        } else {
            let rate = command.implied_rate.ok_or_else(|| {
                LedgerError::invalid_money("cross-currency transfer requires an implied rate")
            })?;
            if rate <= Decimal::ZERO {
                return Err(LedgerError::invalid_money(
                    "implied FX rate must be positive",
                ));
            }
            let source_clearing = ensure_system(
                &mut tx,
                command.user_id,
                source,
                SystemAccountRole::FxClearing,
                clock,
            )
            .await?;
            let target_clearing = ensure_system(
                &mut tx,
                command.user_id,
                target,
                SystemAccountRole::FxClearing,
                clock,
            )
            .await?;
            postings.extend([
                Posting::for_account(
                    PostingId::generate(),
                    source,
                    -command.source_amount.amount(),
                    PostingPurpose::Ordinary,
                )?,
                Posting::for_account(
                    PostingId::generate(),
                    &source_clearing,
                    command.source_amount.amount(),
                    PostingPurpose::Ordinary,
                )?,
                Posting::for_account(
                    PostingId::generate(),
                    &target_clearing,
                    -command.target_amount.amount(),
                    PostingPurpose::Ordinary,
                )?,
                Posting::for_account(
                    PostingId::generate(),
                    target,
                    command.target_amount.amount(),
                    PostingPurpose::Ordinary,
                )?,
            ]);
        }

        if let Some(fee) = &command.fee {
            let fee_account = if fee.amount.currency() == source.currency() {
                source
            } else if fee.amount.currency() == target.currency() {
                target
            } else {
                return Err(LedgerError::currency_mismatch());
            };
            let expense = ensure_system(
                &mut tx,
                command.user_id,
                fee_account,
                SystemAccountRole::UncategorizedExpense,
                clock,
            )
            .await?;
            postings.push(Posting::for_account(
                PostingId::generate(),
                fee_account,
                -fee.amount.amount(),
                PostingPurpose::Ordinary,
            )?);
            postings.push(Posting::for_account(
                PostingId::generate(),
                &expense,
                fee.amount.amount(),
                PostingPurpose::Ordinary,
            )?);
        }

        let mut journal = JournalEntry::post(
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
            postings,
        )?;
        if let Some(rate) = command.implied_rate {
            journal = journal.with_fx_rate(rate)?;
        }
        commit_journal(
            &mut tx,
            "transfer_funds",
            &journal,
            None,
            "ledger.entry-posted.v1",
        )
        .await?;

        let mut effects_by_account = BTreeMap::new();
        for posting in journal.postings().iter().filter(|posting| {
            posting.account_id() == source.id() || posting.account_id() == target.id()
        }) {
            effects_by_account
                .entry(posting.account_id())
                .and_modify(|amount| *amount += posting.signed_amount())
                .or_insert(posting.signed_amount());
        }
        let mut effects = Vec::new();
        for account in [source, target] {
            let signed_amount = effects_by_account
                .get(&account.id())
                .copied()
                .unwrap_or_default();
            let (signed_balance, balance_version) = tx
                .signed_balance(command.user_id, account.id(), false)
                .await?
                .ok_or_else(LedgerError::not_found)?;
            effects.push(AccountEffect {
                account_id: account.id(),
                currency: account.currency().clone(),
                signed_amount,
                display_effect: signed_amount * Decimal::from(account.normal_sign()),
                signed_balance,
                display_balance: signed_balance * Decimal::from(account.normal_sign()),
                balance_version,
            });
        }
        let result = TransferResult {
            journal_entry_id: journal.id(),
            effects,
            implied_rate: journal.fx_rate(),
            replayed: false,
        };
        let result_json = serde_json::to_value(&result)
            .map_err(|error| LedgerError::persistence(error.to_string()))?;
        tx.insert_receipt(
            command.user_id,
            "transfer_funds",
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
            Err(error) => replay_after_failure(uow, &command, &request_hash, error).await,
        },
        Err(error) => {
            tx.rollback().await?;
            replay_after_failure(uow, &command, &request_hash, error).await
        }
    }
}

async fn ensure_system<T: LedgerAccountStore>(
    tx: &mut T,
    user_id: crate::shared_kernel::UserId,
    currency_account: &LedgerAccount,
    role: SystemAccountRole,
    clock: &dyn Clock,
) -> Result<LedgerAccount, LedgerError> {
    if let Some(account) = tx
        .find_system_account(user_id, currency_account.currency(), role, None)
        .await?
    {
        return Ok(account);
    }
    let account = LedgerAccount::open_system(
        LedgerAccountId::generate(),
        user_id,
        currency_account.currency().clone(),
        role,
        clock,
    );
    tx.insert_account(&account).await?;
    Ok(account)
}

fn validate(command: &TransferFunds) -> Result<(), LedgerError> {
    if command.source_account_id == command.target_account_id {
        return Err(LedgerError::invalid_state(
            "source and target accounts must differ",
        ));
    }
    for amount in [&command.source_amount, &command.target_amount] {
        if amount.is_zero() || amount.amount().is_sign_negative() {
            return Err(LedgerError::invalid_money(
                "transfer amounts must be positive",
            ));
        }
    }
    if command
        .fee
        .as_ref()
        .is_some_and(|fee| fee.amount.is_zero() || fee.amount.amount().is_sign_negative())
    {
        return Err(LedgerError::invalid_money("transfer fee must be positive"));
    }
    Ok(())
}

async fn replay<T: CommandReceiptStore>(
    tx: &mut T,
    user_id: crate::shared_kernel::UserId,
    key: &crate::shared_kernel::IdempotencyKey,
    request_hash: &[u8; 32],
) -> Result<Option<TransferResult>, LedgerError> {
    let Some(receipt) = tx
        .find_receipt(user_id, "transfer_funds", key, true)
        .await?
    else {
        return Ok(None);
    };
    if &receipt.request_hash != request_hash {
        return Err(LedgerError::idempotency_conflict());
    }
    let mut result: TransferResult = serde_json::from_value(receipt.result)
        .map_err(|error| LedgerError::persistence(error.to_string()))?;
    result.replayed = true;
    Ok(Some(result))
}

async fn replay_after_failure<U: LedgerUnitOfWork>(
    uow: &U,
    command: &TransferFunds,
    request_hash: &[u8; 32],
    original: LedgerError,
) -> Result<TransferResult, LedgerError> {
    let mut tx = uow.begin().await?;
    let replayed = replay(
        &mut tx,
        command.user_id,
        &command.idempotency_key,
        request_hash,
    )
    .await;
    tx.rollback().await?;
    replayed?.ok_or(original)
}
