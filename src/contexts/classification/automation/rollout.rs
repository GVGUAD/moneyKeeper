//! Evidence required before enabling automatic application.

#[derive(Clone, Copy, Debug, Default)]
pub struct RolloutEvidence {
    pub high_confidence_resolved: u64,
    pub accepted: u64,
    pub outbound_attempts: u64,
    pub unsuccessful_attempts: u64,
}

impl RolloutEvidence {
    pub fn qualifies(self) -> bool {
        self.high_confidence_resolved >= 200
            && self.accepted <= self.high_confidence_resolved
            && self.outbound_attempts > 0
            && self.unsuccessful_attempts <= self.outbound_attempts
            && u128::from(self.accepted) * 100 >= u128::from(self.high_confidence_resolved) * 95
            && u128::from(self.unsuccessful_attempts) * 100 < u128::from(self.outbound_attempts) * 5
    }
}
