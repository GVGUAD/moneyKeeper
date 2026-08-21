use super::{RecurringError, SubscriptionId};
use crate::shared_kernel::{Money, UserId};
use chrono::{DateTime, Utc};
crate::define_uuid_id!(#[doc="Identifies immutable charge evidence."] pub ChargeEvidenceId);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceKind {
    Renewal,
    OneTime,
    Refund,
    Cancellation,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChargeEvidence {
    id: ChargeEvidenceId,
    user_id: UserId,
    subscription_id: Option<SubscriptionId>,
    source_context: String,
    source_evidence_id: uuid::Uuid,
    kind: EvidenceKind,
    merchant: String,
    money: Option<Money>,
    charged_at: Option<DateTime<Utc>>,
    recorded_at: DateTime<Utc>,
}
impl ChargeEvidence {
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        user_id: UserId,
        subscription_id: Option<SubscriptionId>,
        source_context: impl Into<String>,
        source_evidence_id: uuid::Uuid,
        kind: EvidenceKind,
        merchant: impl Into<String>,
        money: Option<Money>,
        charged_at: Option<DateTime<Utc>>,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, RecurringError> {
        let source_context = source_context.into();
        let merchant = merchant.into();
        if source_context.is_empty() || merchant.is_empty() {
            return Err(RecurringError::InvalidValue("evidence provenance"));
        }
        if matches!(
            kind,
            EvidenceKind::Renewal | EvidenceKind::OneTime | EvidenceKind::Refund
        ) && money.is_none()
        {
            return Err(RecurringError::InvalidValue(
                "monetary evidence requires money",
            ));
        }
        Ok(Self {
            id: ChargeEvidenceId::generate(),
            user_id,
            subscription_id,
            source_context,
            source_evidence_id,
            kind,
            merchant,
            money,
            charged_at,
            recorded_at,
        })
    }
    pub const fn id(&self) -> ChargeEvidenceId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub fn money(&self) -> Option<&Money> {
        self.money.as_ref()
    }
    pub fn merchant(&self) -> &str {
        &self.merchant
    }
    pub const fn kind(&self) -> EvidenceKind {
        self.kind
    }
    pub fn source_context(&self) -> &str {
        &self.source_context
    }
    pub const fn source_evidence_id(&self) -> uuid::Uuid {
        self.source_evidence_id
    }
}
