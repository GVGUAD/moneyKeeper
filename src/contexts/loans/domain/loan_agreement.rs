//! LoanAgreement aggregate and lifecycle invariants.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::contexts::ledger::public::{JournalEntryId, LedgerAccountId};
use crate::shared_kernel::{CurrencyCode, UserId};

use super::{
    ComponentBalances, LoanError, LoanMovement, LoanMovementId, LoanTerms, MovementKind,
    MovementStatus, TermRevision, TermRevisionId,
};

crate::define_uuid_id!(#[doc = "Identifies a LoanAgreement aggregate."] pub LoanAgreementId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoanDirection {
    Borrowed,
    Lent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoanStatus {
    Draft,
    PendingAccounting,
    Active,
    Failed,
    Closed,
}

/// Facts recorded by the aggregate; the application translates these into
/// versioned integration events after persistence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LoanDomainEvent {
    AgreementOpened {
        agreement_id: LoanAgreementId,
    },
    PrincipalAccountLinked {
        agreement_id: LoanAgreementId,
        account_id: LedgerAccountId,
    },
    OpeningFailed {
        agreement_id: LoanAgreementId,
    },
    TermsRevised {
        agreement_id: LoanAgreementId,
        revision: u64,
    },
    MovementRequested {
        agreement_id: LoanAgreementId,
        movement_id: LoanMovementId,
    },
    MovementPosted {
        agreement_id: LoanAgreementId,
        movement_id: LoanMovementId,
        balances: ComponentBalances,
    },
    MovementFailed {
        agreement_id: LoanAgreementId,
        movement_id: LoanMovementId,
    },
    MovementReversed {
        agreement_id: LoanAgreementId,
        movement_id: LoanMovementId,
        balances: ComponentBalances,
    },
    AgreementClosed {
        agreement_id: LoanAgreementId,
    },
}

/// Contract and confirmed loan state. Monetary intent is represented by
/// separate LoanMovement aggregates and coordinated through events.
#[derive(Clone, Debug)]
pub struct LoanAgreement {
    id: LoanAgreementId,
    user_id: UserId,
    direction: LoanDirection,
    currency: CurrencyCode,
    status: LoanStatus,
    terms: Vec<TermRevision>,
    ledger_principal_account_id: Option<LedgerAccountId>,
    balances: ComponentBalances,
    cumulative_posted_disbursement: Decimal,
    pending_movements: u32,
    version: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    events: Vec<LoanDomainEvent>,
}

impl LoanAgreement {
    pub fn open(
        user_id: UserId,
        direction: LoanDirection,
        terms: LoanTerms,
        now: DateTime<Utc>,
    ) -> Result<Self, LoanError> {
        let currency = terms.contractual_principal().currency().clone();
        let id = LoanAgreementId::generate();
        let revision = TermRevision {
            id: TermRevisionId::generate(),
            revision: 1,
            terms,
            reason: "Agreement opened".to_owned(),
            recorded_at: now,
        };
        Ok(Self {
            id,
            user_id,
            direction,
            currency: currency.clone(),
            status: LoanStatus::PendingAccounting,
            terms: vec![revision],
            ledger_principal_account_id: None,
            balances: ComponentBalances::zero(currency),
            cumulative_posted_disbursement: Decimal::ZERO,
            pending_movements: 0,
            version: 1,
            created_at: now,
            updated_at: now,
            events: vec![LoanDomainEvent::AgreementOpened { agreement_id: id }],
        })
    }

    pub fn link_principal_account(
        &mut self,
        account_id: LedgerAccountId,
        expected_version: u64,
        now: DateTime<Utc>,
    ) -> Result<(), LoanError> {
        self.require_version(expected_version)?;
        if self.status != LoanStatus::PendingAccounting
            || self.ledger_principal_account_id.is_some()
        {
            return Err(LoanError::InvalidState);
        }
        self.ledger_principal_account_id = Some(account_id);
        self.status = LoanStatus::Active;
        self.bump(now)?;
        self.events.push(LoanDomainEvent::PrincipalAccountLinked {
            agreement_id: self.id,
            account_id,
        });
        Ok(())
    }

