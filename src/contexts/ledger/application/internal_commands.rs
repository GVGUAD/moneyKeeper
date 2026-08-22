//! Closed accounting recipes for later-context process managers.

use std::collections::{BTreeMap, BTreeSet};

use rust_decimal::Decimal;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::shared_kernel::Clock;

use super::super::{
    domain::{
        Actor, JournalEntry, JournalEntryId, JournalRelations, JournalSource, LedgerAccount,
        LedgerAccountId, LedgerError, Posting, PostingId, PostingPurpose, SystemAccountRole,
    },
    public::{
        AccountEffect, CancelOrReverseCashControlSettlement, CashFlowDirection,
        ControlAccountResult, ControlAccountRole, ControlDirection, EnsureTypedControlAccount,
        ImportProviderTransaction, InternalAccountingResult, ProjectionVersion,
        ProviderTransactionState, ReclassifyExpenseToReceivableOrPayable,
        ReclassifyImportedSettlement, RecordCashControlSettlement, RecordExpenseAndControlBalances,
        RecordInterestAndFee, RecordInterestOrFeeAccrual, RecordPrincipalDisbursement,
        RecordPrincipalRepayment, ReverseProviderTransaction, ReverseTransaction,
        SettleReceivableOrPayable, TransitionProviderTransactionState,
        WriteOffLiabilityOrReceivable,
    },
};
use super::{
    accounts::LedgerFacade,
    commit::commit_journal,
    ports::{
        CommandReceiptStore, JournalStore, LedgerAccountStore, LedgerUnitOfWork, ProjectionStore,
        TransactionControl,
    },
};

impl LedgerFacade {
    /// Deterministically ensures one hidden control account for a typed role and subject.
    pub async fn ensure_typed_control_account(
        &self,
        command: EnsureTypedControlAccount,
    ) -> Result<ControlAccountResult, LedgerError> {
        ensure_control(&self.uow, self.clock.as_ref(), command).await
    }

    /// Posts the closed Sharing recipe without accepting caller-defined postings.
    pub async fn record_expense_and_control_balances(
        &self,
        command: RecordExpenseAndControlBalances,
    ) -> Result<InternalAccountingResult, LedgerError> {
        record_expense_controls(&self.uow, self.clock.as_ref(), command).await
    }

    pub async fn import_provider_transaction(
        &self,
        command: ImportProviderTransaction,
    ) -> Result<InternalAccountingResult, LedgerError> {
        import_provider(self, command).await
    }

    pub async fn transition_provider_transaction_state(
        &self,
        command: TransitionProviderTransactionState,
    ) -> Result<InternalAccountingResult, LedgerError> {
        if command.from == command.to {
            return Err(LedgerError::invalid_state(
                "provider state transition must change state",
            ));
        }
        if command.to == ProviderTransactionState::Reversed {
            return self
                .reverse_provider_transaction(ReverseProviderTransaction {
                    metadata: command.metadata,
                    imported_journal_entry_id: command.imported_journal_entry_id,
                    reason: "Provider transaction state reversed".to_owned(),
                })
                .await;
        }
        Ok(empty_result(command.metadata.correlation_id))
    }

    pub async fn reverse_provider_transaction(
        &self,
        command: ReverseProviderTransaction,
    ) -> Result<InternalAccountingResult, LedgerError> {
        let result = self
            .reverse_transaction(ReverseTransaction {
                user_id: command.metadata.user_id,
                journal_entry_id: command.imported_journal_entry_id,
                reason: command.reason,
                idempotency_key: command.metadata.idempotency_key,
                correlation_id: command.metadata.correlation_id,
                causation_id: command.metadata.causation_id,
                occurred_at: command.metadata.occurred_at,
            })
            .await?;
        Ok(InternalAccountingResult {
            journal_entry_id: Some(result.journal_entry_id),
            projection_versions: result
                .effects
                .iter()
                .map(|effect| ProjectionVersion {
                    account_id: effect.account_id,
                    version: effect.balance_version,
                })
                .collect(),
            effects: result.effects,
            replayed: result.replayed,
            cancelled: false,
            outbox_correlation_id: command.metadata.correlation_id,
        })
    }

