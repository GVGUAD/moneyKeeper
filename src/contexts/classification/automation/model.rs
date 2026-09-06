//! Domain model for asynchronous transaction classification.

use std::collections::HashSet;
use std::fmt;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::shared_kernel::UserId;

crate::shared_kernel::define_uuid_id!(pub ClassificationTargetId);
crate::shared_kernel::define_uuid_id!(pub ClassificationDecisionId);
crate::shared_kernel::define_uuid_id!(pub BackfillJobId);

/// The maximum number of tenant-private examples included in one model call.
pub const MAX_FEEDBACK_EXAMPLES: usize = 20;
/// The maximum number of Unicode scalar values retained from a model explanation.
pub const MAX_EXPLANATION_CHARS: usize = 240;
/// The stable prompt contract stored alongside every prediction.
pub const PROMPT_VERSION: &str = "transaction-classification-v1";

/// Reports a rejected classification-domain value or state transition.
#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub enum AutomationDomainError {
    #[error("classification evidence is invalid")]
    InvalidEvidence,
    #[error("a classification category is invalid")]
    InvalidCategory,
    #[error("a feedback example is invalid")]
    InvalidExample,
    #[error("classification confidence must be between zero and one")]
    InvalidConfidence,
    #[error("classification explanation is empty")]
    InvalidExplanation,
    #[error("the prediction selected a category outside the supplied allowlist")]
    CategoryNotAllowed,
    #[error("the classification policy thresholds are invalid")]
    InvalidThresholds,
    #[error("the classification decision version is stale")]
    VersionConflict,
    #[error("the classification decision cannot make that transition")]
    InvalidTransition,
    #[error("the review resolution is invalid")]
    InvalidResolution,
    #[error("the backfill range is invalid")]
    InvalidBackfillRange,
    #[error("stored classification state is invalid")]
    InvalidStoredState,
}

/// Classifies the direction of an ordinary cash-flow transaction.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CashFlowKind {
    Income,
    Expense,
}

impl CashFlowKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Income => "income",
            Self::Expense => "expense",
        }
    }
}

/// The transaction directions to which a category may be assigned.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationCategoryKind {
    Income,
    Expense,
    Both,
}

impl ClassificationCategoryKind {
    pub const fn accepts(self, cash_flow: CashFlowKind) -> bool {
        matches!(
            (self, cash_flow),
            (Self::Both, _)
                | (Self::Income, CashFlowKind::Income)
                | (Self::Expense, CashFlowKind::Expense)
        )
    }
}

/// One effectively-active assignable taxonomy leaf supplied to the classifier.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClassificationCategory {
    id: Uuid,
    path: String,
    kind: ClassificationCategoryKind,
}

impl ClassificationCategory {
    /// Creates an allowed classification category.
    pub fn new(
        id: Uuid,
        path: impl Into<String>,
        kind: ClassificationCategoryKind,
    ) -> Result<Self, AutomationDomainError> {
        let path =
            normalize_required(path.into(), 500).ok_or(AutomationDomainError::InvalidCategory)?;
        Ok(Self { id, path, kind })
    }

    pub const fn id(&self) -> Uuid {
        self.id
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub const fn kind(&self) -> ClassificationCategoryKind {
        self.kind
    }
}

impl fmt::Debug for ClassificationCategory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClassificationCategory")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("path", &"[REDACTED]")
            .finish()
    }
}

/// Whether a tenant-private example supports or contradicts a category.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "polarity", rename_all = "snake_case")]
pub enum FeedbackSignal {
    Positive { category_id: Uuid },
    Negative { category_id: Uuid },
}

/// A minimized tenant-private labeled example supplied to the classifier.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackExample {
    description: String,
    #[serde(with = "rust_decimal::serde::str")]
    amount: Decimal,
    currency: String,
    provider: Option<String>,
    merchant_mcc: Option<u16>,
    signal: FeedbackSignal,
}

impl FeedbackExample {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        description: impl Into<String>,
        amount: Decimal,
        currency: impl Into<String>,
        provider: Option<String>,
        merchant_mcc: Option<u16>,
        signal: FeedbackSignal,
    ) -> Result<Self, AutomationDomainError> {
        let description = normalize_required(description.into(), 500)
            .ok_or(AutomationDomainError::InvalidExample)?;
        if amount <= Decimal::ZERO {
            return Err(AutomationDomainError::InvalidExample);
        }
        let currency =
            normalize_currency(currency.into()).ok_or(AutomationDomainError::InvalidExample)?;
        let provider =
            normalize_optional(provider, 100).ok_or(AutomationDomainError::InvalidExample)?;
        if merchant_mcc.is_some_and(|mcc| mcc > 9_999) {
            return Err(AutomationDomainError::InvalidExample);
        }
        Ok(Self {
            description,
            amount,
            currency,
            provider,
            merchant_mcc,
            signal,
        })
    }

    pub const fn signal(&self) -> FeedbackSignal {
        self.signal
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub const fn amount(&self) -> Decimal {
        self.amount
    }

    pub fn currency(&self) -> &str {
        &self.currency
    }

    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    pub const fn merchant_mcc(&self) -> Option<u16> {
        self.merchant_mcc
    }
}

