//! Durable state machine for Recurring-to-Ledger categorization.
use crate::contexts::recurring::public::{MatchId, RecurringError};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CategorizationState {
    Pending,
    Posted,
    RetryDue,
    TerminalNoEffect,
    Compensating,
    Compensated,
    CompensationSkippedNewerAnnotation,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecurringMatchProcess {
    pub match_id: MatchId,
    pub generation: u64,
    pub state: CategorizationState,
    pub prior_category_id: Option<uuid::Uuid>,
    pub prior_annotation_version: u64,
    pub produced_annotation_version: Option<u64>,
}
impl RecurringMatchProcess {
    pub fn start(
        match_id: MatchId,
        prior_category_id: Option<uuid::Uuid>,
        prior_annotation_version: u64,
    ) -> Self {
        Self {
            match_id,
            generation: 1,
            state: CategorizationState::Pending,
            prior_category_id,
            prior_annotation_version,
            produced_annotation_version: None,
        }
    }
    pub fn annotation_posted(&mut self, version: u64) -> Result<(), RecurringError> {
        if !matches!(
            self.state,
            CategorizationState::Pending | CategorizationState::RetryDue
        ) {
            return Err(RecurringError::InvalidState);
        }
        self.produced_annotation_version = Some(version);
        self.state = CategorizationState::Posted;
        Ok(())
    }
    pub fn request_unmatch(
        &mut self,
        current_annotation_version: u64,
    ) -> Result<(), RecurringError> {
        match self.state {
            CategorizationState::Pending | CategorizationState::RetryDue => {
                Err(RecurringError::CategorizationPending)
            }
            CategorizationState::TerminalNoEffect => {
                self.state = CategorizationState::Compensated;
                Ok(())
            }
            CategorizationState::Posted => {
                if self.produced_annotation_version != Some(current_annotation_version) {
                    self.state = CategorizationState::CompensationSkippedNewerAnnotation;
                } else {
                    self.state = CategorizationState::Compensating;
                }
                Ok(())
            }
            _ => Err(RecurringError::InvalidState),
        }
    }
    pub fn compensation_posted(&mut self) -> Result<(), RecurringError> {
        if self.state != CategorizationState::Compensating {
            return Err(RecurringError::InvalidState);
        }
        self.state = CategorizationState::Compensated;
        Ok(())
    }
}
