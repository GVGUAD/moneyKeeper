use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use super::super::{
    domain::{SubscriptionId, SubscriptionStatus},
    public::SubscriptionView,
};
use crate::shared_kernel::UserId;

#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    #[error("recurring item was not found")]
    NotFound,
    #[error("recurring aggregate version conflict")]
    VersionConflict,
    #[error("idempotency key conflicts with an earlier request")]
    IdempotencyConflict,
    #[error("categorization is still pending")]
    CategorizationPending,
    #[error("invalid recurring command: {0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

#[derive(Clone, Debug)]
pub(crate) struct MatchAllocation {
    pub journal_entry_id: Uuid,
    pub amount: Decimal,
    pub currency: String,
}

#[derive(Clone)]
pub(crate) struct PgRecurringStore {
    pool: PgPool,
}

impl PgRecurringStore {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) async fn list_subscriptions(
        &self,
        user: UserId,
    ) -> Result<Vec<SubscriptionView>, sqlx::Error> {
        sqlx::query("SELECT id,merchant,status,version FROM recurring.subscriptions WHERE user_id=$1 ORDER BY merchant,id")
            .bind(user.into_uuid()).fetch_all(&self.pool).await?.into_iter().map(subscription_view).collect()
    }

    pub(crate) async fn get_subscription(
        &self,
        user: UserId,
        id: Uuid,
    ) -> Result<Option<SubscriptionView>, sqlx::Error> {
        sqlx::query("SELECT id,merchant,status,version FROM recurring.subscriptions WHERE user_id=$1 AND id=$2")
            .bind(user.into_uuid()).bind(id).fetch_optional(&self.pool).await?.map(subscription_view).transpose()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn update_subscription(
        &self,
        user: UserId,
        id: Uuid,
        expected: u64,
        status: Option<&str>,
        category_id: Option<Uuid>,
        key: &str,
        hash: [u8; 32],
    ) -> Result<Value, StoreError> {
        if status.is_some_and(|status| !matches!(status, "active" | "paused" | "cancelled")) {
            return Err(StoreError::Invalid("invalid subscription status"));
        }
        let mut tx = self.pool.begin().await?;
        if let Some(response) = claim_receipt(
            &mut tx,
            user,
            "update_subscription",
            key,
            "update_subscription",
            id,
            hash,
        )
        .await?
        {
            tx.commit().await?;
            return Ok(response);
        }
        let row = sqlx::query("UPDATE recurring.subscriptions SET status=COALESCE($4,status),category_id=COALESCE($5,category_id),version=version+1,updated_at=$6 WHERE user_id=$1 AND id=$2 AND version=$3 AND status<>'cancelled' RETURNING version,status,category_id")
            .bind(user.into_uuid()).bind(id).bind(i64::try_from(expected).unwrap_or(i64::MAX)).bind(status).bind(category_id).bind(Utc::now()).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            reject_receipt(
                &mut tx,
                user,
                "update_subscription",
                key,
                "version_conflict",
                409,
            )
            .await?;
            tx.commit().await?;
            return Err(StoreError::VersionConflict);
        };
        let version = row.get::<i64, _>("version");
        let response = json!({"subscription_id":id,"status":row.get::<String,_>("status"),"category_id":row.get::<Option<Uuid>,_>("category_id"),"version":version});
        finish_receipt(
            &mut tx,
            user,
            "update_subscription",
            key,
            200,
            &response,
            id,
            version,
        )
        .await?;
        tx.commit().await?;
        Ok(response)
    }

    pub(crate) async fn charges(
        &self,
        user: UserId,
        subscription_id: Uuid,
    ) -> Result<Vec<Value>, sqlx::Error> {
        sqlx::query("SELECT e.id,e.kind,e.merchant,e.amount,e.currency,e.charged_at,m.version AS matching_version,m.state FROM recurring.charge_evidence e JOIN recurring.charge_matching m ON m.evidence_id=e.id AND m.user_id=e.user_id WHERE e.user_id=$1 AND e.subscription_id=$2 ORDER BY e.charged_at DESC NULLS LAST,e.recorded_at DESC,e.id")
            .bind(user.into_uuid()).bind(subscription_id).fetch_all(&self.pool).await?.into_iter().map(|row| Ok(json!({
                "charge_evidence_id":row.get::<Uuid,_>("id"),"kind":row.get::<String,_>("kind"),"merchant":row.get::<String,_>("merchant"),
                "amount":row.get::<Option<Decimal>,_>("amount").map(|value|value.to_string()),"currency":row.get::<Option<String>,_>("currency"),
                "charged_at":row.get::<Option<DateTime<Utc>>,_>("charged_at"),"matching_version":row.get::<i64,_>("matching_version"),"matching_state":row.get::<String,_>("state")
            }))).collect()
    }

    pub(crate) async fn forecast(&self, user: UserId) -> Result<Vec<Value>, sqlx::Error> {
        sqlx::query("SELECT id,merchant,cadence,expected_amount,currency,next_expected_at,version FROM recurring.subscriptions WHERE user_id=$1 AND status='active' ORDER BY next_expected_at NULLS LAST,merchant,id")
            .bind(user.into_uuid()).fetch_all(&self.pool).await?.into_iter().map(|row| Ok(json!({
                "subscription_id":row.get::<Uuid,_>("id"),"merchant":row.get::<String,_>("merchant"),"cadence":row.get::<String,_>("cadence"),
                "expected_amount":row.get::<Option<Decimal>,_>("expected_amount").map(|value|value.to_string()),"currency":row.get::<Option<String>,_>("currency"),
                "next_expected_at":row.get::<Option<DateTime<Utc>>,_>("next_expected_at"),"version":row.get::<i64,_>("version")
            }))).collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_match(
        &self,
        user: UserId,
        evidence_id: Uuid,
        expected: u64,
        allocations: Vec<MatchAllocation>,
        key: &str,
        hash: [u8; 32],
    ) -> Result<Value, StoreError> {
        if allocations.is_empty() {
            return Err(StoreError::Invalid("allocations are required"));
        }
        let mut tx = self.pool.begin().await?;
        if let Some(response) = claim_receipt(
            &mut tx,
            user,
            "match_charge",
            key,
            "match_charge",
            evidence_id,
            hash,
        )
        .await?
        {
            tx.commit().await?;
            return Ok(response);
        }
        let row=sqlx::query("SELECT e.amount,e.currency,m.version,m.allocated_amount,s.category_id FROM recurring.charge_evidence e JOIN recurring.charge_matching m ON m.evidence_id=e.id AND m.user_id=e.user_id LEFT JOIN recurring.subscriptions s ON s.id=e.subscription_id AND s.user_id=e.user_id WHERE e.user_id=$1 AND e.id=$2 FOR UPDATE OF m")
            .bind(user.into_uuid()).bind(evidence_id).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            reject_receipt(&mut tx, user, "match_charge", key, "not_found", 404).await?;
            tx.commit().await?;
            return Err(StoreError::NotFound);
        };
        let evidence_amount: Decimal = row
            .try_get("amount")
            .map_err(|_| StoreError::Invalid("evidence has no amount"))?;
        let currency: String = row
            .try_get("currency")
            .map_err(|_| StoreError::Invalid("evidence has no currency"))?;
        if u64::try_from(row.get::<i64, _>("version")).unwrap_or(u64::MAX) != expected {
            reject_receipt(&mut tx, user, "match_charge", key, "version_conflict", 409).await?;
            tx.commit().await?;
            return Err(StoreError::VersionConflict);
        }
        let mut added = Decimal::ZERO;
        for allocation in &allocations {
            if allocation.currency != currency {
                return Err(StoreError::Invalid("allocation currency mismatch"));
            }
            if allocation.amount <= Decimal::ZERO {
                return Err(StoreError::Invalid("allocation must be positive"));
            }
            added = added
                .checked_add(allocation.amount)
                .ok_or(StoreError::Invalid("allocation overflow"))?;
        }
        let total = row
            .get::<Decimal, _>("allocated_amount")
            .checked_add(added)
            .ok_or(StoreError::Invalid("allocation overflow"))?;
        if total > evidence_amount {
            return Err(StoreError::Invalid("allocations exceed evidence amount"));
        }
        let match_id = Uuid::new_v4();
        let next = i64::try_from(expected.saturating_add(1)).unwrap_or(i64::MAX);
        let now = Utc::now();
        let category_id: Option<Uuid> = row.get("category_id");
        sqlx::query("INSERT INTO recurring.match_records(id,user_id,evidence_id,matching_version,decision_source,category_id,created_at) VALUES($1,$2,$3,$4,'manual',$5,$6)").bind(match_id).bind(user.into_uuid()).bind(evidence_id).bind(next).bind(category_id).bind(now).execute(&mut *tx).await?;
        for allocation in &allocations {
            sqlx::query("INSERT INTO recurring.match_allocations(match_id,user_id,journal_entry_id,amount,currency) VALUES($1,$2,$3,$4,$5)").bind(match_id).bind(user.into_uuid()).bind(allocation.journal_entry_id).bind(allocation.amount).bind(&allocation.currency).execute(&mut *tx).await?;
            let process_state = if category_id.is_some() {
                "pending"
            } else {
                "terminal_no_effect"
            };
            sqlx::query("INSERT INTO recurring.categorization_targets(match_id,user_id,journal_entry_id,state,process_generation,updated_at) VALUES($1,$2,$3,$4,1,$5)")
                .bind(match_id).bind(user.into_uuid()).bind(allocation.journal_entry_id)
                .bind(process_state).bind(now).execute(&mut *tx).await?;
        }
        let state = if total == evidence_amount {
            "matched"
        } else {
            "partially_matched"
        };
        sqlx::query("UPDATE recurring.charge_matching SET version=$3,allocated_amount=$4,state=$5,updated_at=$6 WHERE evidence_id=$1 AND user_id=$2").bind(evidence_id).bind(user.into_uuid()).bind(next).bind(total).bind(state).bind(now).execute(&mut *tx).await?;
        let process_state = if category_id.is_some() {
            "pending"
        } else {
            "terminal_no_effect"
        };
        sqlx::query("INSERT INTO recurring.categorization_processes(match_id,user_id,state,process_generation,updated_at) VALUES($1,$2,$3,1,$4)").bind(match_id).bind(user.into_uuid()).bind(process_state).bind(now).execute(&mut *tx).await?;
        let payload = json!({"evidence_id":evidence_id,"match_id":match_id,"allocations":allocations.iter().map(|allocation|json!({"journal_entry_id":allocation.journal_entry_id,"amount":allocation.amount.to_string(),"currency":allocation.currency})).collect::<Vec<_>>()});
        append_outbox(
            &mut tx,
            user,
            evidence_id,
            next,
            "recurring.charge-matched.v1",
            match_id,
            payload,
            now,
        )
        .await?;
        let response = json!({"charge_evidence_id":evidence_id,"match_id":match_id,"matching_version":next,"status":"pending"});
        finish_receipt(
            &mut tx,
            user,
            "match_charge",
            key,
            200,
            &response,
            evidence_id,
            next,
        )
        .await?;
        tx.commit().await?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn reject(
        &self,
        user: UserId,
        evidence_id: Uuid,
        expected: u64,
        reason: &str,
        key: &str,
        hash: [u8; 32],
    ) -> Result<Value, StoreError> {
        if reason.trim().is_empty() {
            return Err(StoreError::Invalid("reason is required"));
        }
        let mut tx = self.pool.begin().await?;
        if let Some(response) = claim_receipt(
            &mut tx,
            user,
            "reject_charge",
            key,
            "reject_charge",
            evidence_id,
            hash,
        )
        .await?
        {
            tx.commit().await?;
            return Ok(response);
        }
        let version:Option<i64>=sqlx::query_scalar("SELECT version FROM recurring.charge_matching WHERE evidence_id=$1 AND user_id=$2 FOR UPDATE").bind(evidence_id).bind(user.into_uuid()).fetch_optional(&mut *tx).await?;
        let Some(version) = version else {
            reject_receipt(&mut tx, user, "reject_charge", key, "not_found", 404).await?;
            tx.commit().await?;
            return Err(StoreError::NotFound);
        };
        if u64::try_from(version).unwrap_or(u64::MAX) != expected {
            reject_receipt(&mut tx, user, "reject_charge", key, "version_conflict", 409).await?;
            tx.commit().await?;
            return Err(StoreError::VersionConflict);
        }
        let next = version + 1;
        let rejection_id = Uuid::new_v4();
        let now = Utc::now();
        sqlx::query("INSERT INTO recurring.rejections(id,user_id,evidence_id,matching_version,reason,recorded_at) VALUES($1,$2,$3,$4,$5,$6)").bind(rejection_id).bind(user.into_uuid()).bind(evidence_id).bind(next).bind(reason).bind(now).execute(&mut *tx).await?;
        sqlx::query("UPDATE recurring.charge_matching SET version=$3,state='rejected',updated_at=$4 WHERE evidence_id=$1 AND user_id=$2").bind(evidence_id).bind(user.into_uuid()).bind(next).bind(now).execute(&mut *tx).await?;
        let response = json!({"charge_evidence_id":evidence_id,"rejection_id":rejection_id,"matching_version":next,"status":"rejected"});
        finish_receipt(
            &mut tx,
            user,
            "reject_charge",
            key,
            200,
            &response,
            evidence_id,
            next,
        )
        .await?;
        tx.commit().await?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn unmatch(
        &self,
        user: UserId,
        evidence_id: Uuid,
        match_id: Uuid,
        expected: u64,
        key: &str,
        hash: [u8; 32],
    ) -> Result<Value, StoreError> {
        let mut tx = self.pool.begin().await?;
        if let Some(response) = claim_receipt(
            &mut tx,
            user,
            "unmatch_charge",
            key,
            "unmatch_charge",
            match_id,
            hash,
        )
        .await?
        {
            tx.commit().await?;
            return Ok(response);
        }
        let process_state:Option<String>=sqlx::query_scalar("SELECT state FROM recurring.categorization_processes WHERE match_id=$1 AND user_id=$2 FOR UPDATE").bind(match_id).bind(user.into_uuid()).fetch_optional(&mut *tx).await?;
        let Some(process_state) = process_state else {
            reject_receipt(&mut tx, user, "unmatch_charge", key, "not_found", 404).await?;
            tx.commit().await?;
            return Err(StoreError::NotFound);
        };
        if matches!(process_state.as_str(), "pending" | "retry_due") {
            reject_receipt(
                &mut tx,
                user,
                "unmatch_charge",
                key,
                "categorization_pending",
                409,
            )
            .await?;
            tx.commit().await?;
            return Err(StoreError::CategorizationPending);
        }
        let version:Option<i64>=sqlx::query_scalar("SELECT version FROM recurring.charge_matching WHERE evidence_id=$1 AND user_id=$2 FOR UPDATE").bind(evidence_id).bind(user.into_uuid()).fetch_optional(&mut *tx).await?;
        let Some(version) = version else {
            reject_receipt(&mut tx, user, "unmatch_charge", key, "not_found", 404).await?;
            tx.commit().await?;
            return Err(StoreError::NotFound);
        };
        if u64::try_from(version).unwrap_or(u64::MAX) != expected {
            reject_receipt(
                &mut tx,
                user,
                "unmatch_charge",
                key,
                "version_conflict",
                409,
            )
            .await?;
            tx.commit().await?;
            return Err(StoreError::VersionConflict);
        }
        let next = version + 1;
        let unmatch_id = Uuid::new_v4();
        let now = Utc::now();
        let inserted=sqlx::query("INSERT INTO recurring.unmatches(id,user_id,evidence_id,match_id,matching_version,recorded_at) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(user_id,match_id) DO NOTHING").bind(unmatch_id).bind(user.into_uuid()).bind(evidence_id).bind(match_id).bind(next).bind(now).execute(&mut *tx).await?;
        if inserted.rows_affected() != 1 {
            return Err(StoreError::Invalid("match was already unmatched"));
        }
        let removed:Decimal=sqlx::query_scalar("SELECT COALESCE(sum(amount),0) FROM recurring.match_allocations WHERE match_id=$1 AND user_id=$2").bind(match_id).bind(user.into_uuid()).fetch_one(&mut *tx).await?;
        sqlx::query("UPDATE recurring.charge_matching SET version=$3,allocated_amount=allocated_amount-$4,state=CASE WHEN allocated_amount-$4=0 THEN 'undecided' ELSE 'partially_matched' END,updated_at=$5 WHERE evidence_id=$1 AND user_id=$2").bind(evidence_id).bind(user.into_uuid()).bind(next).bind(removed).bind(now).execute(&mut *tx).await?;
        sqlx::query("UPDATE recurring.categorization_targets SET state=CASE WHEN state='posted' THEN 'compensating' WHEN state='terminal_no_effect' THEN 'compensated' ELSE state END,process_generation=process_generation+1,next_retry_at=NULL,updated_at=$3 WHERE match_id=$1 AND user_id=$2").bind(match_id).bind(user.into_uuid()).bind(now).execute(&mut *tx).await?;
        sqlx::query("UPDATE recurring.categorization_processes SET state=CASE WHEN EXISTS(SELECT 1 FROM recurring.categorization_targets t WHERE t.match_id=$1 AND t.user_id=$2 AND t.state='compensating') THEN 'compensating' WHEN EXISTS(SELECT 1 FROM recurring.categorization_targets t WHERE t.match_id=$1 AND t.user_id=$2 AND t.state='review_required') THEN 'review_required' ELSE 'compensated' END,process_generation=process_generation+1,updated_at=$3 WHERE match_id=$1 AND user_id=$2").bind(match_id).bind(user.into_uuid()).bind(now).execute(&mut *tx).await?;
        let process_state: String = sqlx::query_scalar(
            "SELECT state FROM recurring.categorization_processes WHERE match_id=$1 AND user_id=$2",
        )
        .bind(match_id)
        .bind(user.into_uuid())
        .fetch_one(&mut *tx)
        .await?;
        append_outbox(
            &mut tx,
            user,
            evidence_id,
            next,
            "recurring.charge-unmatched.v1",
            unmatch_id,
            json!({"charge_evidence_id":evidence_id,"match_id":match_id,"unmatch_id":unmatch_id,"matching_version":next,"process_state":process_state}),
            now,
        ).await?;
        let response = json!({"charge_evidence_id":evidence_id,"match_id":match_id,"unmatch_id":unmatch_id,"matching_version":next,"status":process_state});
        finish_receipt(
            &mut tx,
            user,
            "unmatch_charge",
            key,
            200,
            &response,
            evidence_id,
            next,
        )
        .await?;
        tx.commit().await?;
        Ok(response)
    }

    pub(crate) async fn consume_mail_evidence(
        &self,
        event_id: Uuid,
        sequence: u64,
        event: crate::contexts::mail::public::ReceiptEvidenceRecordedV1,
    ) -> Result<super::super::public::ConsumeResult, StoreError> {
        let source_sequence = i64::try_from(sequence)
            .map_err(|_| StoreError::Invalid("event sequence exceeds BIGINT"))?;
        let digest: [u8; 32] = Sha256::digest(
            serde_json::to_vec(&event)
                .map_err(|_| StoreError::Invalid("mail evidence cannot be serialized"))?,
        )
        .into();
        let mut tx = self.pool.begin().await?;
        if !claim_consumed_event(
            &mut tx,
            "recurring-mail-evidence-v1",
            event_id,
            crate::contexts::mail::public::RECEIPT_EVIDENCE_RECORDED_V1,
            source_sequence,
            digest,
        )
        .await?
        {
            return Ok(super::super::public::ConsumeResult {
                applied: false,
                sequence,
            });
        }
        let now = event.recorded_at;
        sqlx::query(
            r#"
            INSERT INTO recurring.subscriptions
                (id,user_id,merchant,status,cadence,expected_amount,currency,next_expected_at,
                 version,created_at,updated_at)
            VALUES($1,$2,$3,'active','monthly',$4,$5,$6,1,$7,$7)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(event.user_id.into_uuid())
        .bind(&event.merchant)
        .bind(
            event
                .money
                .as_ref()
                .map(crate::shared_kernel::Money::amount),
        )
        .bind(event.money.as_ref().map(|money| money.currency().as_str()))
        .bind(
            event
                .charged_at
                .map(|charged| charged + chrono::Duration::days(30)),
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        let subscription_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM recurring.subscriptions WHERE user_id=$1 AND lower(merchant)=lower($2) AND status<>'cancelled' ORDER BY created_at,id LIMIT 1 FOR UPDATE",
        )
        .bind(event.user_id.into_uuid())
        .bind(&event.merchant)
        .fetch_one(&mut *tx)
        .await?;
        let evidence_id = Uuid::new_v4();
        let inserted = sqlx::query(
            r#"
            INSERT INTO recurring.charge_evidence
                (id,user_id,subscription_id,source_context,source_evidence_id,kind,merchant,
                 amount,currency,charged_at,recorded_at)
            VALUES($1,$2,$3,'mail',$4,$5,$6,$7,$8,$9,$10)
            ON CONFLICT(user_id,source_context,source_evidence_id) DO NOTHING
            "#,
        )
        .bind(evidence_id)
        .bind(event.user_id.into_uuid())
        .bind(subscription_id)
        .bind(event.evidence_id.into_uuid())
        .bind(mail_evidence_kind(event.kind))
        .bind(&event.merchant)
        .bind(
            event
                .money
                .as_ref()
                .map(crate::shared_kernel::Money::amount),
        )
        .bind(event.money.as_ref().map(|money| money.currency().as_str()))
        .bind(event.charged_at)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        if inserted.rows_affected() == 1 {
            sqlx::query("INSERT INTO recurring.charge_matching(evidence_id,user_id,version,allocated_amount,state,updated_at) VALUES($1,$2,0,0,'undecided',$3)")
                .bind(evidence_id).bind(event.user_id.into_uuid()).bind(now)
                .execute(&mut *tx).await?;
            append_outbox(
                &mut tx,
                event.user_id,
                evidence_id,
                1,
                "recurring.charge-evidence-recorded.v1",
                event_id,
                serde_json::to_value(super::super::public::ChargeEvidenceRecordedV1 {
                    user_id: event.user_id,
                    charge_evidence_id: super::super::domain::ChargeEvidenceId::new(evidence_id),
                    subscription_id: super::super::domain::SubscriptionId::new(subscription_id),
                    merchant: event.merchant,
                    money: event.money,
                    charged_at: event.charged_at,
                    recorded_at: now,
                })
                .map_err(|_| StoreError::Invalid("charge evidence cannot be serialized"))?,
                now,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(super::super::public::ConsumeResult {
            applied: true,
            sequence,
        })
    }

    pub(crate) async fn consume_ledger_event(
        &self,
        event: crate::contexts::ledger::public::LedgerEventV1,
    ) -> Result<super::super::public::ConsumeResult, StoreError> {
        if event.metadata.schema_version != 1 {
            return Err(StoreError::Invalid("unknown ledger event major version"));
        }
        let sequence = i64::try_from(event.metadata.sequence)
            .map_err(|_| StoreError::Invalid("event sequence exceeds BIGINT"))?;
        let digest: [u8; 32] = Sha256::digest(
            serde_json::to_vec(&event)
                .map_err(|_| StoreError::Invalid("ledger event cannot be serialized"))?,
        )
        .into();
        let mut tx = self.pool.begin().await?;
        if !claim_consumed_event(
            &mut tx,
            "recurring-ledger-candidates-v1",
            event.metadata.event_id.into_uuid(),
            recurring_ledger_event_type(&event.fact),
            sequence,
            digest,
        )
        .await?
        {
            return Ok(super::super::public::ConsumeResult {
                applied: false,
                sequence: event.metadata.sequence,
            });
        }
        match event.fact {
            crate::contexts::ledger::public::LedgerEventFactV1::EntryPosted {
                journal_entry_id,
                effects,
            } => {
                if let Some(effect) = effects.first() {
                    sqlx::query(
                        r#"
                        INSERT INTO recurring.ledger_candidates
                            (journal_entry_id,user_id,event_sequence,amount,currency,occurred_at)
                        VALUES($1,$2,$3,$4,$5,$6)
                        ON CONFLICT(journal_entry_id,user_id) DO UPDATE SET
                            event_sequence=EXCLUDED.event_sequence,amount=EXCLUDED.amount,
                            currency=EXCLUDED.currency,occurred_at=EXCLUDED.occurred_at
                        WHERE recurring.ledger_candidates.event_sequence<EXCLUDED.event_sequence
                        "#,
                    )
                    .bind(journal_entry_id.into_uuid())
                    .bind(event.metadata.user_id.into_uuid())
                    .bind(sequence)
                    .bind(effect.amount.abs())
                    .bind(effect.currency.as_str())
                    .bind(event.metadata.occurred_at)
                    .execute(&mut *tx)
                    .await?;
                }
            }
            crate::contexts::ledger::public::LedgerEventFactV1::EntryReversed {
                original_journal_entry_id,
                ..
            } => {
                sqlx::query("UPDATE recurring.ledger_candidates SET reversed=true,event_sequence=$3 WHERE journal_entry_id=$1 AND user_id=$2 AND event_sequence<$3")
                    .bind(original_journal_entry_id.into_uuid())
                    .bind(event.metadata.user_id.into_uuid()).bind(sequence)
                    .execute(&mut *tx).await?;
            }
            _ => {}
        }
        tx.commit().await?;
        Ok(super::super::public::ConsumeResult {
            applied: true,
            sequence: event.metadata.sequence,
        })
    }
}

fn mail_evidence_kind(kind: crate::contexts::mail::public::ReceiptEvidenceKind) -> &'static str {
    match kind {
        crate::contexts::mail::public::ReceiptEvidenceKind::Renewal => "renewal",
        crate::contexts::mail::public::ReceiptEvidenceKind::OneTime => "one_time",
        crate::contexts::mail::public::ReceiptEvidenceKind::Refund => "refund",
        crate::contexts::mail::public::ReceiptEvidenceKind::Cancellation => "cancellation",
    }
}

fn recurring_ledger_event_type(
    fact: &crate::contexts::ledger::public::LedgerEventFactV1,
) -> &'static str {
    match fact {
        crate::contexts::ledger::public::LedgerEventFactV1::EntryPosted { .. } => {
            "ledger.journal-posted.v1"
        }
        crate::contexts::ledger::public::LedgerEventFactV1::EntryReversed { .. } => {
            "ledger.journal-reversed.v1"
        }
        crate::contexts::ledger::public::LedgerEventFactV1::AnnotationChanged { .. } => {
            "ledger.annotation-changed.v1"
        }
        _ => "ledger.event.v1",
    }
}

async fn claim_consumed_event(
    tx: &mut Transaction<'_, Postgres>,
    consumer: &str,
    event_id: Uuid,
    event_type: &str,
    sequence: i64,
    digest: [u8; 32],
) -> Result<bool, StoreError> {
    let inserted = sqlx::query("INSERT INTO recurring.consumed_events(consumer_name,event_id,event_type,sequence,payload_digest,processed_at) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(consumer_name,event_id) DO NOTHING")
        .bind(consumer).bind(event_id).bind(event_type).bind(sequence)
        .bind(digest.as_slice()).bind(Utc::now()).execute(&mut **tx).await?;
    if inserted.rows_affected() == 1 {
        return Ok(true);
    }
    let existing: Vec<u8> = sqlx::query_scalar("SELECT payload_digest FROM recurring.consumed_events WHERE consumer_name=$1 AND event_id=$2")
        .bind(consumer).bind(event_id).fetch_one(&mut **tx).await?;
    if existing != digest {
        return Err(StoreError::IdempotencyConflict);
    }
    Ok(false)
}

fn subscription_view(row: sqlx::postgres::PgRow) -> Result<SubscriptionView, sqlx::Error> {
    let status = match row.get::<String, _>("status").as_str() {
        "active" => SubscriptionStatus::Active,
        "paused" => SubscriptionStatus::Paused,
        "cancelled" => SubscriptionStatus::Cancelled,
        _ => return Err(sqlx::Error::Protocol("invalid subscription status".into())),
    };
    Ok(SubscriptionView {
        id: SubscriptionId::new(row.get("id")),
        merchant: row.get("merchant"),
        status,
        version: u64::try_from(row.get::<i64, _>("version")).unwrap_or_default(),
    })
}

#[allow(clippy::too_many_arguments)]
async fn claim_receipt(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    scope: &str,
    key: &str,
    command: &str,
    target: Uuid,
    hash: [u8; 32],
) -> Result<Option<Value>, StoreError> {
    let inserted=sqlx::query("INSERT INTO recurring.command_receipts(user_id,command_scope,idempotency_key,command_name,target_id,request_hash,status,created_at) VALUES($1,$2,$3,$4,$5,$6,'processing',$7) ON CONFLICT DO NOTHING").bind(user.into_uuid()).bind(scope).bind(key).bind(command).bind(target).bind(hash.as_slice()).bind(Utc::now()).execute(&mut **tx).await?;
    if inserted.rows_affected() == 1 {
        return Ok(None);
    }
    let row=sqlx::query("SELECT request_hash,status,response_body FROM recurring.command_receipts WHERE user_id=$1 AND command_scope=$2 AND idempotency_key=$3").bind(user.into_uuid()).bind(scope).bind(key).fetch_one(&mut **tx).await?;
    if row.get::<Vec<u8>, _>("request_hash") != hash {
        return Err(StoreError::IdempotencyConflict);
    }
    let status: String = row.get("status");
    if status == "processing" {
        return Err(StoreError::VersionConflict);
    }
    if status == "rejected" {
        let response: Value = row.try_get("response_body")?;
        return Err(match response.get("error").and_then(Value::as_str) {
            Some("not_found") => StoreError::NotFound,
            Some("categorization_pending") => StoreError::CategorizationPending,
            Some("version_conflict") => StoreError::VersionConflict,
            _ => StoreError::Invalid("command was rejected"),
        });
    }
    Ok(row.try_get("response_body")?)
}

async fn reject_receipt(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    scope: &str,
    key: &str,
    code: &str,
    http: i16,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE recurring.command_receipts SET status='rejected',http_status=$4,response_body=$5,completed_at=$6 WHERE user_id=$1 AND command_scope=$2 AND idempotency_key=$3")
        .bind(user.into_uuid()).bind(scope).bind(key).bind(http)
        .bind(json!({"error":code})).bind(Utc::now()).execute(&mut **tx).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn finish_receipt(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    scope: &str,
    key: &str,
    http: i16,
    response: &Value,
    aggregate: Uuid,
    version: i64,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE recurring.command_receipts SET status='succeeded',http_status=$4,response_body=$5,aggregate_id=$6,aggregate_version=$7,completed_at=$8 WHERE user_id=$1 AND command_scope=$2 AND idempotency_key=$3").bind(user.into_uuid()).bind(scope).bind(key).bind(http).bind(response).bind(aggregate).bind(version).bind(Utc::now()).execute(&mut **tx).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn append_outbox(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    aggregate: Uuid,
    version: i64,
    event_type: &str,
    correlation: Uuid,
    payload: Value,
    occurred_at: DateTime<Utc>,
) -> Result<(), StoreError> {
    sqlx::query("INSERT INTO integration.outbox_messages(message_id,event_id,message_schema_version,context_name,aggregate_id,aggregate_version,event_type,user_id,occurred_at,correlation_id,payload) VALUES($1,$2,1,'recurring',$3,$4,$5,$6,$7,$8,$9)").bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(aggregate.to_string()).bind(version).bind(event_type).bind(user.into_uuid()).bind(occurred_at).bind(correlation).bind(payload).execute(&mut **tx).await?;
    Ok(())
}