impl fmt::Debug for FeedbackExample {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FeedbackExample")
            .field("currency", &self.currency)
            .field("merchant_mcc", &self.merchant_mcc)
            .field("signal", &self.signal)
            .field("evidence", &"[REDACTED]")
            .finish()
    }
}

/// Input used to create validated classification evidence.
pub struct ClassificationEvidenceInput {
    pub user_id: UserId,
    pub journal_entry_id: Uuid,
    pub description: String,
    pub amount: Decimal,
    pub currency: String,
    pub occurred_at: DateTime<Utc>,
    pub cash_flow_kind: CashFlowKind,
    pub provider: Option<String>,
    pub merchant_mcc: Option<u16>,
    pub account_label: Option<String>,
    pub taxonomy_version: i64,
    pub annotation_version: i64,
    pub categories: Vec<ClassificationCategory>,
    pub examples: Vec<FeedbackExample>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct EvidencePayload {
    pub(super) description: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub(super) amount: Decimal,
    pub(super) currency: String,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) cash_flow_kind: CashFlowKind,
    pub(super) provider: Option<String>,
    pub(super) merchant_mcc: Option<u16>,
    pub(super) account_label: Option<String>,
    pub(super) categories: Vec<ClassificationCategory>,
    pub(super) examples: Vec<FeedbackExample>,
}

/// Validated, minimized evidence for one transaction classification attempt.
#[derive(Clone)]
pub struct ClassificationEvidence {
    user_id: UserId,
    journal_entry_id: Uuid,
    taxonomy_version: i64,
    annotation_version: i64,
    evidence_digest: [u8; 32],
    taxonomy_digest: [u8; 32],
    payload: EvidencePayload,
}

impl ClassificationEvidence {
    /// Creates evidence while enforcing the classifier's data and category limits.
    pub fn new(mut input: ClassificationEvidenceInput) -> Result<Self, AutomationDomainError> {
        let description = normalize_required(input.description, 500)
            .ok_or(AutomationDomainError::InvalidEvidence)?;
        if input.amount <= Decimal::ZERO
            || input.taxonomy_version < 1
            || input.annotation_version < 1
        {
            return Err(AutomationDomainError::InvalidEvidence);
        }
        let currency =
            normalize_currency(input.currency).ok_or(AutomationDomainError::InvalidEvidence)?;
        let provider = normalize_optional(input.provider, 100)
            .ok_or(AutomationDomainError::InvalidEvidence)?;
        let account_label = normalize_optional(input.account_label, 200)
            .ok_or(AutomationDomainError::InvalidEvidence)?;
        if input.merchant_mcc.is_some_and(|mcc| mcc > 9_999) || input.categories.is_empty() {
            return Err(AutomationDomainError::InvalidEvidence);
        }

        let mut category_ids = HashSet::with_capacity(input.categories.len());
        if input.categories.iter().any(|category| {
            !category.kind.accepts(input.cash_flow_kind) || !category_ids.insert(category.id)
        }) {
            return Err(AutomationDomainError::InvalidEvidence);
        }
        input.examples.truncate(MAX_FEEDBACK_EXAMPLES);
        let payload = EvidencePayload {
            description,
            amount: input.amount,
            currency,
            occurred_at: input.occurred_at,
            cash_flow_kind: input.cash_flow_kind,
            provider,
            merchant_mcc: input.merchant_mcc,
            account_label,
            categories: input.categories,
            examples: input.examples,
        };
        let evidence_digest = digest_json(&payload)?;
        let taxonomy_digest = digest_json(&payload.categories)?;
        Ok(Self {
            user_id: input.user_id,
            journal_entry_id: input.journal_entry_id,
            taxonomy_version: input.taxonomy_version,
            annotation_version: input.annotation_version,
            evidence_digest,
            taxonomy_digest,
            payload,
        })
    }

    pub(super) fn from_stored(
        user_id: UserId,
        journal_entry_id: Uuid,
        taxonomy_version: i64,
        annotation_version: i64,
        payload: EvidencePayload,
    ) -> Result<Self, AutomationDomainError> {
        Self::new(ClassificationEvidenceInput {
            user_id,
            journal_entry_id,
            description: payload.description,
            amount: payload.amount,
            currency: payload.currency,
            occurred_at: payload.occurred_at,
            cash_flow_kind: payload.cash_flow_kind,
            provider: payload.provider,
            merchant_mcc: payload.merchant_mcc,
            account_label: payload.account_label,
            taxonomy_version,
            annotation_version,
            categories: payload.categories,
            examples: payload.examples,
        })
    }