    pub async fn settle_receivable_or_payable(
        &self,
        command: SettleReceivableOrPayable,
    ) -> Result<InternalAccountingResult, LedgerError> {
        let (cash_sign, control_sign) = match command.direction {
            ControlDirection::Receivable => (1, -1),
            ControlDirection::Payable => (-1, 1),
        };
        post_pair(
            &self.uow,
            self.clock.as_ref(),
            command.metadata,
            "settle_receivable_or_payable",
            command.user_account_id,
            cash_sign,
            command.control_account_id,
            control_sign,
            command.amount,
            "Settle receivable or payable",
        )
        .await
    }

    pub async fn record_principal_disbursement(
        &self,
        command: RecordPrincipalDisbursement,
    ) -> Result<InternalAccountingResult, LedgerError> {
        post_by_control_nature(
            &self.uow,
            self.clock.as_ref(),
            command.metadata,
            "record_principal_disbursement",
            command.cash_account_id,
            command.principal_control_account_id,
            command.amount,
            false,
        )
        .await
    }

    pub async fn record_principal_repayment(
        &self,
        command: RecordPrincipalRepayment,
    ) -> Result<InternalAccountingResult, LedgerError> {
        post_by_control_nature(
            &self.uow,
            self.clock.as_ref(),
            command.metadata,
            "record_principal_repayment",
            command.cash_account_id,
            command.principal_control_account_id,
            command.amount,
            true,
        )
        .await
    }

    pub async fn record_interest_and_fee(
        &self,
        command: RecordInterestAndFee,
    ) -> Result<InternalAccountingResult, LedgerError> {
        let (cash_sign, control_sign) = match command.direction {
            ControlDirection::Receivable => (1, -1),
            ControlDirection::Payable => (-1, 1),
        };
        post_pair(
            &self.uow,
            self.clock.as_ref(),
            command.metadata,
            "record_interest_and_fee",
            command.cash_account_id,
            cash_sign,
            command.accrual_control_account_id,
            control_sign,
            command.amount,
            "Record interest or fee",
        )
        .await
    }

    /// Posts a closed manual accrual recipe. The caller identifies only the
    /// typed control account and component; Ledger owns the offset account.
    pub async fn record_interest_or_fee_accrual(
        &self,
        command: RecordInterestOrFeeAccrual,
    ) -> Result<InternalAccountingResult, LedgerError> {
        if command.component == super::super::public::PrincipalOrAccrual::Principal {
            return Err(LedgerError::invalid_state(
                "principal cannot use an accrual recipe",
            ));
        }
        let (control_sign, role) = match command.direction {
            ControlDirection::Receivable => (1, SystemAccountRole::UncategorizedIncome),
            ControlDirection::Payable => (-1, SystemAccountRole::UncategorizedExpense),
        };
        post_with_system(
            &self.uow,
            self.clock.as_ref(),
            command.metadata,
            "record_interest_or_fee_accrual",
            command.accrual_control_account_id,
            control_sign,
            role,
            command.amount,
            "Accrue loan interest or fee",
        )
        .await
    }

    pub async fn reclassify_expense_to_receivable_or_payable(
        &self,
        command: ReclassifyExpenseToReceivableOrPayable,
    ) -> Result<InternalAccountingResult, LedgerError> {
        let role = SystemAccountRole::UncategorizedExpense;
        post_with_system(
            &self.uow,
            self.clock.as_ref(),
            command.metadata,
            "reclassify_expense",
            command.control_account_id,
            if command.direction == ControlDirection::Receivable {
                1
            } else {
                -1
            },
            role,
            command.amount,
            "Reclassify expense",
        )
        .await
    }

    /// Appends a typed reclassification for an already imported settlement.
    pub async fn reclassify_imported_settlement(
        &self,
        command: ReclassifyImportedSettlement,
    ) -> Result<InternalAccountingResult, LedgerError> {
        let (control_sign, role) = match command.direction {
            ControlDirection::Receivable => (-1, SystemAccountRole::UncategorizedIncome),
            ControlDirection::Payable => (1, SystemAccountRole::UncategorizedExpense),
        };
        post_with_system(
            &self.uow,
            self.clock.as_ref(),
            command.metadata,
            "reclassify_imported_settlement",
            command.control_account_id,
            control_sign,
            role,
            command.amount,
            "Reclassify imported settlement",
        )
        .await
    }

