//! Manual ОВДП instrument aggregate.

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::shared_kernel::{CurrencyCode, UserId};

use super::PortfolioError;

crate::define_uuid_id!(#[doc = "Identifies a Portfolio instrument aggregate."] pub InstrumentId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentifierKind {
    Isin,
    Manual,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentIdentifier {
    pub kind: IdentifierKind,
    pub value: String,
}

impl InstrumentIdentifier {
    pub fn new(kind: IdentifierKind, value: impl Into<String>) -> Result<Self, PortfolioError> {
        let value = value.into();
        if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
            return Err(PortfolioError::InvalidValue("identifier"));
        }
        if kind == IdentifierKind::Isin
            && (value.len() != 12 || !value.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        {
            return Err(PortfolioError::InvalidValue("isin"));
        }
        Ok(Self { kind, value })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CouponTerms {
    Fixed {
        #[serde(with = "rust_decimal::serde::str")]
        annual_rate: Decimal,
    },
    ZeroCoupon,
    Unknown,
}

impl CouponTerms {
    fn validate(&self) -> Result<(), PortfolioError> {
        if let Self::Fixed { annual_rate } = self
            && annual_rate.is_sign_negative()
        {
            return Err(PortfolioError::InvalidValue("coupon_rate"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentType {
    Ovdp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssuerType {
    SovereignBond,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentSource {
    Manual,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instrument {
    id: InstrumentId,
    user_id: UserId,
    identifier: InstrumentIdentifier,
    display_name: String,
    instrument_type: InstrumentType,
    issuer_type: IssuerType,
    currency: CurrencyCode,
    face_value: Decimal,
    issue_date: NaiveDate,
    maturity_date: NaiveDate,
    coupon_terms: CouponTerms,
    source: InstrumentSource,
    version: u64,
    created_at: DateTime<Utc>,
}

impl Instrument {
    #[allow(clippy::too_many_arguments)]
    pub fn manual_ovdp(
        user_id: UserId,
        identifier: InstrumentIdentifier,
        display_name: impl Into<String>,
        currency: CurrencyCode,
        face_value: Decimal,
        issue_date: NaiveDate,
        maturity_date: NaiveDate,
        coupon_terms: CouponTerms,
        now: DateTime<Utc>,
    ) -> Result<Self, PortfolioError> {
        let display_name = display_name.into();
        if display_name.is_empty() || display_name.trim() != display_name {
            return Err(PortfolioError::InvalidValue("display_name"));
        }
        if face_value <= Decimal::ZERO {
            return Err(PortfolioError::InvalidValue("face_value"));
        }
        if issue_date > maturity_date {
            return Err(PortfolioError::InvalidValue("maturity_date"));
        }
        coupon_terms.validate()?;
        Ok(Self {
            id: InstrumentId::generate(),
            user_id,
            identifier,
            display_name,
            instrument_type: InstrumentType::Ovdp,
            issuer_type: IssuerType::SovereignBond,
            currency,
            face_value,
            issue_date,
            maturity_date,
            coupon_terms,
            source: InstrumentSource::Manual,
            version: 1,
            created_at: now,
        })
    }
    pub const fn id(&self) -> InstrumentId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub fn identifier(&self) -> &InstrumentIdentifier {
        &self.identifier
    }
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
    pub const fn instrument_type(&self) -> InstrumentType {
        self.instrument_type
    }
    pub const fn issuer_type(&self) -> IssuerType {
        self.issuer_type
    }
    pub fn currency(&self) -> &CurrencyCode {
        &self.currency
    }
    pub const fn face_value(&self) -> Decimal {
        self.face_value
    }
    pub const fn issue_date(&self) -> NaiveDate {
        self.issue_date
    }
    pub const fn maturity_date(&self) -> NaiveDate {
        self.maturity_date
    }
    pub fn coupon_terms(&self) -> &CouponTerms {
        &self.coupon_terms
    }
    pub const fn source(&self) -> InstrumentSource {
        self.source
    }
    pub const fn version(&self) -> u64 {
        self.version
    }
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}
