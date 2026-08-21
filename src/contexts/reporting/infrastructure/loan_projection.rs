//! Rebuildable Loans event consumer.

use sha2::{Digest, Sha256};
use sqlx::Row;

use super::PgReportingStore;
use crate::contexts::loans::public::{LoanDirection, LoanEventFactV1, LoanEventV1};
use crate::contexts::reporting::public::{LoanSummary, ProjectionApplyResult};

impl PgReportingStore {
    pub(crate) async fn apply_loan_event(
        &self,
        event: LoanEventV1,
    ) -> Result<ProjectionApplyResult, sqlx::Error> {
        if event.metadata.schema_version != 1 {
            return Err(sqlx::Error::Protocol(
                "unknown Loans event major version".into(),
            ));
        }
        let sequence = i64::try_from(event.metadata.sequence)
            .map_err(|_| sqlx::Error::Protocol("Loans sequence exceeds BIGINT".into()))?;
        let digest: Vec<u8> = Sha256::digest(
            serde_json::to_vec(&event).map_err(|e| sqlx::Error::Protocol(e.to_string()))?,
        )
        .to_vec();
        let mut tx = self.pool.begin().await?;
        let inserted=sqlx::query("INSERT INTO reporting.consumed_events(consumer_name,event_id,event_type,source_sequence,payload_digest,processed_at) VALUES('reporting-loans-v1',$1,$2,$3,$4,$5) ON CONFLICT(consumer_name,event_id) DO NOTHING")
            .bind(event.metadata.event_id.into_uuid()).bind(event.event_type()).bind(sequence).bind(&digest).bind(chrono::Utc::now()).execute(&mut *tx).await?;
        if inserted.rows_affected() == 0 {
            let existing:Vec<u8>=sqlx::query_scalar("SELECT payload_digest FROM reporting.consumed_events WHERE consumer_name='reporting-loans-v1' AND event_id=$1")
                .bind(event.metadata.event_id.into_uuid()).fetch_one(&mut *tx).await?;
            if existing != digest {
                return Err(sqlx::Error::Protocol(
                    "Loans event identity conflict".into(),
                ));
            }
            tx.rollback().await?;
            return Ok(ProjectionApplyResult {
                applied: false,
                sequence: event.metadata.sequence,
            });
        }
        let user = event.metadata.user_id.into_uuid();
        match event.fact {
            LoanEventFactV1::AgreementOpened {
                agreement_id,
                direction,
                currency,
                ..
            } => {
                sqlx::query("INSERT INTO reporting.loan_summaries(user_id,loan_id,currency,principal,interest,fees,source_sequence,direction,status) VALUES($1,$2,$3,0,0,0,$4,$5,'pending_accounting') ON CONFLICT(user_id,loan_id) DO NOTHING")
                    .bind(user).bind(agreement_id.into_uuid()).bind(currency.as_str()).bind(sequence).bind(direction_str(direction)).execute(&mut *tx).await?;
            }
            LoanEventFactV1::MovementPosted {
                agreement_id,
                balances,
                ..
            }
            | LoanEventFactV1::MovementReversed {
                agreement_id,
                balances,
                ..
            } => {
                sqlx::query("UPDATE reporting.loan_summaries SET principal=$3,interest=$4,fees=$5,status='active',source_sequence=$6 WHERE user_id=$1 AND loan_id=$2 AND source_sequence<$6")
                    .bind(user).bind(agreement_id.into_uuid()).bind(balances.principal).bind(balances.accrued_interest).bind(balances.accrued_fee).bind(sequence).execute(&mut *tx).await?;
            }
            LoanEventFactV1::AgreementClosed { agreement_id } => {
                sqlx::query("UPDATE reporting.loan_summaries SET status='closed',source_sequence=$3 WHERE user_id=$1 AND loan_id=$2 AND source_sequence<$3")
                .bind(user).bind(agreement_id.into_uuid()).bind(sequence).execute(&mut *tx).await?;
            }
            _ => {}
        }
        sqlx::query("INSERT INTO reporting.checkpoints(consumer_name,last_sequence,updated_at) VALUES('reporting-loans-v1',$1,$2) ON CONFLICT(consumer_name) DO UPDATE SET last_sequence=GREATEST(reporting.checkpoints.last_sequence,EXCLUDED.last_sequence),updated_at=EXCLUDED.updated_at")
            .bind(sequence).bind(chrono::Utc::now()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(ProjectionApplyResult {
            applied: true,
            sequence: event.metadata.sequence,
        })
    }
    pub(crate) async fn loan_summary(
        &self,
        user: crate::shared_kernel::UserId,
        id: crate::contexts::loans::public::LoanAgreementId,
    ) -> Result<Option<LoanSummary>, sqlx::Error> {
        sqlx::query("SELECT currency,principal,interest,fees,direction,status,source_sequence FROM reporting.loan_summaries WHERE user_id=$1 AND loan_id=$2")
            .bind(user.into_uuid()).bind(id.into_uuid()).fetch_optional(&self.pool).await?.map(|row|Ok(LoanSummary{agreement_id:id,currency:crate::shared_kernel::CurrencyCode::new(row.get::<String,_>("currency")).map_err(|_|sqlx::Error::Protocol("stored loan currency is invalid".into()))?,direction:match row.get::<Option<String>,_>("direction").as_deref(){Some("borrowed")=>Some(LoanDirection::Borrowed),Some("lent")=>Some(LoanDirection::Lent),_=>None},principal:row.get("principal"),interest:row.get("interest"),fees:row.get("fees"),status:row.get("status"),source_sequence:u64::try_from(row.get::<i64,_>("source_sequence")).unwrap_or_default()})).transpose()
    }
}
fn direction_str(v: LoanDirection) -> &'static str {
    match v {
        LoanDirection::Borrowed => "borrowed",
        LoanDirection::Lent => "lent",
    }
}
