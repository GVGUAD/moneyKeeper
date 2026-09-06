//! Restart-safe Recurring-to-Ledger annotation and compensation worker.

use std::time::Duration;

use sqlx::{PgPool, Row};
use tracing::Instrument as _;
use uuid::Uuid;

use crate::{
    contexts::classification::public::CategoryId,
    contexts::ledger::public::{
        AnnotationVersion, ApplyCategoryAssignment, AssignmentOrigin, AutomationState,
        CategoryAssignmentDisposition, CategoryAssignmentSnapshot, CategoryReference,
        JournalEntryId, LedgerFacade, RestoreCategoryAssignment,
    },
    shared_kernel::{CorrelationId, IdempotencyKey, UserId},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CategorizationReport {
    pub claimed: bool,
    pub posted: bool,
    pub compensated: bool,
    pub review_required: bool,
    pub retry_scheduled: bool,
    pub fenced: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CategorizationError {
    #[error("categorization persistence failed")]
    Database(#[from] sqlx::Error),
    #[error("categorization configuration is invalid")]
    Configuration,
    #[error("Ledger annotation command failed")]
    Ledger,
}

#[derive(Clone)]
pub(crate) struct CategorizationWorker {
    pool: PgPool,
    ledger: LedgerFacade,
    holder: String,
    lease_ttl: Duration,
}

impl CategorizationWorker {
    pub(crate) fn new(
        pool: PgPool,
        ledger: LedgerFacade,
        holder: impl Into<String>,
        lease_ttl: Duration,
    ) -> Result<Self, CategorizationError> {
        let holder = holder.into();
        if holder.trim() != holder || holder.is_empty() || holder.len() > 200 || lease_ttl.is_zero()
        {
            return Err(CategorizationError::Configuration);
        }
        Ok(Self {
            pool,
            ledger,
            holder,
            lease_ttl,
        })
    }

    pub(crate) async fn run_once(&self) -> Result<CategorizationReport, CategorizationError> {
        let Some(claim) = self.claim().await? else {
            return Ok(CategorizationReport::default());
        };
        let item_span = tracing::info_span!(
            "worker.item",
            operation = "recurring.categorization",
            match_id = %claim.match_id,
            journal_entry_id = %claim.journal_entry_id,
            correlation_id = %claim.match_id,
        );
        item_span.in_scope(|| {
            tracing::info!(
                event.name = "worker.item.claimed",
                outcome = "claimed",
                "Worker item claimed"
            );
        });
        async move {
            match claim.state.as_str() {
                "pending" | "retry_due" => self.apply(claim).await,
                "compensating" => self.compensate(claim).await,
                _ => Ok(CategorizationReport {
                    claimed: true,
                    fenced: true,
                    ..CategorizationReport::default()
                }),
            }
        }
        .instrument(item_span)
        .await
    }

    async fn claim(&self) -> Result<Option<TargetClaim>, CategorizationError> {
        let ttl = i64::try_from(self.lease_ttl.as_millis())
            .map_err(|_| CategorizationError::Configuration)?;
        let row = sqlx::query(
            r#"
            WITH candidate AS (
                SELECT t.match_id,t.user_id,t.journal_entry_id
                FROM recurring.categorization_targets t
                WHERE t.state IN ('pending','retry_due','compensating')
                  AND (t.next_retry_at IS NULL OR t.next_retry_at<=clock_timestamp())
                  AND (t.lease_expires_at IS NULL OR t.lease_expires_at<=clock_timestamp())
                ORDER BY t.updated_at,t.match_id,t.journal_entry_id
                FOR UPDATE SKIP LOCKED LIMIT 1
            )
            UPDATE recurring.categorization_targets t SET
                lease_holder=$1,lease_expires_at=clock_timestamp()+($2::bigint*interval '1 millisecond'),
                lease_token=t.lease_token+1,attempts=t.attempts+1,updated_at=clock_timestamp(),
                apply_command_occurred_at=CASE WHEN t.state<>'compensating'
                  THEN COALESCE(t.apply_command_occurred_at,clock_timestamp())
                  ELSE t.apply_command_occurred_at END,
                compensation_command_occurred_at=CASE WHEN t.state='compensating'
                  THEN COALESCE(t.compensation_command_occurred_at,clock_timestamp())
                  ELSE t.compensation_command_occurred_at END
            FROM candidate c,recurring.match_records m
            WHERE t.match_id=c.match_id AND t.user_id=c.user_id AND t.journal_entry_id=c.journal_entry_id
              AND m.id=t.match_id AND m.user_id=t.user_id
            RETURNING t.match_id,t.user_id,t.journal_entry_id,t.state,t.process_generation,
                      t.prior_category_id,t.prior_annotation_version,t.produced_annotation_version,
                      t.prior_assignment_origin,t.prior_classification_decision_id,
                      t.prior_automation_state,t.lease_token,m.category_id,
                      CASE WHEN t.state='compensating'
                        THEN COALESCE(t.compensation_command_occurred_at,clock_timestamp())
                        ELSE COALESCE(t.apply_command_occurred_at,clock_timestamp()) END AS command_occurred_at
            "#,
        )
        .bind(&self.holder)
        .bind(ttl)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| TargetClaim {
            match_id: row.get("match_id"),
            user_id: row.get("user_id"),
            journal_entry_id: row.get("journal_entry_id"),
            state: row.get("state"),
            generation: row.get("process_generation"),
            prior_category_id: row.get("prior_category_id"),
            prior_annotation_version: row.get("prior_annotation_version"),
            produced_annotation_version: row.get("produced_annotation_version"),
            prior_assignment_origin: row.get("prior_assignment_origin"),
            prior_classification_decision_id: row.get("prior_classification_decision_id"),
            prior_automation_state: row.get("prior_automation_state"),
            lease_token: row.get("lease_token"),
            category_id: row.get("category_id"),
            command_occurred_at: row.get("command_occurred_at"),
        }))
    }

    async fn apply(&self, claim: TargetClaim) -> Result<CategorizationReport, CategorizationError> {
        let Some(category_id) = claim.category_id else {
            let updated = self
                .finish(&claim, "terminal_no_effect", None, None)
                .await?;
            return Ok(CategorizationReport {
                claimed: true,
                fenced: !updated,
                ..CategorizationReport::default()
            });
        };
        let journal = match self
            .ledger
            .get_journal(
                UserId::new(claim.user_id),
                JournalEntryId::new(claim.journal_entry_id),
            )
            .await
        {
            Ok(journal) => journal,
            Err(error) if error.is_not_found() || error.is_invalid_annotation() => {
                let updated = self
                    .finish(&claim, "terminal_no_effect", None, None)
                    .await?;
                return Ok(CategorizationReport {
                    claimed: true,
                    fenced: !updated,
                    ..CategorizationReport::default()
                });
            }
            Err(_) => return self.retry(&claim).await,
        };
        let Some(annotation) = journal.annotation.as_ref() else {
            let updated = self
                .finish(&claim, "terminal_no_effect", None, None)
                .await?;
            return Ok(CategorizationReport {
                claimed: true,
                fenced: !updated,
                ..CategorizationReport::default()
            });
        };
        let mut version = annotation.version;
        let mut prior = CategoryAssignmentSnapshot {
            category: annotation
                .category_id
                .map(|id| CategoryReference::new(id.into_uuid())),
            origin: annotation.assignment_origin,
            classification_decision_id: annotation.classification_decision_id,
            automation_state: annotation.automation_state,
        };
        if let Some(captured_version) = claim.prior_annotation_version {
            version = AnnotationVersion::new(captured_version)
                .map_err(|_| CategorizationError::Ledger)?;
            prior = prior_snapshot(&claim)?;
        } else {
            // Capture the command's complete input before its external Ledger effect.
            // A restart reuses this version and snapshot when replaying its receipt.
            let captured = sqlx::query(
                "UPDATE recurring.categorization_targets SET prior_category_id=$6,prior_annotation_version=$7, \
                 prior_assignment_origin=$8,prior_classification_decision_id=$9,prior_automation_state=$10 \
                 WHERE match_id=$1 AND user_id=$2 AND journal_entry_id=$3 AND lease_holder=$4 AND lease_token=$5 \
                   AND lease_expires_at>clock_timestamp() AND prior_annotation_version IS NULL",
            ).bind(claim.match_id).bind(claim.user_id).bind(claim.journal_entry_id).bind(&self.holder).bind(claim.lease_token)
                .bind(prior.category.map(CategoryReference::into_uuid)).bind(version.get())
                .bind(prior.origin.map(AssignmentOrigin::as_str)).bind(prior.classification_decision_id)
                .bind(prior.automation_state.as_str()).execute(&self.pool).await?;
            if captured.rows_affected() != 1 {
                return Ok(CategorizationReport {
                    claimed: true,
                    fenced: true,
                    ..Default::default()
                });
            }
        }
        let result = self
            .ledger
            .apply_category_assignment(ApplyCategoryAssignment {
                user_id: UserId::new(claim.user_id),
                journal_entry_id: JournalEntryId::new(claim.journal_entry_id),
                category_id: Some(CategoryId::new(category_id)),
                origin: AssignmentOrigin::Recurring,
                classification_decision_id: None,
                expected_version: version,
                idempotency_key: derived_key(&claim, "apply")?,
                correlation_id: CorrelationId::new(claim.match_id),
                occurred_at: claim.command_occurred_at,
            })
            .await;
        match result {
            Ok(result) => {
                let applied = result.disposition == CategoryAssignmentDisposition::Applied;
                let updated = self
                    .finish(
                        &claim,
                        if applied {
                            "posted"
                        } else {
                            "terminal_no_effect"
                        },
                        applied.then_some(prior),
                        applied.then_some((version.get(), result.version.get())),
                    )
                    .await?;
                Ok(CategorizationReport {
                    claimed: true,
                    posted: applied && updated,
                    fenced: !updated,
                    ..CategorizationReport::default()
                })
            }
            Err(error) if error.is_version_conflict() => {
                let updated = self.finish(&claim, "review_required", None, None).await?;
                Ok(CategorizationReport {
                    claimed: true,
                    review_required: updated,
                    fenced: !updated,
                    ..Default::default()
                })
            }
            Err(error) if error.is_not_found() || error.is_invalid_annotation() => {
                let updated = self
                    .finish(&claim, "terminal_no_effect", None, None)
                    .await?;
                Ok(CategorizationReport {
                    claimed: true,
                    fenced: !updated,
                    ..CategorizationReport::default()
                })
            }
            Err(_) => self.retry(&claim).await,
        }
    }

    async fn compensate(
        &self,
        claim: TargetClaim,
    ) -> Result<CategorizationReport, CategorizationError> {
        match self
            .ledger
            .get_journal(
                UserId::new(claim.user_id),
                JournalEntryId::new(claim.journal_entry_id),
            )
            .await
        {
            Ok(_) => {}
            Err(error) if error.is_not_found() => {
                let updated = self.finish(&claim, "review_required", None, None).await?;
                return Ok(CategorizationReport {
                    claimed: true,
                    review_required: updated,
                    fenced: !updated,
                    ..CategorizationReport::default()
                });
            }
            Err(_) => return self.retry(&claim).await,
        };
        let expected = AnnotationVersion::new(
            claim
                .produced_annotation_version
                .ok_or(CategorizationError::Ledger)?,
        )
        .map_err(|_| CategorizationError::Ledger)?;
        let snapshot = prior_snapshot(&claim)?;
        let result = self
            .ledger
            .restore_category_assignment(RestoreCategoryAssignment {
                user_id: UserId::new(claim.user_id),
                journal_entry_id: JournalEntryId::new(claim.journal_entry_id),
                snapshot,
                expected_version: expected,
                idempotency_key: derived_key(&claim, "compensate")?,
                correlation_id: CorrelationId::new(claim.match_id),
                occurred_at: claim.command_occurred_at,
            })
            .await;
        match result {
            Ok(_) => {
                let updated = self.finish(&claim, "compensated", None, None).await?;
                Ok(CategorizationReport {
                    claimed: true,
                    compensated: updated,
                    fenced: !updated,
                    ..CategorizationReport::default()
                })
            }
            Err(error) if error.is_version_conflict() => {
                let updated = self.finish(&claim, "review_required", None, None).await?;
                Ok(CategorizationReport {
                    claimed: true,
                    review_required: updated,
                    fenced: !updated,
                    ..CategorizationReport::default()
                })
            }
            Err(_) => self.retry(&claim).await,
        }
    }

    async fn finish(
        &self,
        claim: &TargetClaim,
        state: &str,
        prior_assignment: Option<CategoryAssignmentSnapshot>,
        versions: Option<(i64, i64)>,
    ) -> Result<bool, CategorizationError> {
        let updated = sqlx::query(
            r#"
            UPDATE recurring.categorization_targets SET state=$6,
                prior_category_id=COALESCE($7,prior_category_id),
                prior_assignment_origin=COALESCE($8,prior_assignment_origin),
                prior_classification_decision_id=COALESCE($9,prior_classification_decision_id),
                prior_automation_state=COALESCE($10,prior_automation_state),
                prior_annotation_version=COALESCE($11,prior_annotation_version),
                produced_annotation_version=COALESCE($12,produced_annotation_version),
                lease_holder=NULL,lease_expires_at=NULL,next_retry_at=NULL,last_error=NULL,
                updated_at=clock_timestamp()
            WHERE match_id=$1 AND user_id=$2 AND journal_entry_id=$3
              AND lease_holder=$4 AND lease_token=$5 AND lease_expires_at>clock_timestamp()
            "#,
        )
        .bind(claim.match_id)
        .bind(claim.user_id)
        .bind(claim.journal_entry_id)
        .bind(&self.holder)
        .bind(claim.lease_token)
        .bind(state)
        .bind(
            prior_assignment
                .and_then(|snapshot| snapshot.category)
                .map(CategoryReference::into_uuid),
        )
        .bind(
            prior_assignment
                .and_then(|snapshot| snapshot.origin)
                .map(AssignmentOrigin::as_str),
        )
        .bind(prior_assignment.and_then(|snapshot| snapshot.classification_decision_id))
        .bind(prior_assignment.map(|snapshot| snapshot.automation_state.as_str()))
        .bind(versions.map(|value| value.0))
        .bind(versions.map(|value| value.1))
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() == 1 {
            refresh_process(&self.pool, claim.match_id, claim.user_id).await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn retry(
        &self,
        claim: &TargetClaim,
    ) -> Result<CategorizationReport, CategorizationError> {
        let updated = sqlx::query(
            r#"
            UPDATE recurring.categorization_targets SET
                state=CASE WHEN attempts>=10 THEN 'review_required' ELSE 'retry_due' END,
                next_retry_at=CASE WHEN attempts>=10 THEN NULL ELSE clock_timestamp()+
                  (LEAST(3600,CAST(power(2,LEAST(attempts,11)) AS BIGINT))*interval '1 second') END,
                last_error='Ledger annotation failed; details redacted',lease_holder=NULL,
                lease_expires_at=NULL,updated_at=clock_timestamp()
            WHERE match_id=$1 AND user_id=$2 AND journal_entry_id=$3
              AND lease_holder=$4 AND lease_token=$5 AND lease_expires_at>clock_timestamp()
            "#,
        )
        .bind(claim.match_id)
        .bind(claim.user_id)
        .bind(claim.journal_entry_id)
        .bind(&self.holder)
        .bind(claim.lease_token)
        .execute(&self.pool)
        .await?;
        let applied = updated.rows_affected() == 1;
        if applied {
            refresh_process(&self.pool, claim.match_id, claim.user_id).await?;
        }
        Ok(CategorizationReport {
            claimed: true,
            retry_scheduled: applied,
            fenced: !applied,
            ..CategorizationReport::default()
        })
    }
}

fn prior_snapshot(claim: &TargetClaim) -> Result<CategoryAssignmentSnapshot, CategorizationError> {
    Ok(CategoryAssignmentSnapshot {
        category: claim.prior_category_id.map(CategoryReference::new),
        origin: claim
            .prior_assignment_origin
            .as_deref()
            .map(parse_origin)
            .transpose()?,
        classification_decision_id: claim.prior_classification_decision_id,
        automation_state: claim
            .prior_automation_state
            .as_deref()
            .map(parse_automation_state)
            .transpose()?
            .unwrap_or(AutomationState::LegacyUnknown),
    })
}

fn derived_key(claim: &TargetClaim, action: &str) -> Result<IdempotencyKey, CategorizationError> {
    IdempotencyKey::new(format!(
        "recurring:{}:{}:{}:{action}",
        claim.match_id, claim.journal_entry_id, claim.generation
    ))
    .map_err(|_| CategorizationError::Configuration)
}

fn parse_origin(value: &str) -> Result<AssignmentOrigin, CategorizationError> {
    match value {
        "manual" => Ok(AssignmentOrigin::Manual),
        "recurring" => Ok(AssignmentOrigin::Recurring),
        "ai" => Ok(AssignmentOrigin::Ai),
        _ => Err(CategorizationError::Ledger),
    }
}

fn parse_automation_state(value: &str) -> Result<AutomationState, CategorizationError> {
    match value {
        "eligible" => Ok(AutomationState::Eligible),
        "suppressed" => Ok(AutomationState::Suppressed),
        "legacy_unknown" => Ok(AutomationState::LegacyUnknown),
        _ => Err(CategorizationError::Ledger),
    }
}

async fn refresh_process(pool: &PgPool, match_id: Uuid, user_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE recurring.categorization_processes p SET state=CASE
          WHEN EXISTS(SELECT 1 FROM recurring.categorization_targets t WHERE t.match_id=$1 AND t.user_id=$2 AND t.state='review_required') THEN 'review_required'
          WHEN EXISTS(SELECT 1 FROM recurring.categorization_targets t WHERE t.match_id=$1 AND t.user_id=$2 AND t.state='compensating') THEN 'compensating'
          WHEN EXISTS(SELECT 1 FROM recurring.categorization_targets t WHERE t.match_id=$1 AND t.user_id=$2 AND t.state IN ('pending','retry_due')) THEN 'retry_due'
          WHEN EXISTS(SELECT 1 FROM recurring.categorization_targets t WHERE t.match_id=$1 AND t.user_id=$2 AND t.state='posted') THEN 'posted'
          WHEN EXISTS(SELECT 1 FROM recurring.categorization_targets t WHERE t.match_id=$1 AND t.user_id=$2 AND t.state='compensated') THEN 'compensated'
          ELSE 'terminal_no_effect' END,updated_at=clock_timestamp()
        WHERE p.match_id=$1 AND p.user_id=$2
        "#,
    )
    .bind(match_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

struct TargetClaim {
    match_id: Uuid,
    user_id: Uuid,
    journal_entry_id: Uuid,
    state: String,
    generation: i64,
    prior_category_id: Option<Uuid>,
    prior_annotation_version: Option<i64>,
    produced_annotation_version: Option<i64>,
    prior_assignment_origin: Option<String>,
    prior_classification_decision_id: Option<Uuid>,
    prior_automation_state: Option<String>,
    lease_token: i64,
    category_id: Option<Uuid>,
    command_occurred_at: chrono::DateTime<chrono::Utc>,
}