    pub async fn write_off_liability_or_receivable(
        &self,
        command: WriteOffLiabilityOrReceivable,
    ) -> Result<InternalAccountingResult, LedgerError> {
        let (control_sign, role) = match command.direction {
            ControlDirection::Receivable => (-1, SystemAccountRole::BadDebtExpense),
            ControlDirection::Payable => (1, SystemAccountRole::DebtForgivenessIncome),
        };
        post_with_system(
            &self.uow,
            self.clock.as_ref(),
            command.metadata,
            "write_off_control",
            command.control_account_id,
            control_sign,
            role,
            command.amount,
            &command.reason,
        )
        .await
    }

    pub async fn record_cash_control_settlement(
        &self,
        mut command: RecordCashControlSettlement,
    ) -> Result<InternalAccountingResult, LedgerError> {
        let operation_key =
            crate::shared_kernel::IdempotencyKey::new(command.source_operation_id.clone())
                .map_err(|_| LedgerError::invalid_source_reference())?;
        command.metadata.idempotency_key = operation_key;
        let inverse = matches!(command.cash_flow, CashFlowDirection::Outgoing);
        post_by_control_nature(
            &self.uow,
            self.clock.as_ref(),
            command.metadata,
            "cash_control_operation",
            command.cash_account_id,
            command.control_account_id,
            command.amount,
            inverse,
        )
        .await
    }

