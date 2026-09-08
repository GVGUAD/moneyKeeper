//! Leased, quota-aware asynchronous classifier worker.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use super::classifier::{ClassifierFailureClass, TransactionClassifier};
use super::model::ThresholdPolicy;
use super::store::{AutomationError, ClassificationWorkFacade};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClassificationWorkerReport {
    pub claimed: bool,
    pub predicted: bool,
    pub review_queued: bool,
    pub abstained: bool,
    pub auto_apply_queued: bool,
    pub quota_deferred: bool,
    pub retry_scheduled: bool,
    pub failed: bool,
    pub fenced: bool,
}

#[derive(Clone)]
pub struct ClassificationWorker {
    store: ClassificationWorkFacade,
    classifier: Arc<dyn TransactionClassifier>,
    holder: String,
    lease_ttl: Duration,
    daily_cap: u32,
    max_attempts: i32,
    policy: ThresholdPolicy,
    auto_apply_enabled: bool,
}

impl ClassificationWorker {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        store: ClassificationWorkFacade,
        classifier: Arc<dyn TransactionClassifier>,
        holder: impl Into<String>,
        lease_ttl: Duration,
        daily_cap: u32,
        max_attempts: i32,
        policy: ThresholdPolicy,
        auto_apply_enabled: bool,
    ) -> Result<Self, AutomationError> {
        let holder = holder.into();
        if holder.trim() != holder
            || holder.is_empty()
            || holder.len() > 200
            || lease_ttl.is_zero()
            || daily_cap == 0
            || max_attempts < 1
        {
            return Err(AutomationError::Invalid);
        }
        Ok(Self {
            store,
            classifier,
            holder,
            lease_ttl,
            daily_cap,
            max_attempts,
            policy,
            auto_apply_enabled,
        })
    }

    pub async fn run_once(&self) -> Result<ClassificationWorkerReport, AutomationError> {
        match self.run_claimed().await {
            Err(AutomationError::Fenced) => Ok(ClassificationWorkerReport {
                claimed: true,
                fenced: true,
                ..ClassificationWorkerReport::default()
            }),
            result => result,
        }
    }

    async fn run_claimed(&self) -> Result<ClassificationWorkerReport, AutomationError> {
        let Some(claim) = self
            .store
            .claim_target(&self.holder, self.lease_ttl)
            .await?
        else {
            return Ok(ClassificationWorkerReport::default());
        };
        let now = Utc::now();
        if !self
            .store
            .reserve_call(&claim, &self.holder, self.daily_cap, now)
            .await?
        {
            self.store
                .defer_for_quota(&claim, &self.holder, now)
                .await?;
            return Ok(ClassificationWorkerReport {
                claimed: true,
                quota_deferred: true,
                ..ClassificationWorkerReport::default()
            });
        }

        let started = Instant::now();
        match self.classifier.classify(&claim.evidence).await {
            Ok(prediction) => {
                self.store.record_provider_outcome(&claim, true).await?;
                let auto_apply = self.auto_apply_enabled && self.store.rollout_qualifies().await?;
                let disposition = self.policy.disposition(&prediction, auto_apply);
                self.store
                    .persist_prediction(
                        &claim,
                        &self.holder,
                        self.classifier.provider_name(),
                        self.classifier.model_name(),
                        &prediction,
                        disposition,
                        started.elapsed(),
                        Utc::now(),
                    )
                    .await?;
                Ok(ClassificationWorkerReport {
                    claimed: true,
                    predicted: true,
                    review_queued: matches!(
                        disposition,
                        super::model::PredictionDisposition::ReviewPending
                    ),
                    abstained: matches!(
                        disposition,
                        super::model::PredictionDisposition::Abstained
                    ),
                    auto_apply_queued: matches!(
                        disposition,
                        super::model::PredictionDisposition::AutoApplyPending
                    ),
                    ..ClassificationWorkerReport::default()
                })
            }
            Err(error) => {
                self.store.record_provider_outcome(&claim, false).await?;
                let terminal = !error.is_retryable() || claim.attempts >= self.max_attempts;
                let retry_at = (!terminal).then(|| {
                    let delay = error
                        .retry_after()
                        .unwrap_or_else(|| exponential_backoff(claim.attempts));
                    add_duration(Utc::now(), delay)
                });
                self.store
                    .persist_failure(
                        &claim,
                        &self.holder,
                        failure_code(error.class()),
                        retry_at,
                        terminal,
                        Utc::now(),
                    )
                    .await?;
                Ok(ClassificationWorkerReport {
                    claimed: true,
                    retry_scheduled: !terminal,
                    failed: terminal,
                    ..ClassificationWorkerReport::default()
                })
            }
        }
    }
}

fn failure_code(class: ClassifierFailureClass) -> &'static str {
    match class {
        ClassifierFailureClass::Transient => "provider_transient",
        ClassifierFailureClass::RateLimited => "provider_rate_limited",
        ClassifierFailureClass::Terminal => "provider_terminal",
        ClassifierFailureClass::InvalidResponse => "provider_invalid_response",
        ClassifierFailureClass::Configuration => "provider_configuration",
    }
}

fn exponential_backoff(attempts: i32) -> Duration {
    let exponent = u32::try_from(attempts.clamp(0, 11)).unwrap_or(11);
    Duration::from_secs(2_u64.saturating_pow(exponent).min(3_600))
}

fn add_duration(now: DateTime<Utc>, duration: Duration) -> DateTime<Utc> {
    chrono::Duration::from_std(duration)
        .ok()
        .and_then(|value| now.checked_add_signed(value))
        .unwrap_or(now + chrono::Duration::hours(1))
}
