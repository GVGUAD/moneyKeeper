//! PostgreSQL persistence and application facade for durable classification work.

use std::time::Duration;

use chrono::{DateTime, Days, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::shared_kernel::{IdempotencyKey, UserId};

use super::model::{
    BackfillJobId, BackfillRange, BackfillState, ClassificationClaim, ClassificationDecision,
    ClassificationDecisionId, ClassificationEvidence, ClassificationTargetId, Confidence,
    EvidencePayload, FeedbackExample, FeedbackSignal, Prediction, PredictionDisposition,
    PredictionReason, ReviewAction, ReviewCursor, ReviewItem, TargetOrigin,
};

#[derive(Debug, thiserror::Error)]
pub enum AutomationError {
    #[error("classification resource was not found")]
    NotFound,
    #[error("classification request is invalid")]
    Invalid,
    #[error("classification state or version conflict")]
    Conflict,
    #[error("classification lease was fenced")]
    Fenced,
    #[error("classification persistence is unavailable")]
    Persistence(#[source] sqlx::Error),
}

impl AutomationError {
    pub const fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound)
    }

    pub const fn is_invalid(&self) -> bool {
        matches!(self, Self::Invalid)
    }

    pub const fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict | Self::Fenced)
    }
}

fn database(error: sqlx::Error) -> AutomationError {
    if error
        .as_database_error()
        .is_some_and(|value| value.code().as_deref() == Some("23505"))
    {
        AutomationError::Conflict
    } else {
        AutomationError::Persistence(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnqueueOutcome {
    pub target_id: ClassificationTargetId,
    pub generation: i64,
    pub queued: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecisionSuggestion {
    pub decision_id: ClassificationDecisionId,
    pub decision_version: i64,
    pub journal_entry_id: Uuid,
    pub state: super::model::DecisionState,
    pub candidate_category_id: Option<Uuid>,
    pub chosen_category_id: Option<Uuid>,
    pub confidence: f64,
    pub reason_code: String,
    pub explanation: String,
    pub taxonomy_version: i64,
    pub annotation_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct ReviewPage {
    pub items: Vec<ReviewItem>,
    pub next_cursor: Option<ReviewCursor>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ClassificationTargetStatus {
    pub target_id: ClassificationTargetId,
    pub state: super::model::TargetState,
    pub outbound_attempts: i32,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub last_error_code: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackfillJobView {
    pub id: BackfillJobId,
    pub state: BackfillState,
    pub range_from: DateTime<Utc>,
    pub range_to: DateTime<Utc>,
    pub cursor_occurred_at: Option<DateTime<Utc>>,
    pub cursor_journal_entry_id: Option<Uuid>,
    pub cursor_ledger_sequence: Option<i64>,
    pub classified_count: i64,
    pub review_count: i64,
    pub abstained_count: i64,
    pub failed_count: i64,
    pub quota_resume_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct BackfillClaim {
    pub id: BackfillJobId,
    pub user_id: UserId,
    pub range: BackfillRange,
    pub cursor_occurred_at: Option<DateTime<Utc>>,
    pub cursor_journal_entry_id: Option<Uuid>,
    pub cursor_ledger_sequence: Option<i64>,
    pub lease_token: i64,
}

#[derive(Clone, Debug)]
pub struct ApplicationClaim {
    pub decision_id: ClassificationDecisionId,
    pub decision_version: i64,
    pub target_id: ClassificationTargetId,
    pub generation: i64,
    pub user_id: UserId,
    pub journal_entry_id: Uuid,
    pub category_id: Option<Uuid>,
    pub action: Option<ReviewAction>,
    pub annotation_version: i64,
    pub taxonomy_version: i64,
    pub candidate_category_id: Option<Uuid>,
    pub occurred_at: DateTime<Utc>,
    pub lease_token: i64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct StaleTarget {
    pub(crate) target_id: ClassificationTargetId,
    pub(crate) generation: i64,
    pub(crate) user_id: UserId,
    pub(crate) journal_entry_id: Uuid,
    pub(crate) origin: TargetOrigin,
    pub(crate) backfill_job_id: Option<BackfillJobId>,
}

#[derive(Clone)]
pub(crate) struct ClassificationWorkFacade {
    pool: PgPool,
}

impl ClassificationWorkFacade {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) async fn enqueue(
        &self,
        evidence: &ClassificationEvidence,
        origin: TargetOrigin,
        backfill_job_id: Option<BackfillJobId>,
    ) -> Result<EnqueueOutcome, AutomationError> {
        if (origin == TargetOrigin::Backfill) != backfill_job_id.is_some() {
            return Err(AutomationError::Invalid);
        }
        let mut transaction = self.pool.begin().await.map_err(database)?;
        let existing = sqlx::query(
            "SELECT id,generation,taxonomy_version,annotation_version,evidence_digest,state \
             FROM classification.classification_targets \
             WHERE user_id=$1 AND journal_entry_id=$2 FOR UPDATE",
        )
        .bind(evidence.user_id().into_uuid())
        .bind(evidence.journal_entry_id())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database)?;
        let payload =
            serde_json::to_value(evidence.payload()).map_err(|_| AutomationError::Invalid)?;
        let outcome = if let Some(row) = existing {
            let id = ClassificationTargetId::new(row.get("id"));
            let generation: i64 = row.get("generation");
            let same = row.get::<i64, _>("taxonomy_version") == evidence.taxonomy_version()
                && row.get::<i64, _>("annotation_version") == evidence.annotation_version()
                && row.get::<Vec<u8>, _>("evidence_digest").as_slice()
                    == evidence.evidence_digest();
            if same {
                EnqueueOutcome {
                    target_id: id,
                    generation,
                    queued: false,
                }
            } else {
                sqlx::query(
                    "UPDATE classification.classification_decisions SET state='stale',version=version+1,updated_at=clock_timestamp() \
                     WHERE user_id=$1 AND target_id=$2 AND state IN ('auto_apply_pending','review_pending','applying')",
                )
                .bind(evidence.user_id().into_uuid())
                .bind(id.into_uuid())
                .execute(&mut *transaction)
                .await
                .map_err(database)?;
                let next = generation.checked_add(1).ok_or(AutomationError::Conflict)?;
                sqlx::query(
                    "UPDATE classification.classification_targets SET origin=$3,backfill_job_id=$4,state='pending',generation=$5, \
                     taxonomy_version=$6,annotation_version=$7,evidence_digest=$8,taxonomy_digest=$9,evidence=$10, \
                     source_occurred_at=$11,attempts=0,outbound_attempts=0,next_attempt_at=NULL,last_error_code=NULL, \
                     lease_holder=NULL,lease_expires_at=NULL,updated_at=clock_timestamp() WHERE user_id=$1 AND id=$2",
                )
                .bind(evidence.user_id().into_uuid())
                .bind(id.into_uuid())
                .bind(origin.as_str())
                .bind(backfill_job_id.map(BackfillJobId::into_uuid))
                .bind(next)
                .bind(evidence.taxonomy_version())
                .bind(evidence.annotation_version())
                .bind(evidence.evidence_digest().as_slice())
                .bind(evidence.taxonomy_digest().as_slice())
                .bind(payload)
                .bind(evidence.occurred_at())
                .execute(&mut *transaction)
                .await
                .map_err(database)?;
                EnqueueOutcome {
                    target_id: id,
                    generation: next,
                    queued: true,
                }
            }
        } else {
            let id = ClassificationTargetId::generate();
            sqlx::query(
                "INSERT INTO classification.classification_targets( \
                 id,user_id,journal_entry_id,origin,backfill_job_id,state,generation,taxonomy_version,annotation_version, \
                 evidence_digest,taxonomy_digest,evidence,source_occurred_at) \
                 VALUES($1,$2,$3,$4,$5,'pending',1,$6,$7,$8,$9,$10,$11)",
            )
            .bind(id.into_uuid())
            .bind(evidence.user_id().into_uuid())
            .bind(evidence.journal_entry_id())
            .bind(origin.as_str())
            .bind(backfill_job_id.map(BackfillJobId::into_uuid))
            .bind(evidence.taxonomy_version())
            .bind(evidence.annotation_version())
            .bind(evidence.evidence_digest().as_slice())
            .bind(evidence.taxonomy_digest().as_slice())
            .bind(payload)
            .bind(evidence.occurred_at())
            .execute(&mut *transaction)
            .await
            .map_err(database)?;
            EnqueueOutcome {
                target_id: id,
                generation: 1,
                queued: true,
            }
        };
        transaction.commit().await.map_err(database)?;
        Ok(outcome)
    }

    pub(crate) async fn fence_transaction(
        &self,
        user_id: UserId,
        journal_entry_id: Uuid,
        applied_decision_id: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<(), AutomationError> {
        let mut transaction = self.pool.begin().await.map_err(database)?;
        let target_id = sqlx::query_scalar::<_, Uuid>(
            "UPDATE classification.classification_targets SET state='stale', \
             lease_holder=NULL,lease_expires_at=NULL,updated_at=$3 \
             WHERE user_id=$1 AND journal_entry_id=$2 \
               AND state IN ('pending','retry_due','quota_deferred','auto_apply_pending','review_pending','applying') \
               AND NOT EXISTS (SELECT 1 FROM classification.classification_decisions decision \
                 WHERE decision.id=$4 AND decision.user_id=$1 \
                   AND decision.target_id=classification.classification_targets.id \
                   AND decision.generation=classification.classification_targets.generation) \
             RETURNING id",
        )
        .bind(user_id.into_uuid())
        .bind(journal_entry_id)
        .bind(now)
        .bind(applied_decision_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database)?;
        if let Some(target_id) = target_id {
            sqlx::query(
                "UPDATE classification.classification_decisions SET state='stale',version=version+1,updated_at=$3 \
                 WHERE user_id=$1 AND target_id=$2 \
                   AND state IN ('auto_apply_pending','review_pending','applying')",
            )
            .bind(user_id.into_uuid())
            .bind(target_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(database)?;
        }
        transaction.commit().await.map_err(database)
    }

    pub(crate) async fn next_stale_target(&self) -> Result<Option<StaleTarget>, AutomationError> {
        let row = sqlx::query(
            "SELECT id,generation,user_id,journal_entry_id,origin,backfill_job_id \
             FROM classification.classification_targets WHERE state='stale' \
             ORDER BY CASE WHEN origin='live' THEN 0 ELSE 1 END,updated_at,id LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(database)?;
        row.map(|row| {
            Ok(StaleTarget {
                target_id: ClassificationTargetId::new(row.get("id")),
                generation: row.get("generation"),
                user_id: UserId::new(row.get("user_id")),
                journal_entry_id: row.get("journal_entry_id"),
                origin: TargetOrigin::parse(row.get::<String, _>("origin").as_str())
                    .map_err(|_| AutomationError::Invalid)?,
                backfill_job_id: row
                    .get::<Option<Uuid>, _>("backfill_job_id")
                    .map(BackfillJobId::new),
            })
        })
        .transpose()
    }

    pub(crate) async fn retire_stale_target(
        &self,
        target: StaleTarget,
        now: DateTime<Utc>,
    ) -> Result<(), AutomationError> {
        sqlx::query(
            "UPDATE classification.classification_targets SET state='completed',updated_at=$4 \
             WHERE id=$1 AND user_id=$2 AND generation=$3 AND state='stale'",
        )
        .bind(target.target_id.into_uuid())
        .bind(target.user_id.into_uuid())
        .bind(target.generation)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(database)?;
        Ok(())
    }

    pub(crate) async fn record_manual_feedback(
        &self,
        evidence: &ClassificationEvidence,
        positive_category_id: Option<Uuid>,
        negative_category_id: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<(), AutomationError> {
        if positive_category_id.is_none() && negative_category_id.is_none() {
            return Err(AutomationError::Invalid);
        }
        let source = if positive_category_id.is_some() {
            "manual"
        } else {
            "automatic_removed"
        };
        sqlx::query(
            "INSERT INTO classification.classification_feedback_examples( \
             id,user_id,journal_entry_id,source,positive_category_id,negative_category_id,description,amount,currency, \
             occurred_at,provider,merchant_mcc,account_label,evidence_digest,created_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) ON CONFLICT DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(evidence.user_id().into_uuid())
        .bind(evidence.journal_entry_id())
        .bind(source)
        .bind(positive_category_id)
        .bind(negative_category_id)
        .bind(evidence.description())
        .bind(evidence.amount())
        .bind(evidence.currency())
        .bind(evidence.occurred_at())
        .bind(evidence.provider())
        .bind(evidence.merchant_mcc().map(i32::from))
        .bind(evidence.account_label())
        .bind(evidence.evidence_digest().as_slice())
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(database)?;
        Ok(())
    }

    pub(crate) async fn ranked_examples(
        &self,
        user_id: UserId,
        provider: Option<&str>,
        merchant_mcc: Option<u16>,
        limit: usize,
    ) -> Result<Vec<FeedbackExample>, AutomationError> {
        let row_limit = i64::try_from(limit.max(1)).map_err(|_| AutomationError::Invalid)?;
        let rows = sqlx::query(
            "SELECT description,amount,currency,provider,merchant_mcc,positive_category_id,negative_category_id \
             FROM classification.classification_feedback_examples WHERE user_id=$1 \
             ORDER BY (merchant_mcc IS NOT DISTINCT FROM $2::integer) DESC, \
                      (provider IS NOT DISTINCT FROM $3::text) DESC,created_at DESC,id DESC LIMIT $4",
        )
        .bind(user_id.into_uuid())
        .bind(merchant_mcc.map(i32::from))
        .bind(provider)
        .bind(row_limit)
        .fetch_all(&self.pool)
        .await
        .map_err(database)?;
        let mut examples = Vec::with_capacity(limit);
        for row in rows {
            for signal in [
                row.get::<Option<Uuid>, _>("positive_category_id")
                    .map(|category_id| FeedbackSignal::Positive { category_id }),
                row.get::<Option<Uuid>, _>("negative_category_id")
                    .map(|category_id| FeedbackSignal::Negative { category_id }),
            ]
            .into_iter()
            .flatten()
            {
                let example = FeedbackExample::new(
                    row.get::<String, _>("description"),
                    row.get("amount"),
                    row.get::<String, _>("currency"),
                    row.get("provider"),
                    row.get::<Option<i32>, _>("merchant_mcc")
                        .and_then(|value| u16::try_from(value).ok()),
                    signal,
                )
                .map_err(|_| AutomationError::Invalid)?;
                examples.push(example);
                if examples.len() == limit {
                    return Ok(examples);
                }
            }
        }
        Ok(examples)
    }

    pub(crate) async fn claim_target(
        &self,
        holder: &str,
        lease_ttl: Duration,
    ) -> Result<Option<ClassificationClaim>, AutomationError> {
        validate_holder(holder, lease_ttl)?;
        let lease_ms = duration_millis(lease_ttl)?;
        let row = sqlx::query(
            "WITH candidate AS ( \
               SELECT id,user_id FROM classification.classification_targets \
               WHERE state IN ('pending','retry_due','quota_deferred') \
                 AND (next_attempt_at IS NULL OR next_attempt_at<=clock_timestamp()) \
                 AND (lease_expires_at IS NULL OR lease_expires_at<=clock_timestamp()) \
               ORDER BY CASE WHEN origin='live' THEN 0 ELSE 1 END, \
                        CASE WHEN origin='backfill' THEN source_occurred_at END DESC,created_at,id \
               FOR UPDATE SKIP LOCKED LIMIT 1 \
             ) \
             UPDATE classification.classification_targets target SET \
               lease_holder=$1,lease_token=target.lease_token+1, \
               lease_expires_at=clock_timestamp()+($2::bigint*interval '1 millisecond'), \
               updated_at=clock_timestamp() \
             FROM candidate WHERE target.id=candidate.id AND target.user_id=candidate.user_id \
             RETURNING target.id,target.user_id,target.journal_entry_id,target.origin,target.backfill_job_id,target.generation, \
               target.outbound_attempts+1 AS attempts,target.lease_token,target.taxonomy_version,target.annotation_version,target.evidence",
        )
        .bind(holder)
        .bind(lease_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(database)?;
        row.map(|row| {
            let payload: EvidencePayload = serde_json::from_value(row.get("evidence"))
                .map_err(|_| AutomationError::Invalid)?;
            let evidence = ClassificationEvidence::from_stored(
                UserId::new(row.get("user_id")),
                row.get("journal_entry_id"),
                row.get("taxonomy_version"),
                row.get("annotation_version"),
                payload,
            )
            .map_err(|_| AutomationError::Invalid)?;
            Ok(ClassificationClaim {
                id: ClassificationTargetId::new(row.get("id")),
                evidence,
                origin: TargetOrigin::parse(row.get::<String, _>("origin").as_str())
                    .map_err(|_| AutomationError::Invalid)?,
                backfill_job_id: row
                    .get::<Option<Uuid>, _>("backfill_job_id")
                    .map(BackfillJobId::new),
                generation: row.get("generation"),
                attempts: row.get("attempts"),
                lease_token: row.get("lease_token"),
            })
        })
        .transpose()
    }

    pub(crate) async fn reserve_call(
        &self,
        claim: &ClassificationClaim,
        holder: &str,
        daily_cap: u32,
        now: DateTime<Utc>,
    ) -> Result<bool, AutomationError> {
        if daily_cap == 0 {
            return Ok(false);
        }
        let user_id = claim.evidence.user_id();
        let mut transaction = self.pool.begin().await.map_err(database)?;
        let taxonomy_version: i64 = sqlx::query_scalar(
            "SELECT version FROM classification.category_taxonomies WHERE user_id=$1 FOR SHARE",
        )
        .bind(user_id.into_uuid())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database)?;
        if taxonomy_version != claim.evidence.taxonomy_version() {
            sqlx::query("UPDATE classification.classification_targets SET state='stale',lease_holder=NULL,lease_expires_at=NULL WHERE id=$1 AND user_id=$2 AND generation=$3 AND lease_token=$4")
                .bind(claim.id.into_uuid()).bind(user_id.into_uuid()).bind(claim.generation).bind(claim.lease_token)
                .execute(&mut *transaction).await.map_err(database)?;
            transaction.commit().await.map_err(database)?;
            return Err(AutomationError::Fenced);
        }
        let current: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM classification.classification_targets \
             WHERE id=$1 AND user_id=$2 AND generation=$3 AND lease_token=$4 \
               AND lease_holder=$5 AND lease_expires_at>clock_timestamp() FOR UPDATE)",
        )
        .bind(claim.id.into_uuid())
        .bind(user_id.into_uuid())
        .bind(claim.generation)
        .bind(claim.lease_token)
        .bind(holder)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database)?;
        if !current {
            return Err(AutomationError::Fenced);
        }
        let used = sqlx::query_scalar::<_, i32>(
            "INSERT INTO classification.classification_daily_usage(user_id,usage_date,outbound_calls,updated_at) \
             VALUES($1,$2,1,$3) ON CONFLICT(user_id,usage_date) DO UPDATE SET \
             outbound_calls=classification.classification_daily_usage.outbound_calls+1,updated_at=EXCLUDED.updated_at \
             WHERE classification.classification_daily_usage.outbound_calls<$4 RETURNING outbound_calls",
        )
        .bind(user_id.into_uuid())
        .bind(now.date_naive())
        .bind(now)
        .bind(i32::try_from(daily_cap).map_err(|_| AutomationError::Invalid)?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database)?;
        if used.is_some() {
            sqlx::query("INSERT INTO classification.classification_provider_attempts(target_id,user_id,lease_token,reserved_at) VALUES($1,$2,$3,$4)")
                .bind(claim.id.into_uuid()).bind(user_id.into_uuid()).bind(claim.lease_token).bind(now)
                .execute(&mut *transaction).await.map_err(database)?;
            sqlx::query("UPDATE classification.classification_targets SET outbound_attempts=outbound_attempts+1 WHERE id=$1 AND user_id=$2")
                .bind(claim.id.into_uuid()).bind(user_id.into_uuid()).execute(&mut *transaction).await.map_err(database)?;
        }
        transaction.commit().await.map_err(database)?;
        Ok(used.is_some())
    }

    pub(crate) async fn record_provider_outcome(
        &self,
        claim: &ClassificationClaim,
        succeeded: bool,
    ) -> Result<(), AutomationError> {
        sqlx::query("UPDATE classification.classification_provider_attempts SET outcome=$4,completed_at=clock_timestamp() WHERE target_id=$1 AND user_id=$2 AND lease_token=$3 AND outcome IS NULL")
            .bind(claim.id.into_uuid()).bind(claim.evidence.user_id().into_uuid()).bind(claim.lease_token)
            .bind(if succeeded { "succeeded" } else { "failed" }).execute(&self.pool).await.map_err(database)?;
        Ok(())
    }

    pub(crate) async fn rollout_qualifies(&self) -> Result<bool, AutomationError> {
        let row = sqlx::query(
            "SELECT (SELECT count(*) FROM classification.classification_decisions WHERE confidence_basis_points>=9000 AND state IN ('accepted','corrected','rejected')) AS resolved, \
             (SELECT count(*) FROM classification.classification_decisions WHERE confidence_basis_points>=9000 AND state='accepted') AS accepted, \
             (SELECT count(*) FROM classification.classification_provider_attempts) AS attempts, \
             (SELECT count(*) FROM classification.classification_provider_attempts WHERE outcome IS DISTINCT FROM 'succeeded') AS failures",
        ).fetch_one(&self.pool).await.map_err(database)?;
        Ok(super::rollout::RolloutEvidence {
            high_confidence_resolved: row.get::<i64, _>("resolved") as u64,
            accepted: row.get::<i64, _>("accepted") as u64,
            outbound_attempts: row.get::<i64, _>("attempts") as u64,
            unsuccessful_attempts: row.get::<i64, _>("failures") as u64,
        }
        .qualifies())
    }

    pub(crate) async fn defer_for_quota(
        &self,
        claim: &ClassificationClaim,
        holder: &str,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, AutomationError> {
        let resume_at = next_utc_midnight(now);
        let mut transaction = self.pool.begin().await.map_err(database)?;
        let updated = sqlx::query(
            "UPDATE classification.classification_targets SET state='quota_deferred',next_attempt_at=$6, \
             lease_holder=NULL,lease_expires_at=NULL,updated_at=$7 WHERE id=$1 AND user_id=$2 \
             AND generation=$3 AND lease_holder=$4 AND lease_token=$5 AND lease_expires_at>clock_timestamp()",
        )
        .bind(claim.id.into_uuid())
        .bind(claim.evidence.user_id().into_uuid())
        .bind(claim.generation)
        .bind(holder)
        .bind(claim.lease_token)
        .bind(resume_at)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database)?;
        if updated.rows_affected() != 1 {
            return Err(AutomationError::Fenced);
        }
        if claim.origin == TargetOrigin::Backfill {
            sqlx::query(
                "UPDATE classification.classification_backfill_jobs job SET state='quota_deferred', \
                 next_resume_at=$3,updated_at=$4 FROM classification.classification_targets target \
                 WHERE target.id=$1 AND target.user_id=$2 AND job.id=target.backfill_job_id \
                   AND job.user_id=target.user_id AND job.state IN ('requested','running','quota_deferred')",
            )
            .bind(claim.id.into_uuid())
            .bind(claim.evidence.user_id().into_uuid())
            .bind(resume_at)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(database)?;
        }
        transaction.commit().await.map_err(database)?;
        Ok(resume_at)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn persist_prediction(
        &self,
        claim: &ClassificationClaim,
        holder: &str,
        provider: &str,
        model: &str,
        prediction: &Prediction,
        disposition: PredictionDisposition,
        provider_duration: Duration,
        now: DateTime<Utc>,
    ) -> Result<ClassificationDecisionId, AutomationError> {
        let mut transaction = self.pool.begin().await.map_err(database)?;
        let fenced = sqlx::query(
            "UPDATE classification.classification_targets SET state=$6, \
             next_attempt_at=NULL,last_error_code=NULL,lease_holder=NULL,lease_expires_at=NULL,updated_at=$7 \
             WHERE id=$1 AND user_id=$2 AND generation=$3 AND lease_holder=$4 AND lease_token=$5 \
               AND lease_expires_at>clock_timestamp()",
        )
        .bind(claim.id.into_uuid())
        .bind(claim.evidence.user_id().into_uuid())
        .bind(claim.generation)
        .bind(holder)
        .bind(claim.lease_token)
        .bind(disposition.as_str())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database)?;
        if fenced.rows_affected() != 1 {
            return Err(AutomationError::Fenced);
        }
        let decision_id = ClassificationDecisionId::generate();
        let decision = ClassificationDecision::record(
            decision_id,
            claim.id,
            &claim.evidence,
            claim.generation,
            prediction.clone(),
            disposition,
            now,
        )
        .map_err(|_| AutomationError::Invalid)?;
        sqlx::query(
            "INSERT INTO classification.classification_decisions( \
             id,user_id,target_id,journal_entry_id,generation,candidate_category_id,confidence_basis_points, \
             reason_code,explanation,state,version,taxonomy_version,annotation_version,evidence_digest, \
             taxonomy_digest,provider,model,prompt_version,provider_duration_ms,created_at,updated_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,1,$11,$12,$13,$14,$15,$16,$17,$18,$19,$19)",
        )
        .bind(decision.id().into_uuid())
        .bind(decision.user_id().into_uuid())
        .bind(decision.target_id().into_uuid())
        .bind(decision.journal_entry_id())
        .bind(decision.generation())
        .bind(decision.prediction().category_id())
        .bind(i32::from(decision.prediction().confidence().basis_points()))
        .bind(decision.prediction().reason().as_str())
        .bind(decision.prediction().explanation())
        .bind(decision.state().as_str())
        .bind(decision.taxonomy_version())
        .bind(decision.annotation_version())
        .bind(claim.evidence.evidence_digest().as_slice())
        .bind(claim.evidence.taxonomy_digest().as_slice())
        .bind(provider)
        .bind(model)
        .bind(super::model::PROMPT_VERSION)
        .bind(duration_millis(provider_duration)?.max(0))
        .bind(decision.created_at())
        .execute(&mut *transaction)
        .await
        .map_err(database)?;
        update_backfill_prediction_count(&mut transaction, claim, disposition, now).await?;
        transaction.commit().await.map_err(database)?;
        Ok(decision_id)
    }

    pub(crate) async fn persist_failure(
        &self,
        claim: &ClassificationClaim,
        holder: &str,
        error_code: &'static str,
        retry_at: Option<DateTime<Utc>>,
        terminal: bool,
        now: DateTime<Utc>,
    ) -> Result<(), AutomationError> {
        let state = if terminal { "failed" } else { "retry_due" };
        let mut transaction = self.pool.begin().await.map_err(database)?;
        let updated = sqlx::query(
            "UPDATE classification.classification_targets SET state=$6, \
             next_attempt_at=$7,last_error_code=$8,lease_holder=NULL,lease_expires_at=NULL,updated_at=$9 \
             WHERE id=$1 AND user_id=$2 AND generation=$3 AND lease_holder=$4 AND lease_token=$5 \
               AND lease_expires_at>clock_timestamp()",
        )
        .bind(claim.id.into_uuid())
        .bind(claim.evidence.user_id().into_uuid())
        .bind(claim.generation)
        .bind(holder)
        .bind(claim.lease_token)
        .bind(state)
        .bind(retry_at)
        .bind(error_code)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database)?;
        if updated.rows_affected() != 1 {
            return Err(AutomationError::Fenced);
        }
        if terminal && claim.origin == TargetOrigin::Backfill {
            record_backfill_outcome(&mut transaction, claim, "failed", now).await?;
        }
        transaction.commit().await.map_err(database)
    }

    pub(crate) async fn review_queue(
        &self,
        user_id: UserId,
        cursor: Option<ReviewCursor>,
        limit: u32,
    ) -> Result<ReviewPage, AutomationError> {
        if limit == 0 || limit > 100 {
            return Err(AutomationError::Invalid);
        }
        let rows = sqlx::query(
            "SELECT id,version,journal_entry_id,candidate_category_id,confidence_basis_points,reason_code, \
             explanation,taxonomy_version,annotation_version,created_at FROM classification.classification_decisions \
             WHERE user_id=$1 AND state='review_pending' \
               AND ($2::timestamptz IS NULL OR (created_at,id)>($2,$3)) \
             ORDER BY created_at,id LIMIT $4",
        )
        .bind(user_id.into_uuid())
        .bind(cursor.map(|value| value.created_at))
        .bind(cursor.map(|value| value.decision_id.into_uuid()))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(database)?;
        let items = rows
            .into_iter()
            .map(review_item)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = (items.len() == limit as usize)
            .then(|| items.last())
            .flatten()
            .map(|item| ReviewCursor {
                created_at: item.created_at,
                decision_id: item.decision_id,
            });
        Ok(ReviewPage { items, next_cursor })
    }

    pub(crate) async fn decision_for_transaction(
        &self,
        user_id: UserId,
        journal_entry_id: Uuid,
    ) -> Result<Option<DecisionSuggestion>, AutomationError> {
        let row = sqlx::query(
            "SELECT id,version,journal_entry_id,state,candidate_category_id,chosen_category_id, \
             confidence_basis_points,reason_code,explanation,taxonomy_version,annotation_version,created_at,updated_at \
             FROM classification.classification_decisions WHERE user_id=$1 AND journal_entry_id=$2 \
             ORDER BY generation DESC,created_at DESC LIMIT 1",
        )
        .bind(user_id.into_uuid())
        .bind(journal_entry_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(database)?;
        row.map(decision_suggestion).transpose()
    }

    pub(crate) async fn get_decision(
        &self,
        user_id: UserId,
        decision_id: ClassificationDecisionId,
    ) -> Result<DecisionSuggestion, AutomationError> {
        let row = sqlx::query(
            "SELECT id,version,journal_entry_id,state,candidate_category_id,chosen_category_id, \
             confidence_basis_points,reason_code,explanation,taxonomy_version,annotation_version,created_at,updated_at \
             FROM classification.classification_decisions WHERE user_id=$1 AND id=$2",
        )
        .bind(user_id.into_uuid())
        .bind(decision_id.into_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(database)?
        .ok_or(AutomationError::NotFound)?;
        decision_suggestion(row)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn resolve_review(
        &self,
        user_id: UserId,
        decision_id: ClassificationDecisionId,
        action: ReviewAction,
        category_id: Option<Uuid>,
        expected_decision_version: i64,
        expected_annotation_version: i64,
        key: &IdempotencyKey,
        now: DateTime<Utc>,
    ) -> Result<DecisionSuggestion, AutomationError> {
        if expected_decision_version < 1 || expected_annotation_version < 1 {
            return Err(AutomationError::Invalid);
        }
        let hash = review_resolution_hash(
            decision_id,
            action,
            category_id,
            expected_decision_version,
            expected_annotation_version,
        )?;
        let scope = format!("resolve:{decision_id}");
        let mut transaction = self.pool.begin().await.map_err(database)?;
        command_lock(&mut transaction, user_id, &scope, key).await?;
        if let Some(value) = receipt(&mut transaction, user_id, &scope, key, &hash).await? {
            transaction.rollback().await.map_err(database)?;
            return serde_json::from_value(value).map_err(|_| AutomationError::Invalid);
        }
        // Match the target -> decision lock order used by workers and taxonomy fencing.
        sqlx::query("SELECT id FROM classification.classification_targets WHERE user_id=$1 AND id=(SELECT target_id FROM classification.classification_decisions WHERE user_id=$1 AND id=$2) FOR UPDATE")
            .bind(user_id.into_uuid()).bind(decision_id.into_uuid())
            .fetch_optional(&mut *transaction).await.map_err(database)?
            .ok_or(AutomationError::NotFound)?;
        let mut decision = load_domain_decision(&mut transaction, user_id, decision_id).await?;
        if decision.version() != expected_decision_version
            || decision.annotation_version() != expected_annotation_version
        {
            return Err(AutomationError::Conflict);
        }
        decision
            .begin_review_resolution(action, category_id, expected_decision_version, now)
            .map_err(map_domain_error)?;
        let updated = sqlx::query(
            "UPDATE classification.classification_decisions SET state='applying',resolution_action=$3, \
             chosen_category_id=$4,version=version+1,resolved_at=$5,updated_at=$5 \
             WHERE user_id=$1 AND id=$2 AND state='review_pending' RETURNING target_id",
        )
        .bind(user_id.into_uuid())
        .bind(decision_id.into_uuid())
        .bind(decision.resolution_action().map(ReviewAction::as_str))
        .bind(decision.chosen_category_id())
        .bind(decision.updated_at())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database)?
        .ok_or(AutomationError::Conflict)?;
        let target_updated = sqlx::query(
            "UPDATE classification.classification_targets SET state='applying',updated_at=$3 \
             WHERE user_id=$1 AND id=$2 AND state='review_pending'",
        )
        .bind(user_id.into_uuid())
        .bind(updated.get::<Uuid, _>("target_id"))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database)?;
        if target_updated.rows_affected() != 1 {
            return Err(AutomationError::Conflict);
        }
        let result = load_decision(&mut transaction, user_id, decision_id).await?;
        save_receipt(
            &mut transaction,
            user_id,
            &scope,
            key,
            "resolve_review",
            &hash,
            StatusReceipt::Success(202),
            &serde_json::to_value(&result).map_err(|_| AutomationError::Invalid)?,
            Some(decision_id.into_uuid()),
            Some(result.decision_version),
            now,
        )
        .await?;
        transaction.commit().await.map_err(database)?;
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn replay_review_resolution(
        &self,
        user_id: UserId,
        decision_id: ClassificationDecisionId,
        action: ReviewAction,
        category_id: Option<Uuid>,
        expected_decision_version: i64,
        expected_annotation_version: i64,
        key: &IdempotencyKey,
    ) -> Result<Option<DecisionSuggestion>, AutomationError> {
        if expected_decision_version < 1 || expected_annotation_version < 1 {
            return Err(AutomationError::Invalid);
        }
        let hash = review_resolution_hash(
            decision_id,
            action,
            category_id,
            expected_decision_version,
            expected_annotation_version,
        )?;
        let scope = format!("resolve:{decision_id}");
        let mut transaction = self.pool.begin().await.map_err(database)?;
        let value = receipt(&mut transaction, user_id, &scope, key, &hash).await?;
        transaction.rollback().await.map_err(database)?;
        value
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| AutomationError::Invalid)
    }

    pub(crate) async fn claim_application(
        &self,
        holder: &str,
        lease_ttl: Duration,
    ) -> Result<Option<ApplicationClaim>, AutomationError> {
        validate_holder(holder, lease_ttl)?;
        let mut transaction = self.pool.begin().await.map_err(database)?;
        let row = sqlx::query(
            "SELECT decision.id,decision.version AS decision_version,decision.target_id,decision.generation, \
             decision.user_id,decision.journal_entry_id,decision.candidate_category_id,decision.chosen_category_id, \
             decision.resolution_action,decision.annotation_version,decision.taxonomy_version,decision.created_at, \
             target.lease_token FROM classification.classification_decisions decision \
             JOIN classification.classification_targets target ON target.id=decision.target_id AND target.user_id=decision.user_id \
             WHERE decision.state IN ('auto_apply_pending','applying') \
               AND target.state IN ('auto_apply_pending','applying') \
               AND (target.lease_expires_at IS NULL OR target.lease_expires_at<=clock_timestamp()) \
             ORDER BY CASE WHEN decision.state='applying' THEN 0 ELSE 1 END,decision.updated_at,decision.id \
             FOR UPDATE OF target SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database)?;
        let Some(row) = row else {
            transaction.rollback().await.map_err(database)?;
            return Ok(None);
        };
        let target_id = ClassificationTargetId::new(row.get("target_id"));
        let user_id = UserId::new(row.get("user_id"));
        let lease_token: i64 = sqlx::query_scalar(
            "UPDATE classification.classification_targets SET state='applying',lease_holder=$3, \
             lease_token=lease_token+1,lease_expires_at=clock_timestamp()+($4::bigint*interval '1 millisecond'), \
             updated_at=clock_timestamp() WHERE id=$1 AND user_id=$2 RETURNING lease_token",
        )
        .bind(target_id.into_uuid())
        .bind(user_id.into_uuid())
        .bind(holder)
        .bind(duration_millis(lease_ttl)?)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database)?;
        transaction.commit().await.map_err(database)?;
        let action = row
            .get::<Option<String>, _>("resolution_action")
            .as_deref()
            .map(parse_review_action)
            .transpose()?;
        let candidate: Option<Uuid> = row.get("candidate_category_id");
        let chosen: Option<Uuid> = row.get("chosen_category_id");
        Ok(Some(ApplicationClaim {
            decision_id: ClassificationDecisionId::new(row.get("id")),
            decision_version: row.get("decision_version"),
            target_id,
            generation: row.get("generation"),
            user_id,
            journal_entry_id: row.get("journal_entry_id"),
            category_id: if action.is_some() { chosen } else { candidate },
            action,
            annotation_version: row.get("annotation_version"),
            taxonomy_version: row.get("taxonomy_version"),
            candidate_category_id: candidate,
            occurred_at: row.get("created_at"),
            lease_token,
        }))
    }

    pub(crate) async fn complete_application(
        &self,
        claim: &ApplicationClaim,
        holder: &str,
        now: DateTime<Utc>,
    ) -> Result<(), AutomationError> {
        let mut transaction = self.pool.begin().await.map_err(database)?;
        let updated = sqlx::query(
            "UPDATE classification.classification_targets SET state='completed',lease_holder=NULL,lease_expires_at=NULL,updated_at=$6 \
             WHERE id=$1 AND user_id=$2 AND generation=$3 AND lease_holder=$4 AND lease_token=$5 \
               AND lease_expires_at>clock_timestamp()",
        )
        .bind(claim.target_id.into_uuid())
        .bind(claim.user_id.into_uuid())
        .bind(claim.generation)
        .bind(holder)
        .bind(claim.lease_token)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database)?;
        if updated.rows_affected() != 1 {
            return Err(AutomationError::Fenced);
        }
        let mut decision =
            load_domain_decision(&mut transaction, claim.user_id, claim.decision_id).await?;
        decision
            .complete_resolution(claim.decision_version, now)
            .map_err(map_domain_error)?;
        let decision_updated = sqlx::query(
            "UPDATE classification.classification_decisions SET state=$4,version=version+1,updated_at=$5 \
             WHERE id=$1 AND user_id=$2 AND version=$3 AND state IN ('auto_apply_pending','applying')",
        )
        .bind(claim.decision_id.into_uuid())
        .bind(claim.user_id.into_uuid())
        .bind(claim.decision_version)
        .bind(decision.state().as_str())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database)?;
        if decision_updated.rows_affected() != 1 {
            return Err(AutomationError::Fenced);
        }
        if let Some(action) = claim.action {
            record_resolution_feedback(&mut transaction, claim, action, now).await?;
        }
        transaction.commit().await.map_err(database)
    }

    pub(crate) async fn mark_application_stale(
        &self,
        claim: &ApplicationClaim,
        holder: &str,
        now: DateTime<Utc>,
    ) -> Result<(), AutomationError> {
        finish_application_with_state(self, claim, holder, "stale", now).await
    }

    pub(crate) async fn return_application_to_review(
        &self,
        claim: &ApplicationClaim,
        holder: &str,
        now: DateTime<Utc>,
    ) -> Result<(), AutomationError> {
        if claim.action.is_some() {
            return Err(AutomationError::Invalid);
        }
        finish_application_with_state(self, claim, holder, "review_pending", now).await
    }

    pub(crate) async fn mark_application_failed(
        &self,
        claim: &ApplicationClaim,
        holder: &str,
        now: DateTime<Utc>,
    ) -> Result<(), AutomationError> {
        finish_application_with_state(self, claim, holder, "failed", now).await
    }

    pub(crate) async fn start_backfill(
        &self,
        user_id: UserId,
        range: BackfillRange,
        key: &IdempotencyKey,
        now: DateTime<Utc>,
    ) -> Result<BackfillJobView, AutomationError> {
        let hash: [u8; 32] = Sha256::digest(
            serde_json::to_vec(&json!({"from":range.from(),"to":range.to()}))
                .map_err(|_| AutomationError::Invalid)?,
        )
        .into();
        let scope = "start_backfill";
        let mut transaction = self.pool.begin().await.map_err(database)?;
        command_lock(&mut transaction, user_id, scope, key).await?;
        if let Some(value) = receipt(&mut transaction, user_id, scope, key, &hash).await? {
            transaction.rollback().await.map_err(database)?;
            return serde_json::from_value(value).map_err(|_| AutomationError::Invalid);
        }
        let id = BackfillJobId::generate();
        sqlx::query(
            "INSERT INTO classification.classification_backfill_jobs(id,user_id,range_from,range_to,state,created_at,updated_at) \
             VALUES($1,$2,$3,$4,'requested',$5,$5)",
        )
        .bind(id.into_uuid())
        .bind(user_id.into_uuid())
        .bind(range.from())
        .bind(range.to())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database)?;
        let result = load_backfill(&mut transaction, user_id, id).await?;
        save_receipt(
            &mut transaction,
            user_id,
            scope,
            key,
            "start_backfill",
            &hash,
            StatusReceipt::Success(202),
            &serde_json::to_value(&result).map_err(|_| AutomationError::Invalid)?,
            Some(id.into_uuid()),
            Some(1),
            now,
        )
        .await?;
        transaction.commit().await.map_err(database)?;
        Ok(result)
    }

    pub(crate) async fn get_backfill(
        &self,
        user_id: UserId,
        id: BackfillJobId,
    ) -> Result<BackfillJobView, AutomationError> {
        let mut transaction = self.pool.begin().await.map_err(database)?;
        let result = load_backfill(&mut transaction, user_id, id).await?;
        transaction.commit().await.map_err(database)?;
        Ok(result)
    }

    pub(crate) async fn claim_backfill(
        &self,
        holder: &str,
        lease_ttl: Duration,
    ) -> Result<Option<BackfillClaim>, AutomationError> {
        validate_holder(holder, lease_ttl)?;
        let row = sqlx::query(
            "WITH candidate AS (SELECT id,user_id FROM classification.classification_backfill_jobs \
             WHERE state IN ('requested','running','quota_deferred') \
               AND (next_resume_at IS NULL OR next_resume_at<=clock_timestamp()) \
               AND (lease_expires_at IS NULL OR lease_expires_at<=clock_timestamp()) \
             ORDER BY updated_at,id FOR UPDATE SKIP LOCKED LIMIT 1) \
             UPDATE classification.classification_backfill_jobs job SET state='running',lease_holder=$1, \
               lease_token=job.lease_token+1,lease_expires_at=clock_timestamp()+($2::bigint*interval '1 millisecond'), \
               updated_at=clock_timestamp() FROM candidate WHERE job.id=candidate.id AND job.user_id=candidate.user_id \
             RETURNING job.id,job.user_id,job.range_from,job.range_to,job.cursor_occurred_at, \
               job.cursor_journal_entry_id,job.cursor_ledger_sequence,job.lease_token",
        )
        .bind(holder)
        .bind(duration_millis(lease_ttl)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(database)?;
        row.map(|row| {
            Ok(BackfillClaim {
                id: BackfillJobId::new(row.get("id")),
                user_id: UserId::new(row.get("user_id")),
                range: BackfillRange::new(row.get("range_from"), row.get("range_to"))
                    .map_err(|_| AutomationError::Invalid)?,
                cursor_occurred_at: row.get("cursor_occurred_at"),
                cursor_journal_entry_id: row.get("cursor_journal_entry_id"),
                cursor_ledger_sequence: row.get("cursor_ledger_sequence"),
                lease_token: row.get("lease_token"),
            })
        })
        .transpose()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn advance_backfill(
        &self,
        claim: &BackfillClaim,
        holder: &str,
        cursor_occurred_at: Option<DateTime<Utc>>,
        cursor_journal_entry_id: Option<Uuid>,
        cursor_ledger_sequence: Option<i64>,
        completed: bool,
        now: DateTime<Utc>,
    ) -> Result<(), AutomationError> {
        let updated = sqlx::query(
            "UPDATE classification.classification_backfill_jobs job SET \
               state=CASE \
                 WHEN $5 AND NOT EXISTS (SELECT 1 FROM classification.classification_targets target \
                   WHERE target.backfill_job_id=job.id AND target.user_id=job.user_id \
                     AND target.state IN ('pending','retry_due','quota_deferred')) THEN 'completed' \
                 WHEN $5 AND NOT EXISTS (SELECT 1 FROM classification.classification_targets target \
                   WHERE target.backfill_job_id=job.id AND target.user_id=job.user_id \
                     AND target.state IN ('pending','retry_due')) THEN 'quota_deferred' \
                 ELSE 'running' END, \
               cursor_occurred_at=$6,cursor_journal_entry_id=$7,cursor_ledger_sequence=$8, \
               lease_holder=NULL,lease_expires_at=NULL, \
               next_resume_at=CASE WHEN $5 AND EXISTS (SELECT 1 FROM classification.classification_targets target \
                 WHERE target.backfill_job_id=job.id AND target.user_id=job.user_id \
                   AND target.state IN ('pending','retry_due','quota_deferred')) THEN \
                   CASE WHEN EXISTS (SELECT 1 FROM classification.classification_targets target \
                     WHERE target.backfill_job_id=job.id AND target.user_id=job.user_id \
                       AND target.state IN ('pending','retry_due') \
                       AND (target.next_attempt_at IS NULL OR target.next_attempt_at<=$9)) \
                   THEN $9+interval '1 second' \
                   ELSE (SELECT min(target.next_attempt_at) FROM classification.classification_targets target \
                     WHERE target.backfill_job_id=job.id AND target.user_id=job.user_id \
                       AND target.state IN ('pending','retry_due','quota_deferred')) END \
                 ELSE NULL END, \
               version=version+1,updated_at=$9, \
               completed_at=CASE WHEN $5 AND NOT EXISTS (SELECT 1 FROM classification.classification_targets target \
                 WHERE target.backfill_job_id=job.id AND target.user_id=job.user_id \
                   AND target.state IN ('pending','retry_due','quota_deferred')) THEN $9 ELSE NULL END \
             WHERE job.id=$1 AND job.user_id=$2 AND job.lease_holder=$3 AND job.lease_token=$4 \
               AND job.lease_expires_at>clock_timestamp() AND job.state='running'",
        )
        .bind(claim.id.into_uuid())
        .bind(claim.user_id.into_uuid())
        .bind(holder)
        .bind(claim.lease_token)
        .bind(completed)
        .bind(cursor_occurred_at)
        .bind(cursor_journal_entry_id)
        .bind(cursor_ledger_sequence)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(database)?;
        if updated.rows_affected() != 1 {
            return Err(AutomationError::Fenced);
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct ClassificationAutomationFacade {
    store: ClassificationWorkFacade,
}

impl ClassificationAutomationFacade {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self {
            store: ClassificationWorkFacade::new(pool),
        }
    }

    pub(crate) fn store(&self) -> ClassificationWorkFacade {
        self.store.clone()
    }

    pub(crate) async fn record_consumed_event(
        &self,
        consumer: &str,
        event_id: Uuid,
        event_type: &str,
        sequence: i64,
        payload_digest: &[u8],
    ) -> Result<(), AutomationError> {
        sqlx::query(
            "INSERT INTO classification.classification_consumed_events(consumer_name,event_id,event_type,sequence,payload_digest,processed_at) \
             VALUES($1,$2,$3,$4,$5,clock_timestamp()) ON CONFLICT(consumer_name,event_id) DO NOTHING",
        )
        .bind(consumer)
        .bind(event_id)
        .bind(event_type)
        .bind(sequence)
        .bind(payload_digest)
        .execute(&self.store.pool)
        .await
        .map_err(database)?;
        Ok(())
    }

    pub async fn target_for_transaction(
        &self,
        user_id: UserId,
        journal_entry_id: Uuid,
    ) -> Result<Option<ClassificationTargetStatus>, AutomationError> {
        let row = sqlx::query("SELECT id,state,outbound_attempts,next_attempt_at,last_error_code FROM classification.classification_targets WHERE user_id=$1 AND journal_entry_id=$2")
            .bind(user_id.into_uuid()).bind(journal_entry_id).fetch_optional(&self.store.pool).await.map_err(database)?;
        row.map(|row| {
            Ok(ClassificationTargetStatus {
                target_id: ClassificationTargetId::new(row.get("id")),
                state: super::model::TargetState::parse(row.get::<String, _>("state").as_str())
                    .map_err(|_| AutomationError::Invalid)?,
                outbound_attempts: row.get("outbound_attempts"),
                next_attempt_at: row.get("next_attempt_at"),
                last_error_code: row.get("last_error_code"),
            })
        })
        .transpose()
    }

    pub async fn enqueue(
        &self,
        evidence: &ClassificationEvidence,
        origin: TargetOrigin,
        backfill_job_id: Option<BackfillJobId>,
    ) -> Result<EnqueueOutcome, AutomationError> {
        self.store.enqueue(evidence, origin, backfill_job_id).await
    }

    pub(crate) async fn fence_transaction(
        &self,
        user_id: UserId,
        journal_entry_id: Uuid,
        applied_decision_id: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<(), AutomationError> {
        self.store
            .fence_transaction(user_id, journal_entry_id, applied_decision_id, now)
            .await
    }

    pub(crate) async fn record_manual_feedback(
        &self,
        evidence: &ClassificationEvidence,
        positive_category_id: Option<Uuid>,
        negative_category_id: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<(), AutomationError> {
        self.store
            .record_manual_feedback(evidence, positive_category_id, negative_category_id, now)
            .await
    }

    pub async fn ranked_examples(
        &self,
        user_id: UserId,
        provider: Option<&str>,
        merchant_mcc: Option<u16>,
        limit: usize,
    ) -> Result<Vec<FeedbackExample>, AutomationError> {
        self.store
            .ranked_examples(user_id, provider, merchant_mcc, limit)
            .await
    }

    pub async fn review_queue(
        &self,
        user_id: UserId,
        cursor: Option<ReviewCursor>,
        limit: u32,
    ) -> Result<ReviewPage, AutomationError> {
        self.store.review_queue(user_id, cursor, limit).await
    }

    pub async fn decision_for_transaction(
        &self,
        user_id: UserId,
        journal_entry_id: Uuid,
    ) -> Result<Option<DecisionSuggestion>, AutomationError> {
        self.store
            .decision_for_transaction(user_id, journal_entry_id)
            .await
    }

    pub async fn get_decision(
        &self,
        user_id: UserId,
        decision_id: ClassificationDecisionId,
    ) -> Result<DecisionSuggestion, AutomationError> {
        self.store.get_decision(user_id, decision_id).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn replay_review_resolution(
        &self,
        user_id: UserId,
        decision_id: ClassificationDecisionId,
        action: ReviewAction,
        category_id: Option<Uuid>,
        expected_decision_version: i64,
        expected_annotation_version: i64,
        key: &IdempotencyKey,
    ) -> Result<Option<DecisionSuggestion>, AutomationError> {
        self.store
            .replay_review_resolution(
                user_id,
                decision_id,
                action,
                category_id,
                expected_decision_version,
                expected_annotation_version,
                key,
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn resolve_review(
        &self,
        user_id: UserId,
        decision_id: ClassificationDecisionId,
        action: ReviewAction,
        category_id: Option<Uuid>,
        expected_decision_version: i64,
        expected_annotation_version: i64,
        key: &IdempotencyKey,
        now: DateTime<Utc>,
    ) -> Result<DecisionSuggestion, AutomationError> {
        self.store
            .resolve_review(
                user_id,
                decision_id,
                action,
                category_id,
                expected_decision_version,
                expected_annotation_version,
                key,
                now,
            )
            .await
    }

    pub async fn start_backfill(
        &self,
        user_id: UserId,
        range: BackfillRange,
        key: &IdempotencyKey,
        now: DateTime<Utc>,
    ) -> Result<BackfillJobView, AutomationError> {
        self.store.start_backfill(user_id, range, key, now).await
    }

    pub async fn get_backfill(
        &self,
        user_id: UserId,
        id: BackfillJobId,
    ) -> Result<BackfillJobView, AutomationError> {
        self.store.get_backfill(user_id, id).await
    }
}

fn review_resolution_hash(
    decision_id: ClassificationDecisionId,
    action: ReviewAction,
    category_id: Option<Uuid>,
    expected_decision_version: i64,
    expected_annotation_version: i64,
) -> Result<[u8; 32], AutomationError> {
    Ok(Sha256::digest(
        serde_json::to_vec(&json!({
            "decision_id": decision_id,
            "action": action.as_str(),
            "category_id": category_id,
            "expected_decision_version": expected_decision_version,
            "expected_annotation_version": expected_annotation_version,
        }))
        .map_err(|_| AutomationError::Invalid)?,
    )
    .into())
}

async fn update_backfill_prediction_count(
    transaction: &mut Transaction<'_, Postgres>,
    claim: &ClassificationClaim,
    disposition: PredictionDisposition,
    now: DateTime<Utc>,
) -> Result<(), AutomationError> {
    if claim.origin != TargetOrigin::Backfill {
        return Ok(());
    }
    let outcome = match disposition {
        PredictionDisposition::AutoApplyPending => "classified",
        PredictionDisposition::ReviewPending => "review",
        PredictionDisposition::Abstained => "abstained",
    };
    record_backfill_outcome(transaction, claim, outcome, now).await
}

async fn record_backfill_outcome(
    transaction: &mut Transaction<'_, Postgres>,
    claim: &ClassificationClaim,
    outcome: &str,
    now: DateTime<Utc>,
) -> Result<(), AutomationError> {
    let Some(job_id) = claim.backfill_job_id else {
        return Ok(());
    };
    let user_id = claim.evidence.user_id().into_uuid();
    // Serialize result updates before computing the job snapshot.
    sqlx::query("SELECT id FROM classification.classification_backfill_jobs WHERE id=$1 AND user_id=$2 FOR UPDATE")
        .bind(job_id.into_uuid()).bind(user_id).fetch_one(&mut **transaction).await.map_err(database)?;
    sqlx::query("INSERT INTO classification.classification_backfill_results(job_id,user_id,journal_entry_id,outcome,updated_at) VALUES($1,$2,$3,$4,$5) ON CONFLICT(job_id,user_id,journal_entry_id) DO UPDATE SET outcome=EXCLUDED.outcome,updated_at=EXCLUDED.updated_at")
        .bind(job_id.into_uuid()).bind(user_id).bind(claim.evidence.journal_entry_id()).bind(outcome).bind(now)
        .execute(&mut **transaction).await.map_err(database)?;
    sqlx::query("UPDATE classification.classification_backfill_jobs job SET classified_count=counts.classified,review_count=counts.review,abstained_count=counts.abstained,failed_count=counts.failed,updated_at=$3 FROM (SELECT count(*) FILTER(WHERE outcome='classified') AS classified,count(*) FILTER(WHERE outcome='review') AS review,count(*) FILTER(WHERE outcome='abstained') AS abstained,count(*) FILTER(WHERE outcome='failed') AS failed FROM classification.classification_backfill_results WHERE job_id=$1 AND user_id=$2) counts WHERE job.id=$1 AND job.user_id=$2")
        .bind(job_id.into_uuid()).bind(user_id).bind(now).execute(&mut **transaction).await.map_err(database)?;
    Ok(())
}

async fn finish_application_with_state(
    store: &ClassificationWorkFacade,
    claim: &ApplicationClaim,
    holder: &str,
    state: &'static str,
    now: DateTime<Utc>,
) -> Result<(), AutomationError> {
    let mut transaction = store.pool.begin().await.map_err(database)?;
    let target = sqlx::query(
        "UPDATE classification.classification_targets SET state=$6,lease_holder=NULL,lease_expires_at=NULL,updated_at=$7 \
         WHERE id=$1 AND user_id=$2 AND generation=$3 AND lease_holder=$4 AND lease_token=$5 \
           AND lease_expires_at>clock_timestamp()",
    )
    .bind(claim.target_id.into_uuid())
    .bind(claim.user_id.into_uuid())
    .bind(claim.generation)
    .bind(holder)
    .bind(claim.lease_token)
    .bind(state)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(database)?;
    if target.rows_affected() != 1 {
        return Err(AutomationError::Fenced);
    }
    if state == "stale" {
        let mut decision =
            load_domain_decision(&mut transaction, claim.user_id, claim.decision_id).await?;
        decision
            .mark_stale(claim.decision_version, now)
            .map_err(map_domain_error)?;
    }
    let decision = sqlx::query(
        "UPDATE classification.classification_decisions SET state=$4,version=version+1,updated_at=$5 \
         WHERE id=$1 AND user_id=$2 AND version=$3 AND state IN ('auto_apply_pending','applying')",
    )
    .bind(claim.decision_id.into_uuid())
    .bind(claim.user_id.into_uuid())
    .bind(claim.decision_version)
    .bind(state)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(database)?;
    if decision.rows_affected() != 1 {
        return Err(AutomationError::Fenced);
    }
    transaction.commit().await.map_err(database)
}

async fn record_resolution_feedback(
    transaction: &mut Transaction<'_, Postgres>,
    claim: &ApplicationClaim,
    action: ReviewAction,
    now: DateTime<Utc>,
) -> Result<(), AutomationError> {
    let row = sqlx::query(
        "SELECT evidence,evidence_digest FROM classification.classification_targets \
         WHERE id=$1 AND user_id=$2",
    )
    .bind(claim.target_id.into_uuid())
    .bind(claim.user_id.into_uuid())
    .fetch_one(&mut **transaction)
    .await
    .map_err(database)?;
    let evidence: EvidencePayload =
        serde_json::from_value(row.get("evidence")).map_err(|_| AutomationError::Invalid)?;
    let (positive, negative, source) = match action {
        ReviewAction::Accept => (claim.category_id, None, "accepted"),
        ReviewAction::Correct => (claim.category_id, claim.candidate_category_id, "corrected"),
        ReviewAction::Reject => (None, claim.candidate_category_id, "rejected"),
    };
    sqlx::query(
        "INSERT INTO classification.classification_feedback_examples( \
         id,user_id,journal_entry_id,source,positive_category_id,negative_category_id,description,amount,currency, \
         occurred_at,provider,merchant_mcc,account_label,evidence_digest,created_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(claim.user_id.into_uuid())
    .bind(claim.journal_entry_id)
    .bind(source)
    .bind(positive)
    .bind(negative)
    .bind(evidence.description)
    .bind(evidence.amount)
    .bind(evidence.currency)
    .bind(evidence.occurred_at)
    .bind(evidence.provider)
    .bind(evidence.merchant_mcc.map(i32::from))
    .bind(evidence.account_label)
    .bind(row.get::<Vec<u8>, _>("evidence_digest"))
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(database)?;
    Ok(())
}

fn review_item(row: sqlx::postgres::PgRow) -> Result<ReviewItem, AutomationError> {
    Ok(ReviewItem {
        decision_id: ClassificationDecisionId::new(row.get("id")),
        decision_version: row.get("version"),
        journal_entry_id: row.get("journal_entry_id"),
        candidate_category_id: row
            .get::<Option<Uuid>, _>("candidate_category_id")
            .ok_or(AutomationError::Invalid)?,
        confidence: Confidence::from_basis_points(
            u16::try_from(row.get::<i32, _>("confidence_basis_points"))
                .map_err(|_| AutomationError::Invalid)?,
        )
        .map_err(|_| AutomationError::Invalid)?,
        reason: PredictionReason::parse(row.get::<String, _>("reason_code").as_str())
            .map_err(|_| AutomationError::Invalid)?,
        explanation: row.get("explanation"),
        taxonomy_version: row.get("taxonomy_version"),
        annotation_version: row.get("annotation_version"),
        created_at: row.get("created_at"),
    })
}

fn decision_suggestion(row: sqlx::postgres::PgRow) -> Result<DecisionSuggestion, AutomationError> {
    Ok(DecisionSuggestion {
        decision_id: ClassificationDecisionId::new(row.get("id")),
        decision_version: row.get("version"),
        journal_entry_id: row.get("journal_entry_id"),
        state: super::model::DecisionState::parse(row.get::<String, _>("state").as_str())
            .map_err(|_| AutomationError::Invalid)?,
        candidate_category_id: row.get("candidate_category_id"),
        chosen_category_id: row.get("chosen_category_id"),
        confidence: f64::from(row.get::<i32, _>("confidence_basis_points")) / 10_000.0,
        reason_code: row.get("reason_code"),
        explanation: row.get("explanation"),
        taxonomy_version: row.get("taxonomy_version"),
        annotation_version: row.get("annotation_version"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

fn map_domain_error(error: super::model::AutomationDomainError) -> AutomationError {
    match error {
        super::model::AutomationDomainError::VersionConflict
        | super::model::AutomationDomainError::InvalidTransition => AutomationError::Conflict,
        _ => AutomationError::Invalid,
    }
}

async fn load_domain_decision(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    id: ClassificationDecisionId,
) -> Result<ClassificationDecision, AutomationError> {
    let row = sqlx::query("SELECT * FROM classification.classification_decisions WHERE user_id=$1 AND id=$2 FOR UPDATE")
        .bind(user_id.into_uuid()).bind(id.into_uuid()).fetch_optional(&mut **transaction).await.map_err(database)?
        .ok_or(AutomationError::NotFound)?;
    ClassificationDecision::reconstitute(
        id,
        ClassificationTargetId::new(row.get("target_id")),
        user_id,
        row.get("journal_entry_id"),
        row.get("generation"),
        row.get("candidate_category_id"),
        Confidence::from_basis_points(row.get::<i32, _>("confidence_basis_points") as u16)
            .map_err(map_domain_error)?,
        PredictionReason::parse(row.get::<String, _>("reason_code").as_str())
            .map_err(map_domain_error)?,
        row.get("explanation"),
        super::model::DecisionState::parse(row.get::<String, _>("state").as_str())
            .map_err(map_domain_error)?,
        row.get("version"),
        row.get("taxonomy_version"),
        row.get("annotation_version"),
        row.get("chosen_category_id"),
        row.get::<Option<String>, _>("resolution_action")
            .as_deref()
            .map(parse_review_action)
            .transpose()?,
        row.get("created_at"),
        row.get("updated_at"),
    )
    .map_err(map_domain_error)
}

async fn load_decision(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    id: ClassificationDecisionId,
) -> Result<DecisionSuggestion, AutomationError> {
    let row = sqlx::query(
        "SELECT id,version,journal_entry_id,state,candidate_category_id,chosen_category_id, \
         confidence_basis_points,reason_code,explanation,taxonomy_version,annotation_version,created_at,updated_at \
         FROM classification.classification_decisions WHERE user_id=$1 AND id=$2",
    )
    .bind(user_id.into_uuid())
    .bind(id.into_uuid())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database)?
    .ok_or(AutomationError::NotFound)?;
    decision_suggestion(row)
}

async fn load_backfill(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    id: BackfillJobId,
) -> Result<BackfillJobView, AutomationError> {
    let row = sqlx::query(
        "SELECT id,state,range_from,range_to,cursor_occurred_at,cursor_journal_entry_id,cursor_ledger_sequence, \
         classified_count,review_count,abstained_count,failed_count,next_resume_at,version,created_at,updated_at \
         FROM classification.classification_backfill_jobs WHERE user_id=$1 AND id=$2",
    )
    .bind(user_id.into_uuid())
    .bind(id.into_uuid())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database)?
    .ok_or(AutomationError::NotFound)?;
    Ok(BackfillJobView {
        id: BackfillJobId::new(row.get("id")),
        state: BackfillState::parse(row.get::<String, _>("state").as_str())
            .map_err(|_| AutomationError::Invalid)?,
        range_from: row.get("range_from"),
        range_to: row.get("range_to"),
        cursor_occurred_at: row.get("cursor_occurred_at"),
        cursor_journal_entry_id: row.get("cursor_journal_entry_id"),
        cursor_ledger_sequence: row.get("cursor_ledger_sequence"),
        classified_count: row.get("classified_count"),
        review_count: row.get("review_count"),
        abstained_count: row.get("abstained_count"),
        failed_count: row.get("failed_count"),
        quota_resume_at: row.get("next_resume_at"),
        version: row.get("version"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

async fn command_lock(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    scope: &str,
    key: &IdempotencyKey,
) -> Result<(), AutomationError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("classification:{user_id}:{scope}:{}", key.as_str()))
        .execute(&mut **transaction)
        .await
        .map_err(database)?;
    Ok(())
}

async fn receipt(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    scope: &str,
    key: &IdempotencyKey,
    hash: &[u8; 32],
) -> Result<Option<Value>, AutomationError> {
    let row = sqlx::query(
        "SELECT request_hash,response_body FROM classification.classification_command_receipts \
         WHERE user_id=$1 AND command_scope=$2 AND idempotency_key=$3 FOR UPDATE",
    )
    .bind(user_id.into_uuid())
    .bind(scope)
    .bind(key.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database)?;
    match row {
        Some(row) if row.get::<Vec<u8>, _>("request_hash").as_slice() == hash => {
            Ok(Some(row.get("response_body")))
        }
        Some(_) => Err(AutomationError::Conflict),
        None => Ok(None),
    }
}

enum StatusReceipt {
    Success(i16),
}

#[allow(clippy::too_many_arguments)]
async fn save_receipt(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    scope: &str,
    key: &IdempotencyKey,
    command_name: &str,
    hash: &[u8; 32],
    status: StatusReceipt,
    response: &Value,
    aggregate_id: Option<Uuid>,
    aggregate_version: Option<i64>,
    now: DateTime<Utc>,
) -> Result<(), AutomationError> {
    let StatusReceipt::Success(http_status) = status;
    sqlx::query(
        "INSERT INTO classification.classification_command_receipts( \
         user_id,command_scope,idempotency_key,command_name,request_hash,status,http_status,response_body, \
         aggregate_id,aggregate_version,created_at,completed_at) \
         VALUES($1,$2,$3,$4,$5,'succeeded',$6,$7,$8,$9,$10,$10)",
    )
    .bind(user_id.into_uuid())
    .bind(scope)
    .bind(key.as_str())
    .bind(command_name)
    .bind(hash.as_slice())
    .bind(http_status)
    .bind(response)
    .bind(aggregate_id)
    .bind(aggregate_version)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(database)?;
    Ok(())
}

fn parse_review_action(value: &str) -> Result<ReviewAction, AutomationError> {
    match value {
        "accept" => Ok(ReviewAction::Accept),
        "correct" => Ok(ReviewAction::Correct),
        "reject" => Ok(ReviewAction::Reject),
        _ => Err(AutomationError::Invalid),
    }
}

fn validate_holder(holder: &str, ttl: Duration) -> Result<(), AutomationError> {
    if holder.trim() != holder || holder.is_empty() || holder.len() > 200 || ttl.is_zero() {
        return Err(AutomationError::Invalid);
    }
    Ok(())
}

fn duration_millis(duration: Duration) -> Result<i64, AutomationError> {
    i64::try_from(duration.as_millis()).map_err(|_| AutomationError::Invalid)
}

fn next_utc_midnight(now: DateTime<Utc>) -> DateTime<Utc> {
    let tomorrow = now
        .date_naive()
        .checked_add_days(Days::new(1))
        .expect("a representable current date has a next day");
    tomorrow.and_time(NaiveTime::MIN).and_utc()
}
