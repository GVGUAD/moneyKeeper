//! Immutable monetary loan intent and confirmed component effects.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::contexts::ledger::public::{JournalEntryId, LedgerAccountId};
use crate::shared_kernel::{CorrelationId, CurrencyCode, Money, UserId};

use super::LoanError;

crate::define_uuid_id!(#[doc = "Identifies one immutable loan movement."] pub LoanMovementId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MovementKind {
    Disbursement,
    Repayment,
    Accrual,
    WriteOff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MovementStatus {
    ReplacementRequested,
    PendingAccounting,
    Posted,
    Failed,
    ReversalPending,
    Reversed,
}

/// Explicit amounts carried by a movement. Applied accrual and current-period
/// amounts remain separate so income or expense is never recognized twice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MovementComponents {
    pub principal: Decimal,
    pub accrued_interest: Decimal,
    pub accrued_fee: Decimal,
    pub current_interest: Decimal,
    pub current_fee: Decimal,
}

impl MovementComponents {
    pub const fn zero() -> Self {
        Self {
            principal: Decimal::ZERO,
            accrued_interest: Decimal::ZERO,
            accrued_fee: Decimal::ZERO,
            current_interest: Decimal::ZERO,
            current_fee: Decimal::ZERO,
        }
    }

    pub fn validate(&self) -> Result<(), LoanError> {
        let values = [
            self.principal,
            self.accrued_interest,
            self.accrued_fee,
            self.current_interest,
            self.current_fee,
        ];
        if values.iter().any(Decimal::is_sign_negative) || values.iter().all(Decimal::is_zero) {
            return Err(LoanError::InvalidValue("movement_components"));
        }
        Ok(())
    }

    pub fn total(&self) -> Result<Decimal, LoanError> {
        [
            self.principal,
            self.accrued_interest,
            self.accrued_fee,
            self.current_interest,
            self.current_fee,
        ]
        .into_iter()
        .try_fold(Decimal::ZERO, |sum, value| {
            sum.checked_add(value).ok_or(LoanError::Arithmetic)
        })
    }
}

/// Confirmed outstanding amounts owned by Loans.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ComponentBalances {
    pub currency: CurrencyCode,
    pub principal: Decimal,
    pub accrued_interest: Decimal,
    pub accrued_fee: Decimal,
    pub version: u64,
}

impl ComponentBalances {
    pub fn zero(currency: CurrencyCode) -> Self {
        Self {
            currency,
            principal: Decimal::ZERO,
            accrued_interest: Decimal::ZERO,
            accrued_fee: Decimal::ZERO,
            version: 1,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.principal.is_zero() && self.accrued_interest.is_zero() && self.accrued_fee.is_zero()
    }

    pub fn apply(
        &self,
        kind: MovementKind,
        components: &MovementComponents,
        reverse: bool,
    ) -> Result<Self, LoanError> {
        let direction = if reverse { -Decimal::ONE } else { Decimal::ONE };
        let base_sign = match kind {
            MovementKind::Disbursement | MovementKind::Accrual => direction,
            MovementKind::Repayment | MovementKind::WriteOff => -direction,
        };
        let mut next = self.clone();
        next.principal = checked_effect(next.principal, components.principal, base_sign)?;
        next.accrued_interest = checked_effect(
            next.accrued_interest,
            components.accrued_interest,
            base_sign,
        )?;
        next.accrued_fee = checked_effect(next.accrued_fee, components.accrued_fee, base_sign)?;
        if next.principal.is_sign_negative()
            || next.accrued_interest.is_sign_negative()
            || next.accrued_fee.is_sign_negative()
        {
            return Err(LoanError::InsufficientOutstanding);
        }
        next.version = next.version.checked_add(1).ok_or(LoanError::Arithmetic)?;
        Ok(next)
    }
}

fn checked_effect(current: Decimal, amount: Decimal, sign: Decimal) -> Result<Decimal, LoanError> {
    current
        .checked_add(amount.checked_mul(sign).ok_or(LoanError::Arithmetic)?)
        .ok_or(LoanError::Arithmetic)
}

/// One immutable monetary intent and its durable accounting state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LoanMovement {
    id: LoanMovementId,
    user_id: UserId,
    kind: MovementKind,
    money: Money,
    components: MovementComponents,
    cash_account_id: Option<LedgerAccountId>,
    reason: Option<String>,
    status: MovementStatus,
    correlation_id: CorrelationId,
    ledger_journal_id: Option<JournalEntryId>,
    ledger_reversal_id: Option<JournalEntryId>,
    replaces: Option<LoanMovementId>,
    requested_at: DateTime<Utc>,
}