    pub const fn user_id(&self) -> UserId {
        self.user_id
    }

    pub const fn journal_entry_id(&self) -> Uuid {
        self.journal_entry_id
    }

    pub const fn taxonomy_version(&self) -> i64 {
        self.taxonomy_version
    }

    pub const fn annotation_version(&self) -> i64 {
        self.annotation_version
    }

    pub const fn evidence_digest(&self) -> &[u8; 32] {
        &self.evidence_digest
    }

    pub const fn taxonomy_digest(&self) -> &[u8; 32] {
        &self.taxonomy_digest
    }

    pub fn categories(&self) -> &[ClassificationCategory] {
        &self.payload.categories
    }

    pub fn examples(&self) -> &[FeedbackExample] {
        &self.payload.examples
    }

    pub fn description(&self) -> &str {
        &self.payload.description
    }

    pub const fn amount(&self) -> Decimal {
        self.payload.amount
    }

    pub fn currency(&self) -> &str {
        &self.payload.currency
    }

    pub const fn occurred_at(&self) -> DateTime<Utc> {
        self.payload.occurred_at
    }

    pub const fn cash_flow_kind(&self) -> CashFlowKind {
        self.payload.cash_flow_kind
    }

    pub fn provider(&self) -> Option<&str> {
        self.payload.provider.as_deref()
    }

    pub const fn merchant_mcc(&self) -> Option<u16> {
        self.payload.merchant_mcc
    }

    pub fn account_label(&self) -> Option<&str> {
        self.payload.account_label.as_deref()
    }

    pub(super) fn payload(&self) -> &EvidencePayload {
        &self.payload
    }

    pub fn contains_category(&self, category_id: Uuid) -> bool {
        self.payload
            .categories
            .iter()
            .any(|category| category.id == category_id)
    }
}

impl fmt::Debug for ClassificationEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClassificationEvidence")
            .field("journal_entry_id", &self.journal_entry_id)
            .field("taxonomy_version", &self.taxonomy_version)
            .field("annotation_version", &self.annotation_version)
            .field("category_count", &self.payload.categories.len())
            .field("example_count", &self.payload.examples.len())
            .field("evidence", &"[REDACTED]")
            .finish()
    }
}

/// Confidence represented in basis points, where 10,000 is exactly 1.0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Confidence(u16);

impl Confidence {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(10_000);

    pub const fn from_basis_points(value: u16) -> Result<Self, AutomationDomainError> {
        if value <= 10_000 {
            Ok(Self(value))
        } else {
            Err(AutomationDomainError::InvalidConfidence)
        }
    }

    pub fn try_from_f64(value: f64) -> Result<Self, AutomationDomainError> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(AutomationDomainError::InvalidConfidence);
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let basis_points = (value * 10_000.0).floor() as u16;
        Self::from_basis_points(basis_points)
    }

    pub const fn basis_points(self) -> u16 {
        self.0
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.0) / 10_000.0
    }
}

/// A constrained explanation code returned by the model.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PredictionReason {
    MerchantMatch,
    MccMatch,
    DescriptionMatch,
    AmountPattern,
    TenantExample,
    MixedSignals,
    InsufficientEvidence,
}

impl PredictionReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MerchantMatch => "merchant_match",
            Self::MccMatch => "mcc_match",
            Self::DescriptionMatch => "description_match",
            Self::AmountPattern => "amount_pattern",
            Self::TenantExample => "tenant_example",
            Self::MixedSignals => "mixed_signals",
            Self::InsufficientEvidence => "insufficient_evidence",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, AutomationDomainError> {
        match value {
            "merchant_match" => Ok(Self::MerchantMatch),
            "mcc_match" => Ok(Self::MccMatch),
            "description_match" => Ok(Self::DescriptionMatch),
            "amount_pattern" => Ok(Self::AmountPattern),
            "tenant_example" => Ok(Self::TenantExample),
            "mixed_signals" => Ok(Self::MixedSignals),
            "insufficient_evidence" => Ok(Self::InsufficientEvidence),
            _ => Err(AutomationDomainError::InvalidStoredState),
        }
    }
}

/// Validated output of one provider call.
#[derive(Clone, PartialEq, Eq)]
pub struct Prediction {
    category_id: Option<Uuid>,
    confidence: Confidence,
    reason: PredictionReason,
    explanation: String,
}

