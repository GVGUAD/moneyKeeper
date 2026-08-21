use super::RecurringError;
use crate::shared_kernel::{Money, UserId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
crate::define_uuid_id!(#[doc="Identifies a subscription."] pub SubscriptionId);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    Active,
    Paused,
    Cancelled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cadence {
    Weekly,
    Monthly,
    Quarterly,
    Yearly,
    Irregular,
}
#[derive(Clone, Debug)]
pub struct Subscription {
    id: SubscriptionId,
    user_id: UserId,
    merchant: String,
    status: SubscriptionStatus,
    cadence: Cadence,
    expected: Option<Money>,
    next_expected_at: Option<DateTime<Utc>>,
    version: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
impl Subscription {
    pub fn discover(
        user_id: UserId,
        merchant: impl Into<String>,
        cadence: Cadence,
        expected: Option<Money>,
        next_expected_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<Self, RecurringError> {
        let merchant = merchant.into();
        if merchant.trim() != merchant || merchant.is_empty() {
            return Err(RecurringError::InvalidValue("merchant"));
        }
        Ok(Self {
            id: SubscriptionId::generate(),
            user_id,
            merchant,
            status: SubscriptionStatus::Active,
            cadence,
            expected,
            next_expected_at,
            version: 1,
            created_at: now,
            updated_at: now,
        })
    }
    pub fn pause(&mut self, expected: u64, now: DateTime<Utc>) -> Result<(), RecurringError> {
        self.transition(expected, SubscriptionStatus::Paused, now)
    }
    pub fn resume(&mut self, expected: u64, now: DateTime<Utc>) -> Result<(), RecurringError> {
        self.transition(expected, SubscriptionStatus::Active, now)
    }
    pub fn cancel(&mut self, expected: u64, now: DateTime<Utc>) -> Result<(), RecurringError> {
        self.transition(expected, SubscriptionStatus::Cancelled, now)
    }
    fn transition(
        &mut self,
        expected: u64,
        state: SubscriptionStatus,
        now: DateTime<Utc>,
    ) -> Result<(), RecurringError> {
        if self.version != expected {
            return Err(RecurringError::VersionConflict);
        }
        if self.status == SubscriptionStatus::Cancelled {
            return Err(RecurringError::InvalidState);
        }
        self.status = state;
        self.version = self
            .version
            .checked_add(1)
            .ok_or(RecurringError::ArithmeticOverflow)?;
        self.updated_at = now;
        Ok(())
    }
    pub const fn id(&self) -> SubscriptionId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub fn merchant(&self) -> &str {
        &self.merchant
    }
    pub const fn status(&self) -> SubscriptionStatus {
        self.status
    }
    pub const fn cadence(&self) -> Cadence {
        self.cadence
    }
    pub fn expected(&self) -> Option<&Money> {
        self.expected.as_ref()
    }
    pub const fn next_expected_at(&self) -> Option<DateTime<Utc>> {
        self.next_expected_at
    }
    pub const fn version(&self) -> u64 {
        self.version
    }
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub const fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
}