impl LoanMovement {
    #[allow(clippy::too_many_arguments)]
    pub fn request(
        user_id: UserId,
        kind: MovementKind,
        money: Money,
        components: MovementComponents,
        cash_account_id: Option<LedgerAccountId>,
        reason: Option<String>,
        correlation_id: CorrelationId,
        replaces: Option<LoanMovementId>,
        now: DateTime<Utc>,
    ) -> Result<Self, LoanError> {
        components.validate()?;
        if money.amount() != components.total()? {
            return Err(LoanError::InvalidValue("movement_total"));
        }
        if matches!(kind, MovementKind::Disbursement | MovementKind::Repayment)
            != cash_account_id.is_some()
        {
            return Err(LoanError::InvalidValue("cash_account_id"));
        }
        if kind == MovementKind::Disbursement
            && (components.principal.is_zero()
                || !components.accrued_interest.is_zero()
                || !components.accrued_fee.is_zero()
                || !components.current_interest.is_zero()
                || !components.current_fee.is_zero())
        {
            return Err(LoanError::InvalidValue("disbursement_components"));
        }
        if kind == MovementKind::Accrual
            && (!components.principal.is_zero()
                || !components.current_interest.is_zero()
                || !components.current_fee.is_zero())
        {
            return Err(LoanError::InvalidValue("accrual_components"));
        }
        if kind == MovementKind::WriteOff {
            if !components.current_interest.is_zero() || !components.current_fee.is_zero() {
                return Err(LoanError::InvalidValue("write_off_components"));
            }
            if reason
                .as_deref()
                .is_none_or(|value| value.is_empty() || value.trim() != value)
            {
                return Err(LoanError::InvalidValue("write_off_reason"));
            }
        }
        Ok(Self {
            id: LoanMovementId::generate(),
            user_id,
            kind,
            money,
            components,
            cash_account_id,
            reason,
            status: MovementStatus::PendingAccounting,
            correlation_id,
            ledger_journal_id: None,
            ledger_reversal_id: None,
            replaces,
            requested_at: now,
        })
    }

    pub fn mark_posted(&mut self, journal_id: JournalEntryId) -> Result<(), LoanError> {
        if self.status != MovementStatus::PendingAccounting {
            return Err(LoanError::InvalidState);
        }
        self.status = MovementStatus::Posted;
        self.ledger_journal_id = Some(journal_id);
        Ok(())
    }
    pub fn mark_failed(&mut self) -> Result<(), LoanError> {
        if self.status != MovementStatus::PendingAccounting {
            return Err(LoanError::InvalidState);
        }
        self.status = MovementStatus::Failed;
        Ok(())
    }
    pub fn mark_reversed(&mut self, reversal_id: JournalEntryId) -> Result<(), LoanError> {
        match self.status {
            MovementStatus::Reversed => Err(LoanError::AlreadyReversed),
            MovementStatus::Posted => {
                self.status = MovementStatus::Reversed;
                self.ledger_reversal_id = Some(reversal_id);
                Ok(())
            }
            _ => Err(LoanError::InvalidState),
        }
    }
    pub fn ledger_idempotency_key(&self) -> String {
        format!("loan-accounting:{}", self.id)
    }
    pub fn reversal_idempotency_key(&self) -> String {
        format!("loan-reversal:{}", self.id)
    }
    pub const fn id(&self) -> LoanMovementId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub const fn kind(&self) -> MovementKind {
        self.kind
    }
    pub const fn money(&self) -> &Money {
        &self.money
    }
    pub const fn components(&self) -> &MovementComponents {
        &self.components
    }
    pub const fn cash_account_id(&self) -> Option<LedgerAccountId> {
        self.cash_account_id
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
    pub const fn status(&self) -> MovementStatus {
        self.status
    }
    pub const fn correlation_id(&self) -> CorrelationId {
        self.correlation_id
    }
    pub const fn ledger_journal_id(&self) -> Option<JournalEntryId> {
        self.ledger_journal_id
    }
    pub const fn ledger_reversal_id(&self) -> Option<JournalEntryId> {
        self.ledger_reversal_id
    }
    pub const fn replaces(&self) -> Option<LoanMovementId> {
        self.replaces
    }
    pub const fn requested_at(&self) -> DateTime<Utc> {
        self.requested_at
    }
}