impl Prediction {
    pub fn new(
        evidence: &ClassificationEvidence,
        category_id: Option<Uuid>,
        confidence: Confidence,
        reason: PredictionReason,
        explanation: impl Into<String>,
    ) -> Result<Self, AutomationDomainError> {
        if category_id.is_some_and(|id| !evidence.contains_category(id)) {
            return Err(AutomationDomainError::CategoryNotAllowed);
        }
        let explanation = sanitize_explanation(&explanation.into());
        if explanation.is_empty() {
            return Err(AutomationDomainError::InvalidExplanation);
        }
        Ok(Self {
            category_id,
            confidence,
            reason,
            explanation,
        })
    }

    pub const fn category_id(&self) -> Option<Uuid> {
        self.category_id
    }

    pub const fn confidence(&self) -> Confidence {
        self.confidence
    }

    pub const fn reason(&self) -> PredictionReason {
        self.reason
    }

    pub fn explanation(&self) -> &str {
        &self.explanation
    }
}

impl fmt::Debug for Prediction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Prediction")
            .field("category_id", &self.category_id)
            .field("confidence", &self.confidence)
            .field("reason", &self.reason)
            .field("explanation", &"[REDACTED]")
            .finish()
    }
}

/// The next workflow step selected from a prediction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PredictionDisposition {
    AutoApplyPending,
    ReviewPending,
    Abstained,
}

impl PredictionDisposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AutoApplyPending => "auto_apply_pending",
            Self::ReviewPending => "review_pending",
            Self::Abstained => "abstained",
        }
    }
}

/// Configurable conservative thresholds for prediction routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThresholdPolicy {
    auto_apply: Confidence,
    review: Confidence,
}

impl ThresholdPolicy {
    pub const CONSERVATIVE: Self = Self {
        auto_apply: Confidence(9_000),
        review: Confidence(6_000),
    };

    pub fn new(auto_apply: Confidence, review: Confidence) -> Result<Self, AutomationDomainError> {
        if review > auto_apply {
            return Err(AutomationDomainError::InvalidThresholds);
        }
        Ok(Self { auto_apply, review })
    }

    pub const fn auto_apply(self) -> Confidence {
        self.auto_apply
    }

    pub const fn review(self) -> Confidence {
        self.review
    }

    pub fn disposition(
        self,
        prediction: &Prediction,
        auto_apply_enabled: bool,
    ) -> PredictionDisposition {
        if prediction.category_id.is_none() || prediction.confidence < self.review {
            PredictionDisposition::Abstained
        } else if auto_apply_enabled && prediction.confidence >= self.auto_apply {
            PredictionDisposition::AutoApplyPending
        } else {
            PredictionDisposition::ReviewPending
        }
    }
}

/// Lifecycle state of a durable classification target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetState {
    Pending,
    RetryDue,
    QuotaDeferred,
    AutoApplyPending,
    ReviewPending,
    Applying,
    Abstained,
    Completed,
    Failed,
    Stale,
}

impl TargetState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::RetryDue => "retry_due",
            Self::QuotaDeferred => "quota_deferred",
            Self::AutoApplyPending => "auto_apply_pending",
            Self::ReviewPending => "review_pending",
            Self::Applying => "applying",
            Self::Abstained => "abstained",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stale => "stale",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, AutomationDomainError> {
        match value {
            "pending" => Ok(Self::Pending),
            "retry_due" => Ok(Self::RetryDue),
            "quota_deferred" => Ok(Self::QuotaDeferred),
            "auto_apply_pending" => Ok(Self::AutoApplyPending),
            "review_pending" => Ok(Self::ReviewPending),
            "applying" => Ok(Self::Applying),
            "abstained" => Ok(Self::Abstained),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "stale" => Ok(Self::Stale),
            _ => Err(AutomationDomainError::InvalidStoredState),
        }
    }
}

/// Scheduling class for a target. Live work is always claimed first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetOrigin {
    Live,
    Backfill,
}

impl TargetOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Backfill => "backfill",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, AutomationDomainError> {
        match value {
            "live" => Ok(Self::Live),
            "backfill" => Ok(Self::Backfill),
            _ => Err(AutomationDomainError::InvalidStoredState),
        }
    }
}

/// A fenced lease returned to one classifier worker.
pub struct ClassificationClaim {
    pub id: ClassificationTargetId,
    pub evidence: ClassificationEvidence,
    pub origin: TargetOrigin,
    pub backfill_job_id: Option<BackfillJobId>,
    pub generation: i64,
    pub attempts: i32,
    pub lease_token: i64,
}

impl fmt::Debug for ClassificationClaim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClassificationClaim")
            .field("id", &self.id)
            .field("origin", &self.origin)
            .field("backfill_job_id", &self.backfill_job_id)
            .field("generation", &self.generation)
            .field("attempts", &self.attempts)
            .field("lease_token", &self.lease_token)
            .field("evidence", &"[REDACTED]")
            .finish()
    }
}

