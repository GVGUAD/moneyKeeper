//! Exact/equal share resolution and multiple-payer contributions.

use super::{LedgerAccountReference, LedgerJournalReference, Participant, SharingError};
use crate::shared_kernel::Money;
use rust_decimal::{Decimal, prelude::ToPrimitive};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct JournalAllocation {
    pub journal_id: LedgerJournalReference,
    pub amount: Money,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContributionEvidence {
    External,
    Manual { account_id: LedgerAccountReference },
    ExistingJournals { allocations: Vec<JournalAllocation> },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Contribution {
    pub participant: Participant,
    pub amount: Money,
    pub evidence: ContributionEvidence,
}

impl Contribution {
    pub fn new(
        participant: Participant,
        amount: Money,
        evidence: ContributionEvidence,
    ) -> Result<Self, SharingError> {
        if amount.amount() <= Decimal::ZERO {
            return Err(SharingError::InvalidContribution);
        }
        if !matches!(participant, Participant::CurrentUser)
            && !matches!(evidence, ContributionEvidence::External)
        {
            return Err(SharingError::InvalidContribution);
        }
        if let ContributionEvidence::ExistingJournals { allocations } = &evidence {
            if allocations.is_empty()
                || allocations.iter().any(|item| {
                    item.amount.currency() != amount.currency()
                        || item.amount.amount() <= Decimal::ZERO
                })
            {
                return Err(SharingError::InvalidContribution);
            }
            let sum = allocations.iter().try_fold(Decimal::ZERO, |acc, item| {
                acc.checked_add(item.amount.amount())
                    .ok_or(SharingError::ArithmeticOverflow)
            })?;
            if sum != amount.amount() {
                return Err(SharingError::ContributionTotalMismatch);
            }
        }
        Ok(Self {
            participant,
            amount,
            evidence,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ExactShare {
    pub participant: Participant,
    pub amount: Money,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", content = "participants", rename_all = "snake_case")]
pub enum ShareRequest {
    Exact(Vec<ExactShare>),
    Equal(Vec<Participant>),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ParticipantShare {
    pub participant: Participant,
    pub amount: Money,
}

pub fn resolve_allocations(
    total: &Money,
    contributions: &[Contribution],
    request: ShareRequest,
    minor_unit_scale: u32,
) -> Result<Vec<ParticipantShare>, SharingError> {
    if total.amount() <= Decimal::ZERO {
        return Err(SharingError::InvalidTotal);
    }
    let contribution_total =
        contributions
            .iter()
            .try_fold(Decimal::ZERO, |sum, contribution| {
                if contribution.amount.currency() != total.currency() {
                    return Err(SharingError::CurrencyMismatch);
                }
                if contribution.amount.amount() <= Decimal::ZERO {
                    return Err(SharingError::InvalidContribution);
                }
                sum.checked_add(contribution.amount.amount())
                    .ok_or(SharingError::ArithmeticOverflow)
            })?;
    if contribution_total != total.amount() {
        return Err(SharingError::ContributionTotalMismatch);
    }
    let shares = match request {
        ShareRequest::Exact(values) => exact_shares(total, values)?,
        ShareRequest::Equal(participants) => equal_shares(total, participants, minor_unit_scale)?,
    };
    if shares.iter().all(|share| share.amount.is_zero()) {
        return Err(SharingError::AllZeroShares);
    }
    Ok(shares)
}

fn exact_shares(
    total: &Money,
    values: Vec<ExactShare>,
) -> Result<Vec<ParticipantShare>, SharingError> {
    let mut seen = BTreeSet::new();
    let mut sum = Decimal::ZERO;
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        if !seen.insert(value.participant) {
            return Err(SharingError::DuplicateParticipant);
        }
        if value.amount.currency() != total.currency() {
            return Err(SharingError::CurrencyMismatch);
        }
        if value.amount.amount() < Decimal::ZERO {
            return Err(SharingError::InvalidShare);
        }
        sum = sum
            .checked_add(value.amount.amount())
            .ok_or(SharingError::ArithmeticOverflow)?;
        result.push(ParticipantShare {
            participant: value.participant,
            amount: value.amount,
        });
    }
    if sum != total.amount() {
        return Err(SharingError::ShareTotalMismatch);
    }
    result.sort_by_key(|share| share.participant);
    Ok(result)
}

fn equal_shares(
    total: &Money,
    participants: Vec<Participant>,
    scale: u32,
) -> Result<Vec<ParticipantShare>, SharingError> {
    if participants.is_empty() {
        return Err(SharingError::EmptyEqualAllocation);
    }
    let unique: BTreeSet<_> = participants.iter().copied().collect();
    if unique.len() != participants.len() {
        return Err(SharingError::DuplicateParticipant);
    }
    let factor = 10_i128
        .checked_pow(scale)
        .ok_or(SharingError::ArithmeticOverflow)?;
    let scaled = total
        .amount()
        .checked_mul(Decimal::from_i128_with_scale(factor, 0))
        .ok_or(SharingError::ArithmeticOverflow)?;
    if scaled.fract() != Decimal::ZERO {
        return Err(SharingError::ArithmeticOverflow);
    }
    let units = scaled.to_i128().ok_or(SharingError::ArithmeticOverflow)?;
    let count = i128::try_from(unique.len()).map_err(|_| SharingError::ArithmeticOverflow)?;
    let base = units / count;
    let remainder = usize::try_from(units % count).map_err(|_| SharingError::ArithmeticOverflow)?;
    unique
        .into_iter()
        .enumerate()
        .map(|(index, participant)| {
            let value = base + i128::from(index < remainder);
            Ok(ParticipantShare {
                participant,
                amount: Money::new(
                    Decimal::from_i128_with_scale(value, scale),
                    total.currency().clone(),
                    scale,
                )?,
            })
        })
        .collect()
}

pub(crate) fn participant_nets(
    contributions: &[Contribution],
    shares: &[ParticipantShare],
) -> Result<BTreeMap<Participant, Decimal>, SharingError> {
    let mut nets = BTreeMap::new();
    for contribution in contributions {
        let value = nets
            .entry(contribution.participant)
            .or_insert(Decimal::ZERO);
        *value = value
            .checked_add(contribution.amount.amount())
            .ok_or(SharingError::ArithmeticOverflow)?;
    }
    for share in shares {
        let value = nets.entry(share.participant).or_insert(Decimal::ZERO);
        *value = value
            .checked_sub(share.amount.amount())
            .ok_or(SharingError::ArithmeticOverflow)?;
    }
    Ok(nets)
}
