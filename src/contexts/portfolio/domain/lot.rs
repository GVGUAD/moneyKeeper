//! Deterministic pure lot allocation engine.

use super::{InstrumentId, PortfolioAccountId, PortfolioError, PortfolioTransactionId};
use crate::shared_kernel::{CurrencyCode, UserId};
use chrono::{DateTime, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

crate::define_uuid_id!(#[doc="Identifies a Portfolio acquisition lot."] pub LotId);
crate::define_uuid_id!(#[doc="Identifies an immutable lot allocation fact."] pub LotAllocationId);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LotCost {
    Known(#[serde(with = "rust_decimal::serde::str")] Decimal),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PositionLot {
    pub id: LotId,
    pub user_id: UserId,
    pub account_id: PortfolioAccountId,
    pub instrument_id: InstrumentId,
    pub source_transaction_id: PortfolioTransactionId,
    pub original_quantity: Decimal,
    pub remaining_quantity: Decimal,
    pub original_cost: LotCost,
    pub remaining_cost: LotCost,
    pub currency: CurrencyCode,
    pub acquired_at: DateTime<Utc>,
    pub created_sequence: u64,
}

impl PositionLot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        user_id: UserId,
        account_id: PortfolioAccountId,
        instrument_id: InstrumentId,
        source_transaction_id: PortfolioTransactionId,
        quantity: Decimal,
        cost: LotCost,
        currency: CurrencyCode,
        acquired_at: DateTime<Utc>,
        created_sequence: u64,
    ) -> Result<Self, PortfolioError> {
        if quantity <= Decimal::ZERO || !quantity.fract().is_zero() {
            return Err(PortfolioError::InvalidValue("lot_quantity"));
        }
        if created_sequence == 0 {
            return Err(PortfolioError::InvalidValue("created_sequence"));
        }
        if matches!(cost,LotCost::Known(v) if v<Decimal::ZERO) {
            return Err(PortfolioError::InvalidValue("lot_cost"));
        }
        Ok(Self {
            id: LotId::generate(),
            user_id,
            account_id,
            instrument_id,
            source_transaction_id,
            original_quantity: quantity,
            remaining_quantity: quantity,
            original_cost: cost.clone(),
            remaining_cost: cost,
            currency,
            acquired_at,
            created_sequence,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplicitLotAllocation {
    pub lot_id: LotId,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LotAllocation {
    pub id: LotAllocationId,
    pub lot_id: LotId,
    pub quantity: Decimal,
    pub allocated_cost: Option<Decimal>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LotAllocationResult {
    pub lots: Vec<PositionLot>,
    pub allocations: Vec<LotAllocation>,
    pub allocated_known_cost: Decimal,
    pub contains_unknown_cost: bool,
}
impl LotAllocationResult {
    pub fn realized_gain_loss(&self, proceeds: Decimal, fees: Decimal) -> Option<Decimal> {
        (!self.contains_unknown_cost).then(|| proceeds - self.allocated_known_cost - fees)
    }
}

pub fn allocate_fifo(
    lots: &[PositionLot],
    quantity: Decimal,
) -> Result<LotAllocationResult, PortfolioError> {
    allocate(lots, quantity, None)
}
pub fn allocate_explicit(
    lots: &[PositionLot],
    quantity: Decimal,
    requested: &[ExplicitLotAllocation],
) -> Result<LotAllocationResult, PortfolioError> {
    allocate(lots, quantity, Some(requested))
}

fn allocate(
    lots: &[PositionLot],
    quantity: Decimal,
    requested: Option<&[ExplicitLotAllocation]>,
) -> Result<LotAllocationResult, PortfolioError> {
    if quantity <= Decimal::ZERO || !quantity.fract().is_zero() {
        return Err(PortfolioError::InvalidValue("disposal_quantity"));
    }
    let mut working = lots.to_vec();
    if let Some(first) = working.first()
        && working.iter().any(|lot| {
            lot.user_id != first.user_id
                || lot.account_id != first.account_id
                || lot.instrument_id != first.instrument_id
                || lot.currency != first.currency
        })
    {
        return Err(PortfolioError::ForeignLot);
    }
    working.sort_by_key(|lot| (lot.acquired_at, lot.created_sequence, lot.id));
    let choices = if let Some(requested) = requested {
        let total = requested.iter().try_fold(Decimal::ZERO, |sum, item| {
            sum.checked_add(item.quantity)
                .ok_or(PortfolioError::Arithmetic)
        })?;
        if total != quantity {
            return Err(PortfolioError::AllocationMismatch);
        }
        requested
            .iter()
            .map(|item| (item.lot_id, item.quantity))
            .collect::<Vec<_>>()
    } else {
        let mut left = quantity;
        let mut choices = Vec::new();
        for lot in &working {
            if left.is_zero() {
                break;
            }
            let take = left.min(lot.remaining_quantity);
            if take > Decimal::ZERO {
                choices.push((lot.id, take));
                left -= take;
            }
        }
        if !left.is_zero() {
            return Err(PortfolioError::InsufficientQuantity);
        }
        choices
    };
    let mut allocations = Vec::new();
    let mut allocated_known_cost = Decimal::ZERO;
    let mut contains_unknown_cost = false;
    for (lot_id, take) in choices {
        if take <= Decimal::ZERO || !take.fract().is_zero() {
            return Err(PortfolioError::InvalidValue("allocation_quantity"));
        }
        let lot = working
            .iter_mut()
            .find(|lot| lot.id == lot_id)
            .ok_or(PortfolioError::ForeignLot)?;
        if take > lot.remaining_quantity {
            return Err(PortfolioError::InsufficientQuantity);
        }
        let cost = match lot.remaining_cost {
            LotCost::Known(remaining) => {
                let allocated = if take == lot.remaining_quantity {
                    remaining
                } else {
                    (remaining * take / lot.remaining_quantity)
                        .round_dp_with_strategy(8, RoundingStrategy::MidpointNearestEven)
                };
                lot.remaining_cost = LotCost::Known(remaining - allocated);
                allocated_known_cost += allocated;
                Some(allocated)
            }
            LotCost::Unknown => {
                contains_unknown_cost = true;
                None
            }
        };
        lot.remaining_quantity -= take;
        allocations.push(LotAllocation {
            id: LotAllocationId::generate(),
            lot_id,
            quantity: take,
            allocated_cost: cost,
        });
    }
    if allocations.iter().map(|a| a.quantity).sum::<Decimal>() != quantity {
        return Err(PortfolioError::InsufficientQuantity);
    }
    Ok(LotAllocationResult {
        lots: working,
        allocations,
        allocated_known_cost,
        contains_unknown_cost,
    })
}

pub fn restore_allocations(
    lots: &[PositionLot],
    allocations: &[LotAllocation],
) -> Result<Vec<PositionLot>, PortfolioError> {
    let mut restored = lots.to_vec();
    for allocation in allocations {
        let lot = restored
            .iter_mut()
            .find(|lot| lot.id == allocation.lot_id)
            .ok_or(PortfolioError::ForeignLot)?;
        lot.remaining_quantity = lot
            .remaining_quantity
            .checked_add(allocation.quantity)
            .ok_or(PortfolioError::Arithmetic)?;
        if lot.remaining_quantity > lot.original_quantity {
            return Err(PortfolioError::AllocationMismatch);
        }
        match (&mut lot.remaining_cost, allocation.allocated_cost) {
            (LotCost::Known(value), Some(cost)) => {
                *value = value.checked_add(cost).ok_or(PortfolioError::Arithmetic)?
            }
            (LotCost::Unknown, None) => {}
            _ => return Err(PortfolioError::AllocationMismatch),
        }
    }
    Ok(restored)
}