    pub async fn cancel_or_reverse_cash_control_settlement(
        &self,
        command: CancelOrReverseCashControlSettlement,
    ) -> Result<InternalAccountingResult, LedgerError> {
        let operation_key =
            crate::shared_kernel::IdempotencyKey::new(command.source_operation_id.clone())
                .map_err(|_| LedgerError::invalid_source_reference())?;
        let mut tx = self.uow.begin().await?;
        let receipt = tx
            .find_receipt(
                command.metadata.user_id,
                "cash_control_operation",
                &operation_key,
                true,
            )
            .await?;
        let Some(receipt) = receipt else {
            let mut cancelled = empty_result(command.metadata.correlation_id);
            cancelled.cancelled = true;
            let hash = digest(&json!({"source_operation_id": command.source_operation_id}))?;
            let value = serde_json::to_value(&cancelled)
                .map_err(|error| LedgerError::persistence(error.to_string()))?;
            tx.insert_cancelled_receipt(
                command.metadata.user_id,
                "cash_control_operation",
                &operation_key,
                &hash,
                &value,
                self.clock.now(),
            )
            .await?;
            tx.commit().await?;
            return Ok(cancelled);
        };
        let stored: InternalAccountingResult = serde_json::from_value(receipt.result)
            .map_err(|error| LedgerError::persistence(error.to_string()))?;
        if stored.cancelled {
            tx.rollback().await?;
            return Ok(stored);
        }
        let original_id = stored.journal_entry_id.ok_or_else(|| {
            LedgerError::invalid_state("posted cash-control operation has no journal")
        })?;
        let original = tx
            .find_journal(command.metadata.user_id, original_id, true)
            .await?
            .ok_or_else(LedgerError::not_found)?;
        let account_ids: Vec<_> = original
            .postings
            .iter()
            .map(Posting::account_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let accounts = tx
            .lock_accounts(command.metadata.user_id, &account_ids)
            .await?;
        if accounts.len() != account_ids.len() {
            return Err(LedgerError::not_found());
        }
        let postings = original
            .postings
            .iter()
            .map(|posting| {
                Posting::rehydrate(
                    PostingId::generate(),
                    posting.position(),
                    posting.account_id(),
                    posting.user_id(),
                    posting.currency().clone(),
                    posting.account_nature(),
                    -posting.signed_amount(),
                )
            })
            .collect();
        let journal = JournalEntry::post(
            JournalEntryId::generate(),
            command.metadata.user_id,
            &command.reason,
            PostingPurpose::Reversal,
            JournalSource::System,
            Actor::External {
                source_kind: command.metadata.source.source_kind().to_owned(),
                source_reference: command.metadata.source.item_id().to_owned(),
            },
            command.metadata.occurred_at,
            self.clock.now(),
            command.metadata.correlation_id,
            command.metadata.causation_id,
            command.metadata.idempotency_key,
            JournalRelations::reversal_of(original.id),
            postings,
        )?;
        commit_journal(
            &mut tx,
            "cancel_cash_control_settlement",
            &journal,
            None,
            "ledger.journal-reversed.v1",
        )
        .await?;
        let mut cancelled = accounting_result(
            &mut tx,
            command.metadata.user_id,
            command.metadata.correlation_id,
            &journal,
            &accounts,
        )
        .await?;
        cancelled.cancelled = true;
        let value = serde_json::to_value(&cancelled)
            .map_err(|error| LedgerError::persistence(error.to_string()))?;
        tx.cancel_receipt(
            command.metadata.user_id,
            "cash_control_operation",
            &operation_key,
            &value,
            journal.recorded_at(),
        )
        .await?;
        tx.commit().await?;
        Ok(cancelled)
    }
}

async fn ensure_control<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    command: EnsureTypedControlAccount,
) -> Result<ControlAccountResult, LedgerError> {
    validate_subject(&command.subject_reference)?;
    let role = system_role(command.role);
    let hash = digest(&json!({
        "source": command.metadata.source, "role": command.role,
        "subject_reference": command.subject_reference, "currency": command.currency,
    }))?;
    let mut tx = uow.begin().await?;
    if let Some(mut result) = replay::<_, ControlAccountResult>(
        &mut tx,
        command.metadata.user_id,
        "ensure_typed_control_account",
        &command.metadata.idempotency_key,
        &hash,
    )
    .await?
    {
        result.replayed = true;
        tx.rollback().await?;
        return Ok(result);
    }
    let account = match tx
        .find_system_account(
            command.metadata.user_id,
            &command.currency,
            role,
            Some(&command.subject_reference),
        )
        .await?
    {
        Some(account) => account,
        None => {
            let account = LedgerAccount::open_system(
                LedgerAccountId::generate(),
                command.metadata.user_id,
                command.currency.clone(),
                role,
                clock,
            );
            tx.insert_system_account(&account, &command.subject_reference)
                .await?;
            account
        }
    };
    let result = ControlAccountResult {
        account_id: account.id(),
        role: command.role,
        subject_reference: command.subject_reference,
        currency: command.currency,
        replayed: false,
    };
    store(
        &mut tx,
        command.metadata.user_id,
        "ensure_typed_control_account",
        &command.metadata.idempotency_key,
        &hash,
        &result,
        clock.now(),
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

async fn record_expense_controls<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    command: RecordExpenseAndControlBalances,
) -> Result<InternalAccountingResult, LedgerError> {
    let currency = command.expense.currency().clone();
    if command.expense.amount().is_sign_negative() {
        return Err(LedgerError::invalid_money("expense cannot be negative"));
    }
    let all_money = command
        .cash_contributions
        .iter()
        .map(|leg| &leg.amount)
        .chain(command.receivables.iter().map(|leg| &leg.amount))
        .chain(command.payables.iter().map(|leg| &leg.amount));
    for money in all_money {
        if money.currency() != &currency || money.amount().is_sign_negative() {
            return Err(LedgerError::currency_mismatch());
        }
    }
    let receivables: Decimal = command
        .receivables
        .iter()
        .map(|leg| leg.amount.amount())
        .sum();
    let payables: Decimal = command.payables.iter().map(|leg| leg.amount.amount()).sum();
    let cash: Decimal = command
        .cash_contributions
        .iter()
        .map(|leg| leg.amount.amount())
        .sum();
    if command.expense.amount() + receivables != cash + payables {
        return Err(LedgerError::unbalanced_journal());
    }
    let hash = digest(&json!({
        "source":command.metadata.source,"cash":command.cash_contributions.iter().map(|leg| (leg.account_id, &leg.amount)).collect::<Vec<_>>(),
        "expense":command.expense,"receivables":command.receivables.iter().map(|leg| (leg.account_id, &leg.amount)).collect::<Vec<_>>(),
        "payables":command.payables.iter().map(|leg| (leg.account_id, &leg.amount)).collect::<Vec<_>>(),
        "description":command.description,"occurred_at":command.metadata.occurred_at,
    }))?;
    let mut tx = uow.begin().await?;
    if let Some(mut result) = replay::<_, InternalAccountingResult>(
        &mut tx,
        command.metadata.user_id,
        "record_expense_and_control_balances",
        &command.metadata.idempotency_key,
        &hash,
    )
    .await?
    {
        result.replayed = true;
        tx.rollback().await?;
        return Ok(result);
    }
    let ids: Vec<_> = command
        .cash_contributions
        .iter()
        .map(|leg| leg.account_id)
        .chain(command.receivables.iter().map(|leg| leg.account_id))
        .chain(command.payables.iter().map(|leg| leg.account_id))
        .collect();
    if ids.iter().copied().collect::<BTreeSet<_>>().len() != ids.len() {
        return Err(LedgerError::invalid_state(
            "an account may appear only once in a controlled recipe",
        ));
    }
    let accounts = tx.lock_accounts(command.metadata.user_id, &ids).await?;
    if accounts.len() != ids.len() {
        return Err(LedgerError::not_found());
    }
    let by_id: BTreeMap<_, _> = accounts
        .iter()
        .map(|account| (account.id(), account))
        .collect();
    for leg in &command.cash_contributions {
        let account = by_id[&leg.account_id];
        account.require_posting_allowed(PostingPurpose::Ordinary)?;
        if account.currency() != &currency {
            return Err(LedgerError::currency_mismatch());
        }
    }
    for leg in &command.receivables {
        let account = by_id[&leg.account_id];
        if account.system_role() != Some(SystemAccountRole::ExternalReceivable) {
            return Err(LedgerError::invalid_account_kind());
        }
    }
    for leg in &command.payables {
        let account = by_id[&leg.account_id];
        if account.system_role() != Some(SystemAccountRole::ExternalPayable) {
            return Err(LedgerError::invalid_account_kind());
        }
    }
    let expense_account = match tx
        .find_system_account(
            command.metadata.user_id,
            &currency,
            SystemAccountRole::UncategorizedExpense,
            None,
        )
        .await?
    {
        Some(value) => value,
        None => {
            let value = LedgerAccount::open_system(
                LedgerAccountId::generate(),
                command.metadata.user_id,
                currency.clone(),
                SystemAccountRole::UncategorizedExpense,
                clock,
            );
            tx.insert_account(&value).await?;
            value
        }
    };
    let mut postings = Vec::new();
    for leg in &command.cash_contributions {
        postings.push(Posting::for_account(
            PostingId::generate(),
            by_id[&leg.account_id],
            -leg.amount.amount(),
            PostingPurpose::Ordinary,
        )?);
    }
    if !command.expense.is_zero() {
        postings.push(Posting::for_account(
            PostingId::generate(),
            &expense_account,
            command.expense.amount(),
            PostingPurpose::Ordinary,
        )?);
    }
    for leg in &command.receivables {
        postings.push(Posting::for_account(
            PostingId::generate(),
            by_id[&leg.account_id],
            leg.amount.amount(),
            PostingPurpose::Ordinary,
        )?);
    }
    for leg in &command.payables {
        postings.push(Posting::for_account(
            PostingId::generate(),
            by_id[&leg.account_id],
            -leg.amount.amount(),
            PostingPurpose::Ordinary,
        )?);
    }
    let journal = JournalEntry::post(
        JournalEntryId::generate(),
        command.metadata.user_id,
        &command.description,
        PostingPurpose::Ordinary,
        JournalSource::System,
        Actor::External {
            source_kind: command.metadata.source.source_kind().to_owned(),
            source_reference: command.metadata.source.item_id().to_owned(),
        },
        command.metadata.occurred_at,
        clock.now(),
        command.metadata.correlation_id,
        command.metadata.causation_id,
        command.metadata.idempotency_key.clone(),
        JournalRelations::none(),
        postings,
    )?;
    commit_journal(
        &mut tx,
        "record_expense_and_control_balances",
        &journal,
        None,
        "ledger.internal-accounting-command-posted.v1",
    )
    .await?;
    let mut effects = Vec::new();
    for account in accounts {
        let signed_amount: Decimal = journal
            .postings()
            .iter()
            .filter(|posting| posting.account_id() == account.id())
            .map(Posting::signed_amount)
            .sum();
        let (signed_balance, balance_version) = tx
            .signed_balance(command.metadata.user_id, account.id(), false)
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
    let projection_versions = effects
        .iter()
        .map(|effect| ProjectionVersion {
            account_id: effect.account_id,
            version: effect.balance_version,
        })
        .collect();
    let result = InternalAccountingResult {
        journal_entry_id: Some(journal.id()),
        effects,
        projection_versions,
        replayed: false,
        cancelled: false,
        outbox_correlation_id: command.metadata.correlation_id,
    };
    store(
        &mut tx,
        command.metadata.user_id,
        "record_expense_and_control_balances",
        &command.metadata.idempotency_key,
        &hash,
        &result,
        clock.now(),
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

async fn import_provider(
    facade: &LedgerFacade,
    command: ImportProviderTransaction,
) -> Result<InternalAccountingResult, LedgerError> {
    if command.state != ProviderTransactionState::Posted {
        return Ok(empty_result(command.metadata.correlation_id));
    }
    if command.amount.is_zero() {
        return Err(LedgerError::invalid_money(
            "provider transaction amount cannot be zero",
        ));
    }
    let sign = if command.amount.amount().is_sign_negative() {
        -1
    } else {
        1
    };
    let role = if sign > 0 {
        SystemAccountRole::UncategorizedIncome
    } else {
        SystemAccountRole::UncategorizedExpense
    };
    let amount = crate::shared_kernel::Money::new(
        command.amount.amount().abs(),
        command.amount.currency().clone(),
        8,
    )
    .map_err(|error| LedgerError::invalid_money(error.to_string()))?;
    post_with_system(
        &facade.uow,
        facade.clock.as_ref(),
        command.metadata,
        "import_provider_transaction",
        command.user_account_id,
        sign,
        role,
        amount,
        &command.description,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn post_by_control_nature<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    metadata: super::super::public::InternalCommandMetadata,
    scope: &str,
    cash_account_id: LedgerAccountId,
    control_account_id: LedgerAccountId,
    amount: crate::shared_kernel::Money,
    inverse: bool,
) -> Result<InternalAccountingResult, LedgerError> {
    let mut tx = uow.begin().await?;
    let control = tx
        .find_account(metadata.user_id, control_account_id, false)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    let asset = control.nature() == super::super::domain::AccountNature::Asset;
    tx.rollback().await?;
    let (mut cash_sign, mut control_sign) = if asset { (-1, 1) } else { (1, -1) };
    if inverse {
        cash_sign = -cash_sign;
        control_sign = -control_sign;
    }
    post_pair(
        uow,
        clock,
        metadata,
        scope,
        cash_account_id,
        cash_sign,
        control_account_id,
        control_sign,
        amount,
        scope,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn post_pair<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    metadata: super::super::public::InternalCommandMetadata,
    scope: &str,
    first_id: LedgerAccountId,
    first_sign: i32,
    second_id: LedgerAccountId,
    second_sign: i32,
    amount: crate::shared_kernel::Money,
    description: &str,
) -> Result<InternalAccountingResult, LedgerError> {
    if amount.is_zero() || amount.amount().is_sign_negative() || first_sign + second_sign != 0 {
        return Err(LedgerError::invalid_money(
            "controlled pair requires a positive balanced amount",
        ));
    }
    let hash = digest(
        &json!({"source":metadata.source,"first":first_id,"first_sign":first_sign,
        "second":second_id,"second_sign":second_sign,"amount":amount,"occurred_at":metadata.occurred_at}),
    )?;
    let mut tx = uow.begin().await?;
    if scope == "cash_control_operation"
        && let Some(receipt) = tx
            .find_receipt(metadata.user_id, scope, &metadata.idempotency_key, true)
            .await?
    {
        let mut result: InternalAccountingResult = serde_json::from_value(receipt.result)
            .map_err(|error| LedgerError::persistence(error.to_string()))?;
        if receipt.request_hash != hash && !result.cancelled {
            return Err(LedgerError::idempotency_conflict());
        }
        result.replayed = true;
        tx.rollback().await?;
        return Ok(result);
    }
    if let Some(mut result) = replay::<_, InternalAccountingResult>(
        &mut tx,
        metadata.user_id,
        scope,
        &metadata.idempotency_key,
        &hash,
    )
    .await?
    {
        result.replayed = true;
        tx.rollback().await?;
        return Ok(result);
    }
    let accounts = tx
        .lock_accounts(metadata.user_id, &[first_id, second_id])
        .await?;
    if accounts.len() != 2 {
        return Err(LedgerError::not_found());
    }
    let first = accounts
        .iter()
        .find(|account| account.id() == first_id)
        .ok_or_else(LedgerError::not_found)?;
    let second = accounts
        .iter()
        .find(|account| account.id() == second_id)
        .ok_or_else(LedgerError::not_found)?;
    if first.currency() != amount.currency() || second.currency() != amount.currency() {
        return Err(LedgerError::currency_mismatch());
    }
    let journal = JournalEntry::post(
        JournalEntryId::generate(),
        metadata.user_id,
        description,
        PostingPurpose::Ordinary,
        JournalSource::System,
        Actor::External {
            source_kind: metadata.source.source_kind().to_owned(),
            source_reference: metadata.source.item_id().to_owned(),
        },
        metadata.occurred_at,
        clock.now(),
        metadata.correlation_id,
        metadata.causation_id,
        metadata.idempotency_key.clone(),
        JournalRelations::none(),
        vec![
            Posting::for_account(
                PostingId::generate(),
                first,
                amount.amount() * Decimal::from(first_sign),
                PostingPurpose::Ordinary,
            )?,
            Posting::for_account(
                PostingId::generate(),
                second,
                amount.amount() * Decimal::from(second_sign),
                PostingPurpose::Ordinary,
            )?,
        ],
    )?;
    commit_journal(
        &mut tx,
        scope,
        &journal,
        None,
        "ledger.internal-accounting-command-posted.v1",
    )
    .await?;
    let result = accounting_result(
        &mut tx,
        metadata.user_id,
        metadata.correlation_id,
        &journal,
        &accounts,
    )
    .await?;
    store(
        &mut tx,
        metadata.user_id,
        scope,
        &metadata.idempotency_key,
        &hash,
        &result,
        clock.now(),
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
async fn post_with_system<U: LedgerUnitOfWork>(
    uow: &U,
    clock: &dyn Clock,
    metadata: super::super::public::InternalCommandMetadata,
    scope: &str,
    account_id: LedgerAccountId,
    account_sign: i32,
    role: SystemAccountRole,
    amount: crate::shared_kernel::Money,
    description: &str,
) -> Result<InternalAccountingResult, LedgerError> {
    if amount.is_zero() || amount.amount().is_sign_negative() {
        return Err(LedgerError::invalid_money(
            "controlled amount must be positive",
        ));
    }
    let hash = digest(
        &json!({"source":metadata.source,"account":account_id,"sign":account_sign,
        "role":role,"amount":amount,"description":description,"occurred_at":metadata.occurred_at}),
    )?;
    let mut tx = uow.begin().await?;
    if let Some(mut result) = replay::<_, InternalAccountingResult>(
        &mut tx,
        metadata.user_id,
        scope,
        &metadata.idempotency_key,
        &hash,
    )
    .await?
    {
        result.replayed = true;
        tx.rollback().await?;
        return Ok(result);
    }
    let account = tx
        .find_account(metadata.user_id, account_id, true)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    if account.currency() != amount.currency() {
        return Err(LedgerError::currency_mismatch());
    }
    let system = match tx
        .find_system_account(metadata.user_id, amount.currency(), role, None)
        .await?
    {
        Some(value) => value,
        None => {
            let value = LedgerAccount::open_system(
                LedgerAccountId::generate(),
                metadata.user_id,
                amount.currency().clone(),
                role,
                clock,
            );
            tx.insert_account(&value).await?;
            value
        }
    };
    let journal = JournalEntry::post(
        JournalEntryId::generate(),
        metadata.user_id,
        description,
        PostingPurpose::Ordinary,
        JournalSource::System,
        Actor::External {
            source_kind: metadata.source.source_kind().to_owned(),
            source_reference: metadata.source.item_id().to_owned(),
        },
        metadata.occurred_at,
        clock.now(),
        metadata.correlation_id,
        metadata.causation_id,
        metadata.idempotency_key.clone(),
        JournalRelations::none(),
        vec![
            Posting::for_account(
                PostingId::generate(),
                &account,
                amount.amount() * Decimal::from(account_sign),
                PostingPurpose::Ordinary,
            )?,
            Posting::for_account(
                PostingId::generate(),
                &system,
                amount.amount() * Decimal::from(-account_sign),
                PostingPurpose::Ordinary,
            )?,
        ],
    )?;
    commit_journal(
        &mut tx,
        scope,
        &journal,
        None,
        "ledger.internal-accounting-command-posted.v1",
    )
    .await?;
    let accounts = vec![account, system];
    let result = accounting_result(
        &mut tx,
        metadata.user_id,
        metadata.correlation_id,
        &journal,
        &accounts,
    )
    .await?;
    store(
        &mut tx,
        metadata.user_id,
        scope,
        &metadata.idempotency_key,
        &hash,
        &result,
        clock.now(),
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

async fn accounting_result<T: ProjectionStore>(
    tx: &mut T,
    user_id: crate::shared_kernel::UserId,
    correlation_id: crate::shared_kernel::CorrelationId,
    journal: &JournalEntry,
    accounts: &[LedgerAccount],
) -> Result<InternalAccountingResult, LedgerError> {
    let mut effects = Vec::new();
    for account in accounts {
        let signed_amount: Decimal = journal
            .postings()
            .iter()
            .filter(|posting| posting.account_id() == account.id())
            .map(Posting::signed_amount)
            .sum();
        let (signed_balance, balance_version) = tx
            .signed_balance(user_id, account.id(), false)
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
    let projection_versions = effects
        .iter()
        .map(|effect| ProjectionVersion {
            account_id: effect.account_id,
            version: effect.balance_version,
        })
        .collect();
    Ok(InternalAccountingResult {
        journal_entry_id: Some(journal.id()),
        effects,
        projection_versions,
        replayed: false,
        cancelled: false,
        outbox_correlation_id: correlation_id,
    })
}

fn empty_result(correlation_id: crate::shared_kernel::CorrelationId) -> InternalAccountingResult {
    InternalAccountingResult {
        journal_entry_id: None,
        effects: Vec::new(),
        projection_versions: Vec::new(),
        replayed: false,
        cancelled: false,
        outbox_correlation_id: correlation_id,
    }
}

fn system_role(role: ControlAccountRole) -> SystemAccountRole {
    match role {
        ControlAccountRole::ExternalReceivable => SystemAccountRole::ExternalReceivable,
        ControlAccountRole::ExternalPayable => SystemAccountRole::ExternalPayable,
        ControlAccountRole::InterestReceivable => SystemAccountRole::InterestReceivable,
        ControlAccountRole::InterestPayable => SystemAccountRole::InterestPayable,
        ControlAccountRole::FeeReceivable => SystemAccountRole::FeeReceivable,
        ControlAccountRole::FeePayable => SystemAccountRole::FeePayable,
        ControlAccountRole::PortfolioCashClearing => SystemAccountRole::PortfolioCashClearing,
    }
}

fn validate_subject(value: &str) -> Result<(), LedgerError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 300
        || value.chars().any(char::is_control)
    {
        return Err(LedgerError::invalid_source_reference());
    }
    Ok(())
}

async fn replay<T: CommandReceiptStore, R: serde::de::DeserializeOwned>(
    tx: &mut T,
    user_id: crate::shared_kernel::UserId,
    scope: &str,
    key: &crate::shared_kernel::IdempotencyKey,
    hash: &[u8; 32],
) -> Result<Option<R>, LedgerError> {
    let Some(receipt) = tx.find_receipt(user_id, scope, key, true).await? else {
        return Ok(None);
    };
    if &receipt.request_hash != hash {
        return Err(LedgerError::idempotency_conflict());
    }
    serde_json::from_value(receipt.result)
        .map(Some)
        .map_err(|error| LedgerError::persistence(error.to_string()))
}

async fn store<T: CommandReceiptStore, R: serde::Serialize>(
    tx: &mut T,
    user_id: crate::shared_kernel::UserId,
    scope: &str,
    key: &crate::shared_kernel::IdempotencyKey,
    hash: &[u8; 32],
    result: &R,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), LedgerError> {
    let value = serde_json::to_value(result)
        .map_err(|error| LedgerError::persistence(error.to_string()))?;
    tx.insert_receipt(user_id, scope, key, hash, &value, now)
        .await
}

fn digest(value: &serde_json::Value) -> Result<[u8; 32], LedgerError> {
    Ok(Sha256::digest(
        serde_json::to_vec(value).map_err(|error| LedgerError::persistence(error.to_string()))?,
    )
    .into())
}
