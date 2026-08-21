//! Ledger account aggregate and closed account policy.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::shared_kernel::{Clock, CurrencyCode, UserId};

use super::{LedgerAccountId, LedgerError};

/// Accounting nature controlling display-balance normalization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountNature {
    /// Debit-normal resources controlled by the user.
    Asset,
    /// Credit-normal obligations owed by the user.
    Liability,
    /// Credit-normal residual/system balancing accounts.
    Equity,
    /// Credit-normal inflows.
    Income,
    /// Debit-normal outflows.
    Expense,
}

impl AccountNature {
    /// Returns the multiplier from raw debit-positive balance to display balance.
    pub const fn normal_sign(self) -> i8 {
        match self {
            Self::Asset | Self::Expense => 1,
            Self::Liability | Self::Equity | Self::Income => -1,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Asset => "asset",
            Self::Liability => "liability",
            Self::Equity => "equity",
            Self::Income => "income",
            Self::Expense => "expense",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "asset" => Ok(Self::Asset),
            "liability" => Ok(Self::Liability),
            "equity" => Ok(Self::Equity),
            "income" => Ok(Self::Income),
            "expense" => Ok(Self::Expense),
            _ => Err(LedgerError::persistence("stored account nature is invalid")),
        }
    }
}

/// Closed account-kind vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    Cash,
    DebitCard,
    CreditCard,
    Current,
    Savings,
    Jar,
    LoanPayable,
    LoanReceivable,
    /// Hidden system account; its precise role is held separately.
    System,
}

impl AccountKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Cash => "cash",
            Self::DebitCard => "debit_card",
            Self::CreditCard => "credit_card",
            Self::Current => "current",
            Self::Savings => "savings",
            Self::Jar => "jar",
            Self::LoanPayable => "loan_payable",
            Self::LoanReceivable => "loan_receivable",
            Self::System => "system",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "cash" => Ok(Self::Cash),
            "debit_card" => Ok(Self::DebitCard),
            "credit_card" => Ok(Self::CreditCard),
            "current" => Ok(Self::Current),
            "savings" => Ok(Self::Savings),
            "jar" => Ok(Self::Jar),
            "loan_payable" => Ok(Self::LoanPayable),
            "loan_receivable" => Ok(Self::LoanReceivable),
            "system" => Ok(Self::System),
            _ => Err(LedgerError::persistence("stored account kind is invalid")),
        }
    }
}

/// Authority allowed to originate account metadata and observations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountAuthority {
    Manual,
    ProviderObserved,
    System,
}

impl AccountAuthority {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::ProviderObserved => "provider_observed",
            Self::System => "system",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "manual" => Ok(Self::Manual),
            "provider_observed" => Ok(Self::ProviderObserved),
            "system" => Ok(Self::System),
            _ => Err(LedgerError::persistence(
                "stored account authority is invalid",
            )),
        }
    }
}

/// Whether an account participates in normal user-facing lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountVisibility {
    UserVisible,
    Hidden,
}

impl AccountVisibility {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::UserVisible => "user_visible",
            Self::Hidden => "hidden",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "user_visible" => Ok(Self::UserVisible),
            "hidden" => Ok(Self::Hidden),
            _ => Err(LedgerError::persistence(
                "stored account visibility is invalid",
            )),
        }
    }
}

/// Account lifecycle. Archive preserves every accounting fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountLifecycle {
    Active,
    Archived,
}

impl AccountLifecycle {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "active" => Ok(Self::Active),
            "archived" => Ok(Self::Archived),
            _ => Err(LedgerError::persistence(
                "stored account lifecycle is invalid",
            )),
        }
    }
}

/// Optimistic account metadata version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountVersion(i64);

impl AccountVersion {
    /// Version assigned to a newly opened account.
    pub const INITIAL: Self = Self(1);

    /// Constructs a positive version.
    pub fn new(value: i64) -> Result<Self, LedgerError> {
        if value < 1 {
            return Err(LedgerError::invalid_version());
        }
        Ok(Self(value))
    }

    /// Returns the persisted integer representation.
    pub const fn get(self) -> i64 {
        self.0
    }

    fn next(self) -> Result<Self, LedgerError> {
        self.0
            .checked_add(1)
            .ok_or_else(|| LedgerError::persistence("account version overflowed"))
            .and_then(Self::new)
    }
}

