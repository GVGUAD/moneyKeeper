//! External resource aggregate and audited Ledger mapping policy.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::contexts::ledger::public::{AccountKind, AccountNature, LedgerAccountId};
use crate::shared_kernel::{CurrencyCode, UserId};

use super::{BankingError, ExternalResourceId, ProviderConnectionId, ResourceMappingId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Card,
    CurrentAccount,
    Jar,
    SecurityPortfolio,
    Unsupported,
}

impl ResourceKind {
    pub const fn is_cash_like(self) -> bool {
        matches!(self, Self::Card | Self::CurrentAccount | Self::Jar)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FundingModel {
    OwnFunds,
    RevolvingCredit,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MappingDecision {
    Allowed,
    IncompatibleAccount,
    NeedsReview,
    RouteToPortfolio,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceMapping {
    id: ResourceMappingId,
    ledger_account_id: LedgerAccountId,
    active: bool,
    effective_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    end_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ExternalResource {
    id: ExternalResourceId,
    user_id: UserId,
    connection_id: ProviderConnectionId,
    external_resource_id: String,
    kind: ResourceKind,
    funding_model: FundingModel,
    currency: CurrencyCode,
    masked_label: String,
    version: i64,
    mappings: Vec<ResourceMapping>,
    imported: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ExternalResource {
    #[allow(clippy::too_many_arguments)]
    pub fn discover(
        user_id: UserId,
        connection_id: ProviderConnectionId,
        external_resource_id: impl Into<String>,
        kind: ResourceKind,
        funding_model: FundingModel,
        currency: CurrencyCode,
        masked_label: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Result<Self, BankingError> {
        let external_resource_id = bounded(external_resource_id.into(), 200)?;
        let masked_label = bounded(masked_label.into(), 200)?;
        Ok(Self {
            id: ExternalResourceId::generate(),
            user_id,
            connection_id,
            external_resource_id,
            kind,
            funding_model,
            currency,
            masked_label,
            version: 1,
            mappings: Vec::new(),
            imported: false,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn mapping_decision(&self, kind: AccountKind, nature: AccountNature) -> MappingDecision {
        if self.kind == ResourceKind::SecurityPortfolio {
            return MappingDecision::RouteToPortfolio;
        }
        if !self.kind.is_cash_like() || self.funding_model == FundingModel::Unknown {
            return MappingDecision::NeedsReview;
        }
        let allowed = match (self.kind, self.funding_model, kind, nature) {
            (
                ResourceKind::Card,
                FundingModel::OwnFunds,
                AccountKind::DebitCard,
                AccountNature::Asset,
            )
            | (
                ResourceKind::CurrentAccount,
                FundingModel::OwnFunds,
                AccountKind::Current,
                AccountNature::Asset,
            )
            | (ResourceKind::Jar, FundingModel::OwnFunds, AccountKind::Jar, AccountNature::Asset)
            | (
                ResourceKind::Card,
                FundingModel::RevolvingCredit,
                AccountKind::CreditCard,
                AccountNature::Liability,
            ) => true,
            _ => false,
        };
        if allowed {
            MappingDecision::Allowed
        } else {
            MappingDecision::IncompatibleAccount
        }
    }

    pub fn map(
        &mut self,
        ledger_account_id: LedgerAccountId,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> Result<ResourceMappingId, BankingError> {
        self.require_version(expected_version)?;
        if !self.kind.is_cash_like() {
            return Err(if self.kind == ResourceKind::SecurityPortfolio {
                BankingError::RouteToPortfolio
            } else {
                BankingError::IncompatibleMapping
            });
        }
        if self.funding_model == FundingModel::Unknown {
            return Err(BankingError::IncompatibleMapping);
        }
        if self.active_mapping().is_some() {
            return Err(BankingError::MappingAlreadyActive);
        }
        let id = ResourceMappingId::generate();
        self.mappings.push(ResourceMapping {
            id,
            ledger_account_id,
            active: true,
            effective_at: now,
            ended_at: None,
            end_reason: None,
        });
        self.bump(now)?;
        Ok(id)
    }

    pub fn deactivate_mapping(
        &mut self,
        expected_version: i64,
        reason: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Result<(), BankingError> {
        self.require_version(expected_version)?;
        let reason = bounded(reason.into(), 500)?;
        let mapping = self
            .mappings
            .iter_mut()
            .find(|mapping| mapping.active)
            .ok_or(BankingError::MappingNotActive)?;
        mapping.active = false;
        mapping.ended_at = Some(now);
        mapping.end_reason = Some(reason);
        self.bump(now)
    }

    pub fn change_currency(&mut self, currency: CurrencyCode) -> Result<(), BankingError> {
        if self.currency == currency {
            return Ok(());
        }
        if self.active_mapping().is_some() || self.imported {
            return Err(BankingError::InvalidState);
        }
        self.currency = currency;
        Ok(())
    }

    pub fn mark_imported(&mut self) {
        self.imported = true;
    }
    fn require_version(&self, expected: i64) -> Result<(), BankingError> {
        (self.version == expected)
            .then_some(())
            .ok_or(BankingError::VersionConflict)
    }
    fn bump(&mut self, now: DateTime<Utc>) -> Result<(), BankingError> {
        self.version = self
            .version
            .checked_add(1)
            .ok_or(BankingError::InvalidValue("resource version overflow"))?;
        self.updated_at = now;
        Ok(())
    }
    pub const fn id(&self) -> ExternalResourceId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub const fn connection_id(&self) -> ProviderConnectionId {
        self.connection_id
    }
    pub fn external_resource_id(&self) -> &str {
        &self.external_resource_id
    }
    pub const fn kind(&self) -> ResourceKind {
        self.kind
    }
    pub const fn funding_model(&self) -> FundingModel {
        self.funding_model
    }
    pub fn currency(&self) -> &CurrencyCode {
        &self.currency
    }
    pub fn masked_label(&self) -> &str {
        &self.masked_label
    }
    pub const fn version(&self) -> i64 {
        self.version
    }
    pub fn mapping_history(&self) -> &[ResourceMapping] {
        &self.mappings
    }
    pub fn active_mapping(&self) -> Option<&ResourceMapping> {
        self.mappings.iter().find(|mapping| mapping.active)
    }
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub const fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

impl ResourceMapping {
    pub const fn id(&self) -> ResourceMappingId {
        self.id
    }
    pub const fn ledger_account_id(&self) -> LedgerAccountId {
        self.ledger_account_id
    }
    pub const fn is_active(&self) -> bool {
        self.active
    }
    pub const fn effective_at(&self) -> DateTime<Utc> {
        self.effective_at
    }
    pub const fn ended_at(&self) -> Option<DateTime<Utc>> {
        self.ended_at
    }
    pub fn end_reason(&self) -> Option<&str> {
        self.end_reason.as_deref()
    }
}

fn bounded(value: String, max: usize) -> Result<String, BankingError> {
    if value.is_empty()
        || value.len() > max
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(BankingError::InvalidValue(
            "text must be bounded and printable",
        ))
    } else {
        Ok(value)
    }
}