/// A decision's review/action lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionState {
    AutoApplyPending,
    ReviewPending,
    Applying,
    Applied,
    Accepted,
    Corrected,
    Rejected,
    Abstained,
    Stale,
    Failed,
}

impl DecisionState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AutoApplyPending => "auto_apply_pending",
            Self::ReviewPending => "review_pending",
            Self::Applying => "applying",
            Self::Applied => "applied",
            Self::Accepted => "accepted",
            Self::Corrected => "corrected",
            Self::Rejected => "rejected",
            Self::Abstained => "abstained",
            Self::Stale => "stale",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, AutomationDomainError> {
        match value {
            "auto_apply_pending" => Ok(Self::AutoApplyPending),
            "review_pending" => Ok(Self::ReviewPending),
            "applying" => Ok(Self::Applying),
            "applied" => Ok(Self::Applied),
            "accepted" => Ok(Self::Accepted),
            "corrected" => Ok(Self::Corrected),
            "rejected" => Ok(Self::Rejected),
            "abstained" => Ok(Self::Abstained),
            "stale" => Ok(Self::Stale),
            "failed" => Ok(Self::Failed),
            _ => Err(AutomationDomainError::InvalidStoredState),
        }
    }
}

/// User action taken on a review suggestion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewAction {
    Accept,
    Correct,
    Reject,
}

impl ReviewAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Correct => "correct",
            Self::Reject => "reject",
        }
    }
}

