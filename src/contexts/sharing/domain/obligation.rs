//! Deterministic debtor-to-creditor waterfall.

use super::{
    Contribution, Participant, ParticipantShare, SharingError, allocation::participant_nets,
};
use crate::shared_kernel::Money;
use rust_decimal::Decimal;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Obligation {
    pub debtor: Participant,
    pub creditor: Participant,
    pub amount: Money,
}

pub fn derive_obligations(
    contributions: &[Contribution],
    shares: &[ParticipantShare],
    minor_unit_scale: u32,
) -> Result<Vec<Obligation>, SharingError> {
    let currency = contributions
        .first()
        .map(|v| v.amount.currency().clone())
        .or_else(|| shares.first().map(|v| v.amount.currency().clone()))
        .ok_or(SharingError::Empty("allocations"))?;
    if contributions
        .iter()
        .any(|v| v.amount.currency() != &currency)
        || shares.iter().any(|v| v.amount.currency() != &currency)
    {
        return Err(SharingError::CurrencyMismatch);
    }
    let nets = participant_nets(contributions, shares)?;
    let total = nets.values().try_fold(Decimal::ZERO, |sum, value| {
        sum.checked_add(*value)
            .ok_or(SharingError::ArithmeticOverflow)
    })?;
    if !total.is_zero() {
        return Err(SharingError::ShareTotalMismatch);
    }
    let mut debtors: Vec<_> = nets
        .iter()
        .filter(|(_, value)| **value < Decimal::ZERO)
        .map(|(participant, value)| (*participant, -*value))
        .collect();
    let mut creditors: Vec<_> = nets
        .iter()
        .filter(|(_, value)| **value > Decimal::ZERO)
        .map(|(participant, value)| (*participant, *value))
        .collect();
    debtors.sort_by_key(|value| value.0);
    creditors.sort_by_key(|value| value.0);
    let (mut debtor_index, mut creditor_index) = (0, 0);
    let mut result = Vec::new();
    while debtor_index < debtors.len() && creditor_index < creditors.len() {
        let amount = debtors[debtor_index].1.min(creditors[creditor_index].1);
        if debtors[debtor_index].0 == creditors[creditor_index].0 {
            return Err(SharingError::SelfObligation);
        }
        result.push(Obligation {
            debtor: debtors[debtor_index].0,
            creditor: creditors[creditor_index].0,
            amount: Money::new(amount, currency.clone(), minor_unit_scale)?,
        });
        debtors[debtor_index].1 -= amount;
        creditors[creditor_index].1 -= amount;
        if debtors[debtor_index].1.is_zero() {
            debtor_index += 1;
        }
        if creditors[creditor_index].1.is_zero() {
            creditor_index += 1;
        }
    }
    if debtor_index != debtors.len() || creditor_index != creditors.len() {
        return Err(SharingError::ArithmeticOverflow);
    }
    Ok(result)
}
