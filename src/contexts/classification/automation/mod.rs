mod classifier;
mod model;
mod openai;
mod rollout;
mod store;
mod worker;

pub use classifier::{ClassifierError, ClassifierFailureClass, TransactionClassifier};
pub use model::{
    AutomationDomainError, BackfillJobId, BackfillRange, BackfillState, CashFlowKind,
    ClassificationCategory, ClassificationCategoryKind, ClassificationDecisionId,
    ClassificationEvidence, ClassificationEvidenceInput, ClassificationTargetId, Confidence,
    DecisionState, FeedbackExample, FeedbackSignal, Prediction, PredictionReason, ReviewAction,
    ReviewCursor, ReviewItem, TargetOrigin, TargetState, ThresholdPolicy,
};
pub use openai::{DEFAULT_OPENAI_MODEL, OpenAiResponsesClassifier};
pub use rollout::RolloutEvidence;
pub(crate) use store::{ApplicationClaim, BackfillClaim, ClassificationWorkFacade, StaleTarget};
pub use store::{
    AutomationError, BackfillJobView, ClassificationAutomationFacade, ClassificationTargetStatus,
    DecisionSuggestion, EnqueueOutcome, ReviewPage,
};
pub(crate) use worker::ClassificationWorker;
