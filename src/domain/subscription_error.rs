#[derive(Debug, thiserror::Error)]
pub enum SubscriptionError {
    #[error("no parser registered for sender: {0}")]
    ParserNotFound(String),
    #[error("OAuth refresh failed: {0}")]
    OAuthRefreshFailed(String),
    #[error("duplicate charge for message id: {0}")]
    DuplicateCharge(String),
    #[error("ambiguous match for charge {charge_id}: {candidate_count} candidates")]
    MatchAmbiguous {
        charge_id: uuid::Uuid,
        candidate_count: usize,
    },
    #[error("email connection not found")]
    ConnectionNotFound,
    #[error("subscription not found")]
    SubscriptionNotFound,
    #[error("subscription charge not found")]
    ChargeNotFound,
    #[error("email connection sync is already in progress")]
    SyncInProgress,
}