    pub fn fail_opening(
        &mut self,
        expected_version: u64,
        now: DateTime<Utc>,
    ) -> Result<(), LoanError> {
        self.require_version(expected_version)?;
        if self.status != LoanStatus::PendingAccounting {
            return Err(LoanError::InvalidState);
        }
        self.status = LoanStatus::Failed;
        self.bump(now)?;
        self.events.push(LoanDomainEvent::OpeningFailed {
            agreement_id: self.id,
        });
        Ok(())
    }

    pub fn revise_terms(
        &mut self,
        terms: LoanTerms,
        reason: impl Into<String>,
        expected_version: u64,
        now: DateTime<Utc>,
    ) -> Result<TermRevisionId, LoanError> {
        self.require_version(expected_version)?;
        if !matches!(
            self.status,
            LoanStatus::Active | LoanStatus::PendingAccounting
        ) {
            return Err(LoanError::InvalidState);
        }
        if terms.contractual_principal().currency() != &self.currency {
            return Err(LoanError::CurrencyMismatch);
        }
        if terms.contractual_principal().amount() < self.cumulative_posted_disbursement {
            return Err(LoanError::ContractualPrincipalExceeded);
        }
        let reason = reason.into();
        if reason.is_empty() || reason.trim() != reason {
            return Err(LoanError::InvalidValue("revision_reason"));
        }
        let revision_number = u64::try_from(self.terms.len())
            .map_err(|_| LoanError::Arithmetic)?
            .checked_add(1)
            .ok_or(LoanError::Arithmetic)?;
        let id = TermRevisionId::generate();
        self.terms.push(TermRevision {
            id,
            revision: revision_number,
            terms,
            reason,
            recorded_at: now,
        });
        self.bump(now)?;
        self.events.push(LoanDomainEvent::TermsRevised {
            agreement_id: self.id,
            revision: revision_number,
        });
        Ok(id)
    }

    pub fn request_movement(
        &mut self,
        movement: &LoanMovement,
        expected_version: u64,
        now: DateTime<Utc>,
    ) -> Result<(), LoanError> {
        self.require_version(expected_version)?;
        if self.status != LoanStatus::Active || movement.user_id() != self.user_id {
            return Err(LoanError::InvalidState);
        }
        if movement.money().currency() != &self.currency {
            return Err(LoanError::CurrencyMismatch);
        }
        if movement.status() != MovementStatus::PendingAccounting {
            return Err(LoanError::InvalidState);
        }
        let proposed = self
            .balances
            .apply(movement.kind(), movement.components(), false)?;
        if movement.kind() == MovementKind::Disbursement {
            let total = self
                .cumulative_posted_disbursement
                .checked_add(movement.components().principal)
                .ok_or(LoanError::Arithmetic)?;
            if total > self.current_terms().contractual_principal().amount() {
                return Err(LoanError::ContractualPrincipalExceeded);
            }
        }
        // Keep validation result live so reductions are checked before intent is committed.
        let _ = proposed;
        self.pending_movements = self
            .pending_movements
            .checked_add(1)
            .ok_or(LoanError::Arithmetic)?;
        self.bump(now)?;
        self.events.push(LoanDomainEvent::MovementRequested {
            agreement_id: self.id,
            movement_id: movement.id(),
        });
        Ok(())
    }

    pub fn confirm_posted(
        &mut self,
        movement: &mut LoanMovement,
        journal_id: JournalEntryId,
        expected_version: u64,
        now: DateTime<Utc>,
    ) -> Result<(), LoanError> {
        self.require_version(expected_version)?;
        movement.mark_posted(journal_id)?;
        self.balances = self
            .balances
            .apply(movement.kind(), movement.components(), false)?;
        if movement.kind() == MovementKind::Disbursement {
            self.cumulative_posted_disbursement = self
                .cumulative_posted_disbursement
                .checked_add(movement.components().principal)
                .ok_or(LoanError::Arithmetic)?;
        }
        self.pending_movements = self
            .pending_movements
            .checked_sub(1)
            .ok_or(LoanError::InvalidState)?;
        self.bump(now)?;
        self.events.push(LoanDomainEvent::MovementPosted {
            agreement_id: self.id,
            movement_id: movement.id(),
            balances: self.balances.clone(),
        });
        Ok(())
    }

