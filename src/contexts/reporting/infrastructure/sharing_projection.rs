//! Rebuildable Sharing bill-position projection.

use super::PgReportingStore;
use crate::contexts::reporting::public::ProjectionApplyResult;
use crate::contexts::sharing::public::{SharingEventFactV1, SharingEventV1};
use sha2::{Digest, Sha256};

impl PgReportingStore {
    pub(crate) async fn apply_sharing_event(
        &self,
        event: SharingEventV1,
    ) -> Result<ProjectionApplyResult, sqlx::Error> {
        if event.metadata.schema_version != 1 {
            return Err(sqlx::Error::Protocol(
                "unknown Sharing event major version".into(),
            ));
        }
        let sequence = i64::try_from(event.metadata.sequence)
            .map_err(|_| sqlx::Error::Protocol("Sharing sequence exceeds BIGINT".into()))?;
        let event_id = event.metadata.event_id.into_uuid();
        let bytes =
            serde_json::to_vec(&event).map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let digest = Sha256::digest(bytes).to_vec();
        let mut tx = self.pool.begin().await?;
        let inserted=sqlx::query("INSERT INTO reporting.consumed_events(consumer_name,event_id,event_type,source_sequence,payload_digest,processed_at) VALUES('reporting-sharing-v1',$1,$2,$3,$4,$5) ON CONFLICT(consumer_name,event_id) DO NOTHING")
            .bind(event_id).bind(sharing_event_type(&event.fact)).bind(sequence).bind(digest).bind(event.metadata.recorded_at).execute(&mut *tx).await?;
        if inserted.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(ProjectionApplyResult {
                applied: false,
                sequence: event.metadata.sequence,
            });
        }
        match event.fact {
            SharingEventFactV1::BillPositionChanged { position } => {
                sqlx::query("INSERT INTO reporting.bill_positions(user_id,bill_id,currency,receivable,payable,source_sequence) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(user_id,bill_id) DO UPDATE SET currency=EXCLUDED.currency,receivable=EXCLUDED.receivable,payable=EXCLUDED.payable,source_sequence=EXCLUDED.source_sequence WHERE reporting.bill_positions.source_sequence<EXCLUDED.source_sequence").bind(event.metadata.user_id.into_uuid()).bind(position.bill_id.into_uuid()).bind(position.currency.as_str()).bind(position.receivable).bind(position.payable).bind(sequence).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO reporting.bill_position_history(event_id,user_id,bill_id,revision,state,currency,receivable,payable,occurred_at,source_sequence) VALUES($1,$2,$3,$4,'active',$5,$6,$7,$8,$9) ON CONFLICT(event_id) DO NOTHING")
                    .bind(event_id).bind(event.metadata.user_id.into_uuid()).bind(position.bill_id.into_uuid()).bind(i32::try_from(position.revision).map_err(|_|sqlx::Error::Protocol("Sharing revision exceeds INTEGER".into()))?).bind(position.currency.as_str()).bind(position.receivable).bind(position.payable).bind(event.metadata.occurred_at).bind(sequence).execute(&mut *tx).await?;
            }
            SharingEventFactV1::BillCancelled {
                bill_id,
                revision,
                reason,
                ..
            } => {
                sqlx::query("DELETE FROM reporting.bill_positions WHERE user_id=$1 AND bill_id=$2 AND source_sequence<$3").bind(event.metadata.user_id.into_uuid()).bind(bill_id.into_uuid()).bind(sequence).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO reporting.bill_position_history(event_id,user_id,bill_id,revision,state,cancellation_reason,occurred_at,source_sequence) VALUES($1,$2,$3,$4,'cancelled',$5,$6,$7) ON CONFLICT(event_id) DO NOTHING")
                    .bind(event_id).bind(event.metadata.user_id.into_uuid()).bind(bill_id.into_uuid()).bind(i32::try_from(revision).map_err(|_|sqlx::Error::Protocol("Sharing revision exceeds INTEGER".into()))?).bind(reason).bind(event.metadata.occurred_at).bind(sequence).execute(&mut *tx).await?;
            }
            SharingEventFactV1::SettlementPosted { .. }
            | SharingEventFactV1::SettlementReversed { .. } => {}
        }
        sqlx::query("INSERT INTO reporting.checkpoints(consumer_name,last_sequence,updated_at) VALUES('reporting-sharing-v1',$1,$2) ON CONFLICT(consumer_name) DO UPDATE SET last_sequence=GREATEST(reporting.checkpoints.last_sequence,EXCLUDED.last_sequence),updated_at=EXCLUDED.updated_at").bind(sequence).bind(event.metadata.recorded_at).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(ProjectionApplyResult {
            applied: true,
            sequence: event.metadata.sequence,
        })
    }

    pub(crate) async fn rebuild_sharing(
        &self,
        events: Vec<SharingEventV1>,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM reporting.bill_positions")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM reporting.bill_position_history")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "DELETE FROM reporting.consumed_events WHERE consumer_name='reporting-sharing-v1'",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM reporting.checkpoints WHERE consumer_name='reporting-sharing-v1'")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        for event in events {
            self.apply_sharing_event(event).await?;
        }
        Ok(())
    }
}

fn sharing_event_type(fact: &SharingEventFactV1) -> &'static str {
    match fact {
        SharingEventFactV1::BillPositionChanged { .. } => "sharing.bill-position-changed.v1",
        SharingEventFactV1::SettlementPosted { .. } => "sharing.settlement-posted.v1",
        SharingEventFactV1::SettlementReversed { .. } => "sharing.settlement-reversed.v1",
        SharingEventFactV1::BillCancelled { .. } => "sharing.bill-cancelled.v1",
    }
}