/// Financial purpose used to enforce archive policy at the aggregate boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostingPurpose {
    Ordinary,
    Correction,
    Reversal,
    ApprovedReconciliation,
}

impl PostingPurpose {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Ordinary => "ordinary",
            Self::Correction => "correction",
            Self::Reversal => "reversal",
            Self::ApprovedReconciliation => "approved_reconciliation",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "ordinary" => Ok(Self::Ordinary),
            "correction" => Ok(Self::Correction),
            "reversal" => Ok(Self::Reversal),
            "approved_reconciliation" => Ok(Self::ApprovedReconciliation),
            _ => Err(LedgerError::persistence(
                "stored posting purpose is invalid",
            )),
        }
    }
}

/// Closed roles for system-controlled accounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemAccountRole {
    UncategorizedIncome,
    UncategorizedExpense,
    OpeningBalanceEquity,
    BalanceAdjustmentEquity,
    FxClearing,
    ExternalReceivable,
    ExternalPayable,
    InterestReceivable,
    InterestPayable,
    FeeReceivable,
    FeePayable,
    PortfolioCashClearing,
    BadDebtExpense,
    DebtForgivenessIncome,
}

impl SystemAccountRole {
    pub(crate) const fn nature(self) -> AccountNature {
        match self {
            Self::UncategorizedIncome => AccountNature::Income,
            Self::UncategorizedExpense => AccountNature::Expense,
            Self::OpeningBalanceEquity
            | Self::BalanceAdjustmentEquity
            | Self::FxClearing
            | Self::PortfolioCashClearing => AccountNature::Equity,
            Self::ExternalReceivable | Self::InterestReceivable | Self::FeeReceivable => {
                AccountNature::Asset
            }
            Self::ExternalPayable | Self::InterestPayable | Self::FeePayable => {
                AccountNature::Liability
            }
            Self::BadDebtExpense => AccountNature::Expense,
            Self::DebtForgivenessIncome => AccountNature::Income,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::UncategorizedIncome => "uncategorized_income",
            Self::UncategorizedExpense => "uncategorized_expense",
            Self::OpeningBalanceEquity => "opening_balance_equity",
            Self::BalanceAdjustmentEquity => "balance_adjustment_equity",
            Self::FxClearing => "fx_clearing",
            Self::ExternalReceivable => "external_receivable",
            Self::ExternalPayable => "external_payable",
            Self::InterestReceivable => "interest_receivable",
            Self::InterestPayable => "interest_payable",
            Self::FeeReceivable => "fee_receivable",
            Self::FeePayable => "fee_payable",
            Self::PortfolioCashClearing => "portfolio_cash_clearing",
            Self::BadDebtExpense => "bad_debt_expense",
            Self::DebtForgivenessIncome => "debt_forgiveness_income",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "uncategorized_income" => Ok(Self::UncategorizedIncome),
            "uncategorized_expense" => Ok(Self::UncategorizedExpense),
            "opening_balance_equity" => Ok(Self::OpeningBalanceEquity),
            "balance_adjustment_equity" => Ok(Self::BalanceAdjustmentEquity),
            "fx_clearing" => Ok(Self::FxClearing),
            "external_receivable" => Ok(Self::ExternalReceivable),
            "external_payable" => Ok(Self::ExternalPayable),
            "interest_receivable" => Ok(Self::InterestReceivable),
            "interest_payable" => Ok(Self::InterestPayable),
            "fee_receivable" => Ok(Self::FeeReceivable),
            "fee_payable" => Ok(Self::FeePayable),
            "portfolio_cash_clearing" => Ok(Self::PortfolioCashClearing),
            "bad_debt_expense" => Ok(Self::BadDebtExpense),
            "debt_forgiveness_income" => Ok(Self::DebtForgivenessIncome),
            _ => Err(LedgerError::persistence(
                "stored system account role is invalid",
            )),
        }
    }
}