    pub fn confirm_failed(
        &mut self,
        movement: &mut LoanMovement,
        expected_version: u64,
        now: DateTime<Utc>,
    ) -> Result<(), LoanError> {
        self.require_version(expected_version)?;
        movement.mark_failed()?;
        self.pending_movements = self
            .pending_movements
            .checked_sub(1)
            .ok_or(LoanError::InvalidState)?;
        self.bump(now)?;
        self.events.push(LoanDomainEvent::MovementFailed {
            agreement_id: self.id,
            movement_id: movement.id(),
        });
        Ok(())
    }

    pub fn confirm_reversed(
        &mut self,
        movement: &mut LoanMovement,
        reversal_id: JournalEntryId,
        expected_version: u64,
        now: DateTime<Utc>,
    ) -> Result<(), LoanError> {
        self.require_version(expected_version)?;
        movement.mark_reversed(reversal_id)?;
        self.balances = self
            .balances
            .apply(movement.kind(), movement.components(), true)?;
        if movement.kind() == MovementKind::Disbursement {
            self.cumulative_posted_disbursement = self
                .cumulative_posted_disbursement
                .checked_sub(movement.components().principal)
                .ok_or(LoanError::Arithmetic)?;
        }
        self.bump(now)?;
        self.events.push(LoanDomainEvent::MovementReversed {
            agreement_id: self.id,
            movement_id: movement.id(),
            balances: self.balances.clone(),
        });
        Ok(())
    }

    pub fn close(&mut self, expected_version: u64, now: DateTime<Utc>) -> Result<(), LoanError> {
        self.require_version(expected_version)?;
        if self.status != LoanStatus::Active {
            return Err(LoanError::InvalidState);
        }
        if self.pending_movements != 0 {
            return Err(LoanError::AccountingPending);
        }
        if !self.balances.is_zero() {
            return Err(LoanError::OutstandingBalance);
        }
        self.status = LoanStatus::Closed;
        self.bump(now)?;
        self.events.push(LoanDomainEvent::AgreementClosed {
            agreement_id: self.id,
        });
        Ok(())
    }

    fn require_version(&self, expected: u64) -> Result<(), LoanError> {
        if expected == self.version {
            Ok(())
        } else {
            Err(LoanError::VersionConflict)
        }
    }
    fn bump(&mut self, now: DateTime<Utc>) -> Result<(), LoanError> {
        self.version = self.version.checked_add(1).ok_or(LoanError::Arithmetic)?;
        self.updated_at = now;
        Ok(())
    }
    pub fn pull_events(&mut self) -> Vec<LoanDomainEvent> {
        std::mem::take(&mut self.events)
    }
    pub const fn id(&self) -> LoanAgreementId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub const fn direction(&self) -> LoanDirection {
        self.direction
    }
    pub const fn currency(&self) -> &CurrencyCode {
        &self.currency
    }
    pub const fn status(&self) -> LoanStatus {
        self.status
    }
    pub fn current_terms(&self) -> &LoanTerms {
        &self.terms.last().expect("agreement always has terms").terms
    }
    pub fn term_revisions(&self) -> &[TermRevision] {
        &self.terms
    }
    pub const fn ledger_principal_account_id(&self) -> Option<LedgerAccountId> {
        self.ledger_principal_account_id
    }
    pub const fn balances(&self) -> &ComponentBalances {
        &self.balances
    }
    pub const fn pending_movements(&self) -> u32 {
        self.pending_movements
    }
    pub const fn version(&self) -> u64 {
        self.version
    }
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub const fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
}
