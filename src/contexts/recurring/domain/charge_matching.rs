use super::{Allocation, ChargeEvidenceId, DecisionSource, MatchId, MatchRecord, RecurringError};
use crate::shared_kernel::Money;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchingVersion(u64);
impl MatchingVersion {
    pub const INITIAL: Self = Self(0);
    pub const fn get(self) -> u64 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchingState {
    Undecided,
    PartiallyMatched,
    Matched,
    Rejected,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchingEvent {
    Matched {
        record: MatchRecord,
        version: MatchingVersion,
    },
    Rejected {
        reason: String,
        version: MatchingVersion,
        recorded_at: DateTime<Utc>,
    },
    Unmatched {
        match_id: MatchId,
        version: MatchingVersion,
        recorded_at: DateTime<Utc>,
    },
}
#[derive(Clone, Debug)]
pub struct ChargeMatching {
    evidence_id: ChargeEvidenceId,
    evidence_money: Money,
    version: MatchingVersion,
    state: MatchingState,
    active_matches: Vec<MatchRecord>,
    unmatched: Vec<MatchId>,
    events: Vec<MatchingEvent>,
}
impl ChargeMatching {
    pub fn new(
        evidence_id: ChargeEvidenceId,
        evidence_money: Money,
    ) -> Result<Self, RecurringError> {
        if evidence_money.amount() <= Decimal::ZERO {
            return Err(RecurringError::InvalidValue(
                "evidence amount must be positive",
            ));
        }
        Ok(Self {
            evidence_id,
            evidence_money,
            version: MatchingVersion::INITIAL,
            state: MatchingState::Undecided,
            active_matches: vec![],
            unmatched: vec![],
            events: vec![],
        })
    }
    pub fn allocate(
        &mut self,
        expected: MatchingVersion,
        allocations: Vec<Allocation>,
        source: DecisionSource,
        now: DateTime<Utc>,
    ) -> Result<MatchId, RecurringError> {
        self.require(expected)?;
        for a in &allocations {
            if a.amount().currency() != self.evidence_money.currency() {
                return Err(RecurringError::CurrencyMismatch);
            }
        }
        let mut allocated = self.active_total()?;
        for a in &allocations {
            allocated = allocated
                .checked_add(a.amount().amount())
                .ok_or(RecurringError::ArithmeticOverflow)?;
        }
        if allocated > self.evidence_money.amount() {
            return Err(RecurringError::AllocationOvercommit);
        }
        let record = MatchRecord::record(self.evidence_id, allocations, source, now)?;
        let id = record.id();
        self.bump()?;
        self.state = if allocated == self.evidence_money.amount() {
            MatchingState::Matched
        } else {
            MatchingState::PartiallyMatched
        };
        self.active_matches.push(record.clone());
        self.events.push(MatchingEvent::Matched {
            record,
            version: self.version,
        });
        Ok(id)
    }
    pub fn reject(
        &mut self,
        expected: MatchingVersion,
        reason: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Result<(), RecurringError> {
        self.require(expected)?;
        if !self.active_matches.is_empty() {
            return Err(RecurringError::InvalidState);
        }
        let reason = reason.into();
        if reason.trim() != reason || reason.is_empty() {
            return Err(RecurringError::InvalidValue("rejection reason"));
        }
        self.bump()?;
        self.state = MatchingState::Rejected;
        self.events.push(MatchingEvent::Rejected {
            reason,
            version: self.version,
            recorded_at: now,
        });
        Ok(())
    }
    pub fn unmatch(
        &mut self,
        expected: MatchingVersion,
        match_id: MatchId,
        now: DateTime<Utc>,
    ) -> Result<(), RecurringError> {
        self.require(expected)?;
        if self.unmatched.contains(&match_id) {
            return Err(RecurringError::AlreadyUnmatched);
        }
        let Some(index) = self.active_matches.iter().position(|m| m.id() == match_id) else {
            return Err(RecurringError::InvalidValue("unknown match"));
        };
        self.active_matches.remove(index);
        self.unmatched.push(match_id);
        self.bump()?;
        self.state = if self.active_matches.is_empty() {
            MatchingState::Undecided
        } else {
            MatchingState::PartiallyMatched
        };
        self.events.push(MatchingEvent::Unmatched {
            match_id,
            version: self.version,
            recorded_at: now,
        });
        Ok(())
    }
    fn require(&self, v: MatchingVersion) -> Result<(), RecurringError> {
        if self.version == v {
            Ok(())
        } else {
            Err(RecurringError::VersionConflict)
        }
    }
    fn bump(&mut self) -> Result<(), RecurringError> {
        self.version = MatchingVersion(
            self.version
                .0
                .checked_add(1)
                .ok_or(RecurringError::ArithmeticOverflow)?,
        );
        Ok(())
    }
    fn active_total(&self) -> Result<Decimal, RecurringError> {
        self.active_matches
            .iter()
            .flat_map(|m| m.allocations())
            .try_fold(Decimal::ZERO, |sum, a| {
                sum.checked_add(a.amount().amount())
                    .ok_or(RecurringError::ArithmeticOverflow)
            })
    }
    pub const fn evidence_id(&self) -> ChargeEvidenceId {
        self.evidence_id
    }
    pub const fn version(&self) -> MatchingVersion {
        self.version
    }
    pub const fn state(&self) -> MatchingState {
        self.state
    }
    pub fn active_matches(&self) -> &[MatchRecord] {
        &self.active_matches
    }
    pub fn pull_events(&mut self) -> Vec<MatchingEvent> {
        std::mem::take(&mut self.events)
    }
}