/// A prediction plus its optimistic review state.
#[derive(Clone)]
pub struct ClassificationDecision {
    id: ClassificationDecisionId,
    target_id: ClassificationTargetId,
    user_id: UserId,
    journal_entry_id: Uuid,
    generation: i64,
    prediction: Prediction,
    state: DecisionState,
    version: i64,
    taxonomy_version: i64,
    annotation_version: i64,
    chosen_category_id: Option<Uuid>,
    resolution_action: Option<ReviewAction>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ClassificationDecision {
    /// Reconstitutes an audited decision; category allowlisting happened on prediction intake.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn reconstitute(
        id: ClassificationDecisionId,
        target_id: ClassificationTargetId,
        user_id: UserId,
        journal_entry_id: Uuid,
        generation: i64,
        candidate: Option<Uuid>,
        confidence: Confidence,
        reason: PredictionReason,
        explanation: String,
        state: DecisionState,
        version: i64,
        taxonomy_version: i64,
        annotation_version: i64,
        chosen_category_id: Option<Uuid>,
        resolution_action: Option<ReviewAction>,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Result<Self, AutomationDomainError> {
        if generation < 1 || version < 1 || taxonomy_version < 1 || annotation_version < 1 {
            return Err(AutomationDomainError::InvalidStoredState);
        }
        Ok(Self {
            id,
            target_id,
            user_id,
            journal_entry_id,
            generation,
            prediction: Prediction {
                category_id: candidate,
                confidence,
                reason,
                explanation,
            },
            state,
            version,
            taxonomy_version,
            annotation_version,
            chosen_category_id,
            resolution_action,
            created_at,
            updated_at,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record(
        id: ClassificationDecisionId,
        target_id: ClassificationTargetId,
        evidence: &ClassificationEvidence,
        generation: i64,
        prediction: Prediction,
        disposition: PredictionDisposition,
        now: DateTime<Utc>,
    ) -> Result<Self, AutomationDomainError> {
        if generation < 1 {
            return Err(AutomationDomainError::InvalidStoredState);
        }
        let state = match disposition {
            PredictionDisposition::AutoApplyPending => DecisionState::AutoApplyPending,
            PredictionDisposition::ReviewPending => DecisionState::ReviewPending,
            PredictionDisposition::Abstained => DecisionState::Abstained,
        };
        Ok(Self {
            id,
            target_id,
            user_id: evidence.user_id,
            journal_entry_id: evidence.journal_entry_id,
            generation,
            prediction,
            state,
            version: 1,
            taxonomy_version: evidence.taxonomy_version,
            annotation_version: evidence.annotation_version,
            chosen_category_id: None,
            resolution_action: None,
            created_at: now,
            updated_at: now,
        })
    }

    /// Moves a review decision to `applying` after validating action semantics.
    pub fn begin_review_resolution(
        &mut self,
        action: ReviewAction,
        replacement_category_id: Option<Uuid>,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> Result<(), AutomationDomainError> {
        self.require_version(expected_version)?;
        if self.state != DecisionState::ReviewPending {
            return Err(AutomationDomainError::InvalidTransition);
        }
        let chosen = match action {
            ReviewAction::Accept if replacement_category_id.is_none() => self
                .prediction
                .category_id
                .ok_or(AutomationDomainError::InvalidResolution)?,
            ReviewAction::Correct => {
                let replacement =
                    replacement_category_id.ok_or(AutomationDomainError::InvalidResolution)?;
                if Some(replacement) == self.prediction.category_id {
                    return Err(AutomationDomainError::InvalidResolution);
                }
                replacement
            }
            ReviewAction::Reject if replacement_category_id.is_none() => {
                self.chosen_category_id = None;
                self.resolution_action = Some(action);
                self.state = DecisionState::Applying;
                self.bump(now);
                return Ok(());
            }
            _ => return Err(AutomationDomainError::InvalidResolution),
        };
        self.chosen_category_id = Some(chosen);
        self.resolution_action = Some(action);
        self.state = DecisionState::Applying;
        self.bump(now);
        Ok(())
    }

    pub fn complete_resolution(
        &mut self,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> Result<(), AutomationDomainError> {
        self.require_version(expected_version)?;
        if !matches!(
            self.state,
            DecisionState::Applying | DecisionState::AutoApplyPending
        ) {
            return Err(AutomationDomainError::InvalidTransition);
        }
        self.state = match self.resolution_action {
            Some(ReviewAction::Accept) => DecisionState::Accepted,
            Some(ReviewAction::Correct) => DecisionState::Corrected,
            Some(ReviewAction::Reject) => DecisionState::Rejected,
            None => DecisionState::Applied,
        };
        self.bump(now);
        Ok(())
    }

    pub fn mark_stale(
        &mut self,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> Result<(), AutomationDomainError> {
        self.require_version(expected_version)?;
        if matches!(
            self.state,
            DecisionState::Applied
                | DecisionState::Accepted
                | DecisionState::Corrected
                | DecisionState::Rejected
                | DecisionState::Abstained
        ) {
            return Err(AutomationDomainError::InvalidTransition);
        }
        self.state = DecisionState::Stale;
        self.bump(now);
        Ok(())
    }

    fn require_version(&self, expected_version: i64) -> Result<(), AutomationDomainError> {
        if self.version == expected_version {
            Ok(())
        } else {
            Err(AutomationDomainError::VersionConflict)
        }
    }

    fn bump(&mut self, now: DateTime<Utc>) {
        self.version += 1;
        self.updated_at = now;
    }

    pub const fn id(&self) -> ClassificationDecisionId {
        self.id
    }

    pub const fn target_id(&self) -> ClassificationTargetId {
        self.target_id
    }

    pub const fn user_id(&self) -> UserId {
        self.user_id
    }

    pub const fn journal_entry_id(&self) -> Uuid {
        self.journal_entry_id
    }

    pub const fn generation(&self) -> i64 {
        self.generation
    }

    pub fn prediction(&self) -> &Prediction {
        &self.prediction
    }

    pub const fn state(&self) -> DecisionState {
        self.state
    }

    pub const fn version(&self) -> i64 {
        self.version
    }

    pub const fn taxonomy_version(&self) -> i64 {
        self.taxonomy_version
    }

    pub const fn annotation_version(&self) -> i64 {
        self.annotation_version
    }

    pub const fn chosen_category_id(&self) -> Option<Uuid> {
        self.chosen_category_id
    }

    pub const fn resolution_action(&self) -> Option<ReviewAction> {
        self.resolution_action
    }

    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub const fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

impl fmt::Debug for ClassificationDecision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClassificationDecision")
            .field("id", &self.id)
            .field("target_id", &self.target_id)
            .field("journal_entry_id", &self.journal_entry_id)
            .field("generation", &self.generation)
            .field("state", &self.state)
            .field("version", &self.version)
            .field("prediction", &self.prediction)
            .finish()
    }
}

/// Half-open UTC range requested for an asynchronous historical backfill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackfillRange {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
}

impl BackfillRange {
    pub fn new(from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Self, AutomationDomainError> {
        if from >= to {
            return Err(AutomationDomainError::InvalidBackfillRange);
        }
        Ok(Self { from, to })
    }

    pub const fn from(self) -> DateTime<Utc> {
        self.from
    }

    pub const fn to(self) -> DateTime<Utc> {
        self.to
    }
}

/// Durable backfill lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackfillState {
    Requested,
    Running,
    QuotaDeferred,
    Completed,
    Failed,
    Cancelled,
}

impl BackfillState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Running => "running",
            Self::QuotaDeferred => "quota_deferred",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, AutomationDomainError> {
        match value {
            "requested" => Ok(Self::Requested),
            "running" => Ok(Self::Running),
            "quota_deferred" => Ok(Self::QuotaDeferred),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(AutomationDomainError::InvalidStoredState),
        }
    }
}

/// Read model returned by the review queue.
#[derive(Clone)]
pub struct ReviewItem {
    pub decision_id: ClassificationDecisionId,
    pub decision_version: i64,
    pub journal_entry_id: Uuid,
    pub candidate_category_id: Uuid,
    pub confidence: Confidence,
    pub reason: PredictionReason,
    pub explanation: String,
    pub taxonomy_version: i64,
    pub annotation_version: i64,
    pub created_at: DateTime<Utc>,
}

impl fmt::Debug for ReviewItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReviewItem")
            .field("decision_id", &self.decision_id)
            .field("journal_entry_id", &self.journal_entry_id)
            .field("candidate_category_id", &self.candidate_category_id)
            .field("confidence", &self.confidence)
            .field("reason", &self.reason)
            .field("explanation", &"[REDACTED]")
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// Stable oldest-first cursor for the review queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReviewCursor {
    pub created_at: DateTime<Utc>,
    pub decision_id: ClassificationDecisionId,
}

fn digest_json(value: &impl Serialize) -> Result<[u8; 32], AutomationDomainError> {
    let encoded = serde_json::to_vec(value).map_err(|_| AutomationDomainError::InvalidEvidence)?;
    Ok(Sha256::digest(encoded).into())
}

fn normalize_required(value: String, max_chars: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.chars().count() <= max_chars).then(|| value.to_owned())
}

fn normalize_optional(value: Option<String>, max_chars: usize) -> Option<Option<String>> {
    match value {
        None => Some(None),
        Some(value) => normalize_required(value, max_chars).map(Some),
    }
}

fn normalize_currency(value: String) -> Option<String> {
    let value = value.trim().to_ascii_uppercase();
    (value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_uppercase())).then_some(value)
}

pub fn sanitize_explanation(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_EXPLANATION_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;
    use rust_decimal_macros::dec;

    use super::*;

    fn evidence() -> ClassificationEvidence {
        let category_id = Uuid::from_u128(3);
        ClassificationEvidence::new(ClassificationEvidenceInput {
            user_id: UserId::new(Uuid::from_u128(1)),
            journal_entry_id: Uuid::from_u128(2),
            description: "Example merchant".to_owned(),
            amount: dec!(12.34),
            currency: "uah".to_owned(),
            occurred_at: Utc.with_ymd_and_hms(2026, 9, 4, 10, 0, 0).unwrap(),
            cash_flow_kind: CashFlowKind::Expense,
            provider: Some("monobank".to_owned()),
            merchant_mcc: Some(5411),
            account_label: Some("Daily card".to_owned()),
            taxonomy_version: 7,
            annotation_version: 2,
            categories: vec![
                ClassificationCategory::new(
                    category_id,
                    "Expenses / Food / Groceries",
                    ClassificationCategoryKind::Expense,
                )
                .unwrap(),
            ],
            examples: Vec::new(),
        })
        .unwrap()
    }

    #[test]
    fn evidence_debug_redacts_descriptions_and_account_labels() {
        let debug = format!("{:?}", evidence());
        assert!(!debug.contains("Example merchant"));
        assert!(!debug.contains("Daily card"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn evidence_rejects_wrong_kind_or_duplicate_category_ids() {
        let mut input = ClassificationEvidenceInput {
            user_id: UserId::new(Uuid::from_u128(1)),
            journal_entry_id: Uuid::from_u128(2),
            description: "Example".to_owned(),
            amount: dec!(1),
            currency: "UAH".to_owned(),
            occurred_at: Utc::now(),
            cash_flow_kind: CashFlowKind::Income,
            provider: None,
            merchant_mcc: None,
            account_label: None,
            taxonomy_version: 1,
            annotation_version: 1,
            categories: vec![
                ClassificationCategory::new(
                    Uuid::from_u128(3),
                    "Expenses / Food",
                    ClassificationCategoryKind::Expense,
                )
                .unwrap(),
            ],
            examples: Vec::new(),
        };
        assert_eq!(
            ClassificationEvidence::new(input).unwrap_err(),
            AutomationDomainError::InvalidEvidence
        );

        input = ClassificationEvidenceInput {
            user_id: UserId::new(Uuid::from_u128(1)),
            journal_entry_id: Uuid::from_u128(2),
            description: "Example".to_owned(),
            amount: dec!(1),
            currency: "UAH".to_owned(),
            occurred_at: Utc::now(),
            cash_flow_kind: CashFlowKind::Expense,
            provider: None,
            merchant_mcc: None,
            account_label: None,
            taxonomy_version: 1,
            annotation_version: 1,
            categories: vec![
                ClassificationCategory::new(
                    Uuid::from_u128(3),
                    "First",
                    ClassificationCategoryKind::Both,
                )
                .unwrap(),
                ClassificationCategory::new(
                    Uuid::from_u128(3),
                    "Second",
                    ClassificationCategoryKind::Both,
                )
                .unwrap(),
            ],
            examples: Vec::new(),
        };
        assert_eq!(
            ClassificationEvidence::new(input).unwrap_err(),
            AutomationDomainError::InvalidEvidence
        );
    }

    #[test]
    fn conservative_policy_honors_review_only_rollout() {
        let evidence = evidence();
        let category_id = evidence.categories()[0].id();
        let high = Prediction::new(
            &evidence,
            Some(category_id),
            Confidence::from_basis_points(9_000).unwrap(),
            PredictionReason::MccMatch,
            "MCC and merchant agree",
        )
        .unwrap();
        assert_eq!(
            ThresholdPolicy::CONSERVATIVE.disposition(&high, false),
            PredictionDisposition::ReviewPending
        );
        assert_eq!(
            ThresholdPolicy::CONSERVATIVE.disposition(&high, true),
            PredictionDisposition::AutoApplyPending
        );

        let low = Prediction::new(
            &evidence,
            Some(category_id),
            Confidence::from_basis_points(5_999).unwrap(),
            PredictionReason::MixedSignals,
            "Weak evidence",
        )
        .unwrap();
        assert_eq!(
            ThresholdPolicy::CONSERVATIVE.disposition(&low, true),
            PredictionDisposition::Abstained
        );
    }

    #[test]
    fn prediction_rejects_out_of_allowlist_and_sanitizes_explanation() {
        let evidence = evidence();
        assert_eq!(
            Prediction::new(
                &evidence,
                Some(Uuid::from_u128(99)),
                Confidence::ONE,
                PredictionReason::MerchantMatch,
                "not allowed",
            )
            .unwrap_err(),
            AutomationDomainError::CategoryNotAllowed
        );

        let prediction = Prediction::new(
            &evidence,
            Some(evidence.categories()[0].id()),
            Confidence::ONE,
            PredictionReason::MerchantMatch,
            format!("line\n\t{}", "x".repeat(300)),
        )
        .unwrap();
        assert!(!prediction.explanation().contains('\n'));
        assert_eq!(prediction.explanation().chars().count(), 240);
    }

    #[test]
    fn review_decision_enforces_resolution_shape_and_version() {
        let evidence = evidence();
        let candidate = evidence.categories()[0].id();
        let prediction = Prediction::new(
            &evidence,
            Some(candidate),
            Confidence::from_basis_points(7_500).unwrap(),
            PredictionReason::DescriptionMatch,
            "Description matched",
        )
        .unwrap();
        let now = Utc::now();
        let mut decision = ClassificationDecision::record(
            ClassificationDecisionId::new(Uuid::from_u128(8)),
            ClassificationTargetId::new(Uuid::from_u128(7)),
            &evidence,
            1,
            prediction,
            PredictionDisposition::ReviewPending,
            now,
        )
        .unwrap();
        assert_eq!(
            decision.begin_review_resolution(ReviewAction::Accept, Some(candidate), 1, now,),
            Err(AutomationDomainError::InvalidResolution)
        );
        decision
            .begin_review_resolution(ReviewAction::Accept, None, 1, now)
            .unwrap();
        assert_eq!(decision.state(), DecisionState::Applying);
        assert_eq!(decision.chosen_category_id(), Some(candidate));
        decision.complete_resolution(2, now).unwrap();
        assert_eq!(decision.state(), DecisionState::Accepted);
    }

    #[test]
    fn feedback_examples_are_bounded_to_twenty() {
        let mut evidence = evidence();
        let category_id = evidence.categories()[0].id();
        let example = FeedbackExample::new(
            "Known merchant",
            dec!(5),
            "UAH",
            None,
            None,
            FeedbackSignal::Positive { category_id },
        )
        .unwrap();
        let input = ClassificationEvidenceInput {
            user_id: evidence.user_id(),
            journal_entry_id: evidence.journal_entry_id(),
            description: evidence.payload.description.clone(),
            amount: evidence.payload.amount,
            currency: evidence.payload.currency.clone(),
            occurred_at: evidence.payload.occurred_at,
            cash_flow_kind: evidence.payload.cash_flow_kind,
            provider: evidence.payload.provider.clone(),
            merchant_mcc: evidence.payload.merchant_mcc,
            account_label: evidence.payload.account_label.clone(),
            taxonomy_version: evidence.taxonomy_version(),
            annotation_version: evidence.annotation_version(),
            categories: std::mem::take(&mut evidence.payload.categories),
            examples: vec![example; 25],
        };
        let bounded = ClassificationEvidence::new(input).unwrap();
        assert_eq!(bounded.examples().len(), MAX_FEEDBACK_EXAMPLES);
    }
}
