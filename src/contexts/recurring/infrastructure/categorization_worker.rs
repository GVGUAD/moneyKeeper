//! Restart-safe Recurring-to-Ledger annotation and compensation worker.

use std::time::Duration;

use chrono::Utc;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    contexts::ledger::public::{
        AnnotationChanges, AnnotationVersion, CategoryReference, JournalEntryId, LedgerFacade,
        UpdateTransactionAnnotation,
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
                lease_token=t.lease_token+1,attempts=t.attempts+1,updated_at=clock_timestamp()
            FROM candidate c,recurring.match_records m
            WHERE t.match_id=c.match_id AND t.user_id=c.user_id AND t.journal_entry_id=c.journal_entry_id
              AND m.id=t.match_id AND m.user_id=t.user_id
            RETURNING t.match_id,t.user_id,t.journal_entry_id,t.state,t.process_generation,
                      t.prior_category_id,t.prior_annotation_version,t.produced_annotation_version,
                      t.lease_token,m.category_id
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
            lease_token: row.get("lease_token"),
            category_id: row.get("category_id"),
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
        let Some(version) = journal.annotation_version else {
            let updated = self
                .finish(&claim, "terminal_no_effect", None, None)
                .await?;
            return Ok(CategorizationReport {
                claimed: true,
                fenced: !updated,
                ..CategorizationReport::default()
            });
        };
        if journal.category_id.map(|id| id.into_uuid()) == Some(category_id) {
            let updated = self
                .finish(&claim, "terminal_no_effect", None, None)
                .await?;
            return Ok(CategorizationReport {
                claimed: true,
                fenced: !updated,
                ..CategorizationReport::default()
            });
        }
        let result = self
            .ledger
            .update_annotation(UpdateTransactionAnnotation {
                user_id: UserId::new(claim.user_id),
                journal_entry_id: JournalEntryId::new(claim.journal_entry_id),
                changes: AnnotationChanges {
                    category: Some(Some(CategoryReference::new(category_id))),
                    ..AnnotationChanges::default()
                },
                expected_version: version,
                idempotency_key: derived_key(&claim, "apply")?,
                correlation_id: CorrelationId::new(claim.match_id),
                occurred_at: Utc::now(),
            })
            .await;
        match result {
            Ok(result) => {
                let updated = self
                    .finish(
                        &claim,
                        "posted",
                        Some(journal.category_id.map(|id| id.into_uuid())),
                        Some((version.get(), result.version.get())),
                    )
                    .await?;
                Ok(CategorizationReport {
                    claimed: true,
                    posted: updated,
                    fenced: !updated,
                    ..CategorizationReport::default()
                })
            }
            Err(error) if error.is_version_conflict() => self.retry(&claim).await,
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
        let journal = match self
            .ledger
            .get_journal(
                UserId::new(claim.user_id),
                JournalEntryId::new(claim.journal_entry_id),
            )
            .await
        {
            Ok(journal) => journal,
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
        let current = journal.annotation_version.map(AnnotationVersion::get);
        if current != claim.produced_annotation_version {
            let updated = self.finish(&claim, "review_required", None, None).await?;
            return Ok(CategorizationReport {
                claimed: true,
                review_required: updated,
                fenced: !updated,
                ..CategorizationReport::default()
            });
        }
        let expected = AnnotationVersion::new(current.ok_or(CategorizationError::Ledger)?)
            .map_err(|_| CategorizationError::Ledger)?;
        let result = self
            .ledger
            .update_annotation(UpdateTransactionAnnotation {
                user_id: UserId::new(claim.user_id),
                journal_entry_id: JournalEntryId::new(claim.journal_entry_id),
                changes: AnnotationChanges {
                    category: Some(claim.prior_category_id.map(CategoryReference::new)),
                    ..AnnotationChanges::default()
                },
                expected_version: expected,
                idempotency_key: derived_key(&claim, "compensate")?,
                correlation_id: CorrelationId::new(claim.match_id),
                occurred_at: Utc::now(),
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
        prior_category: Option<Option<Uuid>>,
        versions: Option<(i64, i64)>,
    ) -> Result<bool, CategorizationError> {
        let updated = sqlx::query(
            r#"
            UPDATE recurring.categorization_targets SET state=$6,
                prior_category_id=COALESCE($7,prior_category_id),
                prior_annotation_version=COALESCE($8,prior_annotation_version),
                produced_annotation_version=COALESCE($9,produced_annotation_version),
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
        .bind(prior_category.flatten())
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

fn derived_key(claim: &TargetClaim, action: &str) -> Result<IdempotencyKey, CategorizationError> {
    IdempotencyKey::new(format!(
        "recurring:{}:{}:{}:{action}",
        claim.match_id, claim.journal_entry_id, claim.generation
    ))
    .map_err(|_| CategorizationError::Configuration)
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
    #[allow(dead_code)]
    prior_annotation_version: Option<i64>,
    produced_annotation_version: Option<i64>,
    lease_token: i64,
    category_id: Option<Uuid>,
}
