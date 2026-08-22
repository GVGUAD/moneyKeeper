//! Immutable Portfolio transaction aggregate.

use super::{InstrumentId, PortfolioAccountId, PortfolioError};
use crate::shared_kernel::{CorrelationId, CurrencyCode, Money, UserId};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

crate::define_uuid_id!(#[doc = "Identifies an immutable Portfolio transaction."] pub PortfolioTransactionId);
crate::define_uuid_id!(#[doc = "Identifies the actor recording Portfolio activity."] pub PortfolioActorId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortfolioTransactionKind {
    OpeningPosition,
    Buy,
    Sell,
    Coupon,
    Redemption,
    PositionCorrection,
    Reversal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionSource {
    Manual,
    Correction,
    Reversal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionCost {
    Known(Money),
    UnknownCost,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize)]
pub struct TransactionComponents {
    pub acquisition_cost: Option<AcquisitionCost>,
    pub proceeds: Option<Money>,
    pub fee: Option<Money>,
    pub accrued_interest: Option<Money>,
    pub coupon: Option<Money>,
    pub cost_delta: Option<Money>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortfolioTransaction {
    id: PortfolioTransactionId,
    user_id: UserId,
    account_id: PortfolioAccountId,
    instrument_id: InstrumentId,
    kind: PortfolioTransactionKind,
    quantity: Decimal,
    currency: CurrencyCode,
    components: TransactionComponents,
    source: TransactionSource,
    reason: Option<String>,
    reversal_of: Option<PortfolioTransactionId>,
    reversed: bool,
    actor_id: PortfolioActorId,
    correlation_id: CorrelationId,
    effective_at: DateTime<Utc>,
    recorded_at: DateTime<Utc>,
    metadata: serde_json::Value,
}

impl PortfolioTransaction {
    #[allow(clippy::too_many_arguments)]
    pub fn opening_position(
        user_id: UserId,
        account_id: PortfolioAccountId,
        instrument_id: InstrumentId,
        quantity: Decimal,
        cost: AcquisitionCost,
        acquisition_date: NaiveDate,
        reason: impl Into<String>,
        currency: CurrencyCode,
        actor_id: PortfolioActorId,
        correlation_id: CorrelationId,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, PortfolioError> {
        let reason = require_reason(reason)?;
        require_whole_positive(quantity)?;
        validate_cost(&cost, &currency, false)?;
        Self::new(
            user_id,
            account_id,
            instrument_id,
            PortfolioTransactionKind::OpeningPosition,
            quantity,
            currency,
            TransactionComponents {
                acquisition_cost: Some(cost),
                ..Default::default()
            },
            TransactionSource::Manual,
            Some(reason),
            None,
            actor_id,
            correlation_id,
            acquisition_date
                .and_hms_opt(0, 0, 0)
                .ok_or(PortfolioError::InvalidValue("acquisition_date"))?
                .and_utc(),
            recorded_at,
            serde_json::json!({"acquisition_date": acquisition_date}),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn buy(
        user_id: UserId,
        account_id: PortfolioAccountId,
        instrument_id: InstrumentId,
        quantity: Decimal,
        acquisition_cost: Money,
        fee: Option<Money>,
        accrued_interest: Option<Money>,
        currency: CurrencyCode,
        actor_id: PortfolioActorId,
        correlation_id: CorrelationId,
        trade_at: DateTime<Utc>,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, PortfolioError> {
        require_whole_positive(quantity)?;
        validate_money(&acquisition_cost, &currency, true)?;
        validate_optional_money(fee.as_ref(), &currency, false)?;
        validate_optional_money(accrued_interest.as_ref(), &currency, false)?;
        Self::new(
            user_id,
            account_id,
            instrument_id,
            PortfolioTransactionKind::Buy,
            quantity,
            currency,
            TransactionComponents {
                acquisition_cost: Some(AcquisitionCost::Known(acquisition_cost)),
                fee,
                accrued_interest,
                ..Default::default()
            },
            TransactionSource::Manual,
            None,
            None,
            actor_id,
            correlation_id,
            trade_at,
            recorded_at,
            serde_json::json!({}),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn sell(
        user_id: UserId,
        account_id: PortfolioAccountId,
        instrument_id: InstrumentId,
        quantity: Decimal,
        proceeds: Money,
        fee: Option<Money>,
        currency: CurrencyCode,
        actor_id: PortfolioActorId,
        correlation_id: CorrelationId,
        trade_at: DateTime<Utc>,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, PortfolioError> {
        require_whole_positive(quantity)?;
        validate_money(&proceeds, &currency, true)?;
        validate_optional_money(fee.as_ref(), &currency, false)?;
        Self::new(
            user_id,
            account_id,
            instrument_id,
            PortfolioTransactionKind::Sell,
            quantity,
            currency,
            TransactionComponents {
                proceeds: Some(proceeds),
                fee,
                ..Default::default()
            },
            TransactionSource::Manual,
            None,
            None,
            actor_id,
            correlation_id,
            trade_at,
            recorded_at,
            serde_json::json!({}),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn coupon(
        user_id: UserId,
        account_id: PortfolioAccountId,
        instrument_id: InstrumentId,
        amount: Money,
        ex_date: Option<NaiveDate>,
        payment_date: NaiveDate,
        currency: CurrencyCode,
        actor_id: PortfolioActorId,
        correlation_id: CorrelationId,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, PortfolioError> {
        validate_money(&amount, &currency, true)?;
        Self::new(
            user_id,
            account_id,
            instrument_id,
            PortfolioTransactionKind::Coupon,
            Decimal::ZERO,
            currency,
            TransactionComponents {
                coupon: Some(amount),
                ..Default::default()
            },
            TransactionSource::Manual,
            None,
            None,
            actor_id,
            correlation_id,
            payment_date.and_hms_opt(0, 0, 0).unwrap().and_utc(),
            recorded_at,
            serde_json::json!({"ex_date":ex_date,"payment_date":payment_date}),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn redemption(
        user_id: UserId,
        account_id: PortfolioAccountId,
        instrument_id: InstrumentId,
        quantity: Decimal,
        proceeds: Money,
        maturity_date: NaiveDate,
        reference: impl Into<String>,
        currency: CurrencyCode,
        actor_id: PortfolioActorId,
        correlation_id: CorrelationId,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, PortfolioError> {
        require_whole_positive(quantity)?;
        validate_money(&proceeds, &currency, true)?;
        let reference = require_reason(reference)?;
        Self::new(
            user_id,
            account_id,
            instrument_id,
            PortfolioTransactionKind::Redemption,
            quantity,
            currency,
            TransactionComponents {
                proceeds: Some(proceeds),
                ..Default::default()
            },
            TransactionSource::Manual,
            None,
            None,
            actor_id,
            correlation_id,
            maturity_date.and_hms_opt(0, 0, 0).unwrap().and_utc(),
            recorded_at,
            serde_json::json!({"maturity_date":maturity_date,"reference":reference}),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn position_correction(
        user_id: UserId,
        account_id: PortfolioAccountId,
        instrument_id: InstrumentId,
        quantity_delta: Decimal,
        cost_delta: Option<Money>,
        reason: impl Into<String>,
        currency: CurrencyCode,
        actor_id: PortfolioActorId,
        correlation_id: CorrelationId,
        effective_at: DateTime<Utc>,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, PortfolioError> {
        if quantity_delta.is_zero() && cost_delta.as_ref().is_none_or(Money::is_zero) {
            return Err(PortfolioError::InvalidValue("correction"));
        }
        require_whole(quantity_delta)?;
        validate_optional_money(cost_delta.as_ref(), &currency, false)?;
        Self::new(
            user_id,
            account_id,
            instrument_id,
            PortfolioTransactionKind::PositionCorrection,
            quantity_delta,
            currency,
            TransactionComponents {
                cost_delta,
                ..Default::default()
            },
            TransactionSource::Correction,
            Some(require_reason(reason)?),
            None,
            actor_id,
            correlation_id,
            effective_at,
            recorded_at,
            serde_json::json!({}),
        )
    }
    pub fn reversal_of(
        original: &mut Self,
        actor_id: PortfolioActorId,
        reason: impl Into<String>,
        correlation_id: CorrelationId,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, PortfolioError> {
        if original.kind == PortfolioTransactionKind::Reversal {
            return Err(PortfolioError::CannotReverseReversal);
        }
        if original.reversed {
            return Err(PortfolioError::AlreadyReversed);
        }
        original.reversed = true;
        let components = negated_components(&original.components)?;
        Self::new(
            original.user_id,
            original.account_id,
            original.instrument_id,
            PortfolioTransactionKind::Reversal,
            -original.quantity,
            original.currency.clone(),
            components,
            TransactionSource::Reversal,
            Some(require_reason(reason)?),
            Some(original.id),
            actor_id,
            correlation_id,
            original.effective_at,
            recorded_at,
            original.metadata.clone(),
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn new(
        user_id: UserId,
        account_id: PortfolioAccountId,
        instrument_id: InstrumentId,
        kind: PortfolioTransactionKind,
        quantity: Decimal,
        currency: CurrencyCode,
        components: TransactionComponents,
        source: TransactionSource,
        reason: Option<String>,
        reversal_of: Option<PortfolioTransactionId>,
        actor_id: PortfolioActorId,
        correlation_id: CorrelationId,
        effective_at: DateTime<Utc>,
        recorded_at: DateTime<Utc>,
        metadata: serde_json::Value,
    ) -> Result<Self, PortfolioError> {
        Ok(Self {
            id: PortfolioTransactionId::generate(),
            user_id,
            account_id,
            instrument_id,
            kind,
            quantity,
            currency,
            components,
            source,
            reason,
            reversal_of,
            reversed: false,
            actor_id,
            correlation_id,
            effective_at,
            recorded_at,
            metadata,
        })
    }
    pub const fn id(&self) -> PortfolioTransactionId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub const fn account_id(&self) -> PortfolioAccountId {
        self.account_id
    }
    pub const fn instrument_id(&self) -> InstrumentId {
        self.instrument_id
    }
    pub const fn kind(&self) -> PortfolioTransactionKind {
        self.kind
    }
    pub const fn quantity(&self) -> Decimal {
        self.quantity
    }
    pub fn currency(&self) -> &CurrencyCode {
        &self.currency
    }
    pub fn components(&self) -> &TransactionComponents {
        &self.components
    }
    pub const fn source(&self) -> TransactionSource {
        self.source
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
    pub const fn reversal_of_id(&self) -> Option<PortfolioTransactionId> {
        self.reversal_of
    }
    pub const fn actor_id(&self) -> PortfolioActorId {
        self.actor_id
    }
    pub const fn correlation_id(&self) -> CorrelationId {
        self.correlation_id
    }
    pub const fn effective_at(&self) -> DateTime<Utc> {
        self.effective_at
    }
    pub const fn recorded_at(&self) -> DateTime<Utc> {
        self.recorded_at
    }
    pub fn metadata(&self) -> &serde_json::Value {
        &self.metadata
    }
}

fn require_whole(value: Decimal) -> Result<(), PortfolioError> {
    if !value.fract().is_zero() {
        Err(PortfolioError::FractionalOvdpQuantity)
    } else {
        Ok(())
    }
}
fn require_whole_positive(value: Decimal) -> Result<(), PortfolioError> {
    require_whole(value)?;
    if value <= Decimal::ZERO {
        Err(PortfolioError::InvalidValue("quantity"))
    } else {
        Ok(())
    }
}
fn require_reason(value: impl Into<String>) -> Result<String, PortfolioError> {
    let value = value.into();
    if value.is_empty() || value.trim() != value {
        Err(PortfolioError::InvalidValue("reason"))
    } else {
        Ok(value)
    }
}
fn validate_money(
    value: &Money,
    currency: &CurrencyCode,
    positive: bool,
) -> Result<(), PortfolioError> {
    if value.currency() != currency {
        return Err(PortfolioError::CurrencyMismatch);
    }
    if (positive && value.amount() <= Decimal::ZERO)
        || (!positive && value.amount() < Decimal::ZERO)
    {
        return Err(PortfolioError::InvalidValue("money"));
    }
    Ok(())
}
fn validate_optional_money(
    value: Option<&Money>,
    currency: &CurrencyCode,
    positive: bool,
) -> Result<(), PortfolioError> {
    if let Some(value) = value {
        validate_money(value, currency, positive)?
    }
    Ok(())
}
fn validate_cost(
    value: &AcquisitionCost,
    currency: &CurrencyCode,
    allow_zero: bool,
) -> Result<(), PortfolioError> {
    if let AcquisitionCost::Known(value) = value {
        validate_money(value, currency, !allow_zero)?
    }
    Ok(())
}
fn negated_money(value: &Option<Money>) -> Result<Option<Money>, PortfolioError> {
    value
        .as_ref()
        .map(|v| v.checked_neg().map_err(|_| PortfolioError::Arithmetic))
        .transpose()
}
fn negated_components(
    value: &TransactionComponents,
) -> Result<TransactionComponents, PortfolioError> {
    Ok(TransactionComponents {
        acquisition_cost: match &value.acquisition_cost {
            Some(AcquisitionCost::Known(v)) => Some(AcquisitionCost::Known(
                v.checked_neg().map_err(|_| PortfolioError::Arithmetic)?,
            )),
            Some(AcquisitionCost::UnknownCost) => Some(AcquisitionCost::UnknownCost),
            None => None,
        },
        proceeds: negated_money(&value.proceeds)?,
        fee: negated_money(&value.fee)?,
        accrued_interest: negated_money(&value.accrued_interest)?,
        coupon: negated_money(&value.coupon)?,
        cost_delta: negated_money(&value.cost_delta)?,
    })
}