/// Account aggregate. Balance is deliberately absent; it is a posting projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerAccount {
    id: LedgerAccountId,
    user_id: UserId,
    name: String,
    currency: CurrencyCode,
    nature: AccountNature,
    kind: AccountKind,
    authority: AccountAuthority,
    visibility: AccountVisibility,
    lifecycle: AccountLifecycle,
    system_role: Option<SystemAccountRole>,
    version: AccountVersion,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl LedgerAccount {
    /// Opens a user-managed, user-visible account in a valid kind/nature combination.
    #[allow(clippy::too_many_arguments)]
    pub fn open_manual(
        id: LedgerAccountId,
        user_id: UserId,
        name: impl Into<String>,
        currency: CurrencyCode,
        kind: AccountKind,
        nature: AccountNature,
        clock: &(impl Clock + ?Sized),
    ) -> Result<Self, LedgerError> {
        Self::open_user(
            id,
            user_id,
            name,
            currency,
            kind,
            nature,
            AccountAuthority::Manual,
            clock,
        )
    }

    /// Opens a provider-observed, user-visible account after an external
    /// resource has been validated by the owning integration context.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_provider_observed(
        id: LedgerAccountId,
        user_id: UserId,
        name: impl Into<String>,
        currency: CurrencyCode,
        kind: AccountKind,
        nature: AccountNature,
        clock: &(impl Clock + ?Sized),
    ) -> Result<Self, LedgerError> {
        Self::open_user(
            id,
            user_id,
            name,
            currency,
            kind,
            nature,
            AccountAuthority::ProviderObserved,
            clock,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn open_user(
        id: LedgerAccountId,
        user_id: UserId,
        name: impl Into<String>,
        currency: CurrencyCode,
        kind: AccountKind,
        nature: AccountNature,
        authority: AccountAuthority,
        clock: &(impl Clock + ?Sized),
    ) -> Result<Self, LedgerError> {
        if !valid_user_kind(kind, nature) || authority == AccountAuthority::System {
            return Err(LedgerError::invalid_account_kind());
        }
        let name = validate_name(name.into())?;
        let now = clock.now();
        Ok(Self {
            id,
            user_id,
            name,
            currency,
            nature,
            kind,
            authority,
            visibility: AccountVisibility::UserVisible,
            lifecycle: AccountLifecycle::Active,
            system_role: None,
            version: AccountVersion::INITIAL,
            created_at: now,
            updated_at: now,
        })
    }

    pub(crate) fn open_system(
        id: LedgerAccountId,
        user_id: UserId,
        currency: CurrencyCode,
        role: SystemAccountRole,
        clock: &(impl Clock + ?Sized),
    ) -> Self {
        let now = clock.now();
        Self {
            id,
            user_id,
            name: role.as_str().replace('_', " "),
            currency,
            nature: role.nature(),
            kind: AccountKind::System,
            authority: AccountAuthority::System,
            visibility: AccountVisibility::Hidden,
            lifecycle: AccountLifecycle::Active,
            system_role: Some(role),
            version: AccountVersion::INITIAL,
            created_at: now,
            updated_at: now,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn rehydrate(
        id: LedgerAccountId,
        user_id: UserId,
        name: String,
        currency: CurrencyCode,
        nature: AccountNature,
        kind: AccountKind,
        authority: AccountAuthority,
        visibility: AccountVisibility,
        lifecycle: AccountLifecycle,
        system_role: Option<SystemAccountRole>,
        version: AccountVersion,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Result<Self, LedgerError> {
        Ok(Self {
            id,
            user_id,
            name: validate_name(name)?,
            currency,
            nature,
            kind,
            authority,
            visibility,
            lifecycle,
            system_role,
            version,
            created_at,
            updated_at,
        })
    }

    /// Renames a user-managed account with optimistic concurrency.
    pub fn rename(
        &mut self,
        name: impl Into<String>,
        expected_version: AccountVersion,
        clock: &(impl Clock + ?Sized),
    ) -> Result<bool, LedgerError> {
        self.require_user_managed()?;
        self.require_version(expected_version)?;
        let name = validate_name(name.into())?;
        if self.name == name {
            return Ok(false);
        }
        self.name = name;
        self.bump(clock.now())?;
        Ok(true)
    }

    /// Archives a user-managed account without removing history or balance.
    pub fn archive(
        &mut self,
        expected_version: AccountVersion,
        clock: &(impl Clock + ?Sized),
    ) -> Result<bool, LedgerError> {
        self.require_user_managed()?;
        self.require_version(expected_version)?;
        if self.lifecycle == AccountLifecycle::Archived {
            return Ok(false);
        }
        self.lifecycle = AccountLifecycle::Archived;
        self.bump(clock.now())?;
        Ok(true)
    }

    /// Restores an archived user-managed account.
    pub fn restore(
        &mut self,
        expected_version: AccountVersion,
        clock: &(impl Clock + ?Sized),
    ) -> Result<bool, LedgerError> {
        self.require_user_managed()?;
        self.require_version(expected_version)?;
        if self.lifecycle == AccountLifecycle::Active {
            return Ok(false);
        }
        self.lifecycle = AccountLifecycle::Active;
        self.bump(clock.now())?;
        Ok(true)
    }

    /// Rejects ordinary activity on archived accounts while permitting repairs.
    pub fn require_posting_allowed(&self, purpose: PostingPurpose) -> Result<(), LedgerError> {
        if self.lifecycle == AccountLifecycle::Archived && purpose == PostingPurpose::Ordinary {
            return Err(LedgerError::account_archived());
        }
        Ok(())
    }

    fn require_user_managed(&self) -> Result<(), LedgerError> {
        if self.authority == AccountAuthority::System {
            return Err(LedgerError::invalid_account_kind());
        }
        Ok(())
    }

    fn require_version(&self, expected: AccountVersion) -> Result<(), LedgerError> {
        if self.version != expected {
            return Err(LedgerError::version_conflict());
        }
        Ok(())
    }

    fn bump(&mut self, now: DateTime<Utc>) -> Result<(), LedgerError> {
        self.version = self.version.next()?;
        self.updated_at = now;
        Ok(())
    }

    /// Returns the account identity.
    pub const fn id(&self) -> LedgerAccountId {
        self.id
    }
    /// Returns the owning tenant.
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    /// Returns the account name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the immutable currency.
    pub const fn currency(&self) -> &CurrencyCode {
        &self.currency
    }
    /// Returns the accounting nature.
    pub const fn nature(&self) -> AccountNature {
        self.nature
    }
    /// Returns the closed kind.
    pub const fn kind(&self) -> AccountKind {
        self.kind
    }
    /// Returns the account authority.
    pub const fn authority(&self) -> AccountAuthority {
        self.authority
    }
    /// Returns account visibility.
    pub const fn visibility(&self) -> AccountVisibility {
        self.visibility
    }
    /// Returns whether this account is user visible.
    pub const fn is_user_visible(&self) -> bool {
        matches!(self.visibility, AccountVisibility::UserVisible)
    }
    /// Returns the lifecycle.
    pub const fn lifecycle(&self) -> AccountLifecycle {
        self.lifecycle
    }
    /// Returns the system role, if any.
    pub const fn system_role(&self) -> Option<SystemAccountRole> {
        self.system_role
    }
    /// Returns the optimistic version.
    pub const fn version(&self) -> AccountVersion {
        self.version
    }
    /// Returns the creation time.
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    /// Returns the last metadata-change time.
    pub const fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
    /// Returns the display-balance normal sign.
    pub const fn normal_sign(&self) -> i8 {
        self.nature.normal_sign()
    }
}

fn valid_user_kind(kind: AccountKind, nature: AccountNature) -> bool {
    matches!(
        (kind, nature),
        (
            AccountKind::Cash
                | AccountKind::DebitCard
                | AccountKind::Current
                | AccountKind::Savings
                | AccountKind::Jar
                | AccountKind::LoanReceivable,
            AccountNature::Asset
        ) | (
            AccountKind::CreditCard | AccountKind::LoanPayable,
            AccountNature::Liability
        )
    )
}

fn validate_name(name: String) -> Result<String, LedgerError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(LedgerError::invalid_name());
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared_kernel::FixedClock;
    use chrono::{TimeZone, Utc};

    #[test]
    fn system_account_construction_stays_inside_the_context() {
        let clock = FixedClock::new(Utc.with_ymd_and_hms(2026, 8, 5, 1, 0, 0).unwrap());
        let account = LedgerAccount::open_system(
            LedgerAccountId::generate(),
            UserId::generate(),
            CurrencyCode::new("UAH").unwrap(),
            SystemAccountRole::OpeningBalanceEquity,
            &clock,
        );
        assert_eq!(account.authority(), AccountAuthority::System);
        assert!(!account.is_user_visible());
    }
}
