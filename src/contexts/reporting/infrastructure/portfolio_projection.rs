//! Rebuildable Portfolio event consumer; no foreign-context SQL reads.
use super::PgReportingStore;
use crate::contexts::portfolio::public::{PortfolioEventFactV1, PortfolioEventV1};
use crate::contexts::reporting::public::{PortfolioSummary, ProjectionApplyResult};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use sqlx::Row;

impl PgReportingStore {
    pub(crate) async fn apply_portfolio_event(
        &self,
        event: PortfolioEventV1,
    ) -> Result<ProjectionApplyResult, sqlx::Error> {
        if event.metadata.schema_version != 1 {
            return Err(sqlx::Error::Protocol(
                "unknown Portfolio event major version".into(),
            ));
        }
        let sequence = i64::try_from(event.metadata.sequence)
            .map_err(|_| sqlx::Error::Protocol("Portfolio sequence exceeds BIGINT".into()))?;
        let digest = Sha256::digest(
            serde_json::to_vec(&event).map_err(|e| sqlx::Error::Protocol(e.to_string()))?,
        )
        .to_vec();
        let mut tx = self.pool.begin().await?;
        let inserted=sqlx::query("INSERT INTO reporting.consumed_events(consumer_name,event_id,event_type,source_sequence,payload_digest,processed_at) VALUES('reporting-portfolio-v1',$1,$2,$3,$4,$5) ON CONFLICT(consumer_name,event_id) DO NOTHING").bind(event.metadata.event_id.into_uuid()).bind(event_type(&event.fact)).bind(sequence).bind(digest).bind(event.metadata.recorded_at).execute(&mut *tx).await?;
        if inserted.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(ProjectionApplyResult {
                applied: false,
                sequence: event.metadata.sequence,
            });
        }
        let user = event.metadata.user_id.into_uuid();
        let payload =
            serde_json::to_value(&event.fact).map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
        match &event.fact {
            PortfolioEventFactV1::PositionChanged {
                account_id,
                instrument_id,
                quantity,
                known_cost_quantity,
                unknown_cost_quantity,
                remaining_known_cost,
                realized_gain_loss,
                currency,
                ..
            } => {
                sqlx::query("INSERT INTO reporting.portfolio_positions(user_id,account_id,instrument_id,quantity,known_cost_quantity,unknown_cost_quantity,remaining_known_cost,realized_gain_loss,currency,source_sequence) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(user_id,account_id,instrument_id) DO UPDATE SET quantity=EXCLUDED.quantity,known_cost_quantity=EXCLUDED.known_cost_quantity,unknown_cost_quantity=EXCLUDED.unknown_cost_quantity,remaining_known_cost=EXCLUDED.remaining_known_cost,realized_gain_loss=EXCLUDED.realized_gain_loss,currency=EXCLUDED.currency,source_sequence=EXCLUDED.source_sequence WHERE reporting.portfolio_positions.source_sequence<EXCLUDED.source_sequence").bind(user).bind(account_id.into_uuid()).bind(instrument_id.into_uuid()).bind(quantity).bind(known_cost_quantity).bind(unknown_cost_quantity).bind(remaining_known_cost).bind(realized_gain_loss).bind(currency).bind(sequence).execute(&mut *tx).await?;
            }
            PortfolioEventFactV1::ValuationRecorded {
                account_id,
                instrument_id,
                market_value,
                currency,
                quoted_at,
                ..
            } => {
                sqlx::query("UPDATE reporting.portfolio_positions SET market_value=$4,currency=$5,valuation_as_of=$6,valuation_event_id=$7,source_sequence=GREATEST(source_sequence,$8) WHERE user_id=$1 AND account_id=$2 AND instrument_id=$3 AND (valuation_as_of IS NULL OR (valuation_as_of,COALESCE(valuation_event_id,'00000000-0000-0000-0000-000000000000'::uuid))<($6,$7))").bind(user).bind(account_id.into_uuid()).bind(instrument_id.into_uuid()).bind(market_value).bind(currency).bind(quoted_at).bind(event.metadata.event_id.into_uuid()).bind(sequence).execute(&mut *tx).await?;
            }
            _ => {}
        }
        sqlx::query("INSERT INTO reporting.portfolio_activity_history(event_id,user_id,account_id,instrument_id,transaction_id,event_kind,correlation_id,payload,occurred_at,source_sequence) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)").bind(event.metadata.event_id.into_uuid()).bind(user).bind(account_id(&event.fact)).bind(instrument_id(&event.fact)).bind(transaction_id(&event.fact)).bind(event_type(&event.fact)).bind(event.metadata.correlation_id.into_uuid()).bind(payload).bind(event.metadata.occurred_at).bind(sequence).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO reporting.checkpoints(consumer_name,last_sequence,updated_at) VALUES('reporting-portfolio-v1',$1,$2) ON CONFLICT(consumer_name) DO UPDATE SET last_sequence=GREATEST(reporting.checkpoints.last_sequence,EXCLUDED.last_sequence),updated_at=EXCLUDED.updated_at").bind(sequence).bind(event.metadata.recorded_at).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(ProjectionApplyResult {
            applied: true,
            sequence: event.metadata.sequence,
        })
    }
    pub(crate) async fn portfolio_summary(
        &self,
        user: crate::shared_kernel::UserId,
    ) -> Result<Vec<PortfolioSummary>, sqlx::Error> {
        let rows=sqlx::query("SELECT account_id,instrument_id,quantity,remaining_known_cost,realized_gain_loss,market_value,currency,valuation_as_of,unknown_cost_quantity,source_sequence FROM reporting.portfolio_positions WHERE user_id=$1 ORDER BY account_id,instrument_id").bind(user.into_uuid()).fetch_all(&self.pool).await?;
        rows.iter()
            .map(|r| {
                Ok(PortfolioSummary {
                    account_id: crate::contexts::portfolio::public::PortfolioAccountId::new(
                        r.get("account_id"),
                    ),
                    instrument_id: crate::contexts::portfolio::public::InstrumentId::new(
                        r.get("instrument_id"),
                    ),
                    quantity: r.get("quantity"),
                    remaining_known_cost: r.get("remaining_known_cost"),
                    realized_gain_loss: r.get("realized_gain_loss"),
                    market_value: r.get("market_value"),
                    currency: crate::shared_kernel::CurrencyCode::new(
                        r.get::<String, _>("currency"),
                    )
                    .map_err(|_| sqlx::Error::Protocol("invalid Portfolio currency".into()))?,
                    valuation_as_of: r.get("valuation_as_of"),
                    incomplete: r.get::<Decimal, _>("unknown_cost_quantity")
                        > rust_decimal::Decimal::ZERO
                        || r.get::<Option<Decimal>, _>("market_value").is_none(),
                    source_sequence: u64::try_from(r.get::<i64, _>("source_sequence"))
                        .unwrap_or_default(),
                })
            })
            .collect()
    }
    pub(crate) async fn rebuild_portfolio(
        &self,
        events: Vec<PortfolioEventV1>,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM reporting.portfolio_positions")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM reporting.portfolio_activity_history")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "DELETE FROM reporting.consumed_events WHERE consumer_name='reporting-portfolio-v1'",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM reporting.checkpoints WHERE consumer_name='reporting-portfolio-v1'",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        for event in events {
            self.apply_portfolio_event(event).await?;
        }
        Ok(())
    }
}
fn event_type(f: &PortfolioEventFactV1) -> &'static str {
    match f {
        PortfolioEventFactV1::InstrumentCreated { .. } => "portfolio.instrument-created.v1",
        PortfolioEventFactV1::AccountChanged { .. } => "portfolio.account-changed.v1",
        PortfolioEventFactV1::TransactionPosted { .. } => "portfolio.transaction-posted.v1",
        PortfolioEventFactV1::TransactionReversed { .. } => "portfolio.transaction-reversed.v1",
        PortfolioEventFactV1::PositionChanged { .. } => "portfolio.position-changed.v1",
        PortfolioEventFactV1::ValuationRecorded { .. } => "portfolio.valuation-recorded.v1",
        PortfolioEventFactV1::CashSettlementPosted { .. } => "portfolio.cash-settlement-posted.v1",
        PortfolioEventFactV1::CashSettlementReversed { .. } => {
            "portfolio.cash-settlement-reversed.v1"
        }
        PortfolioEventFactV1::CashSettlementCancelledWithoutEffect { .. } => {
            "portfolio.cash-settlement-cancelled-without-effect.v1"
        }
    }
}
fn account_id(f: &PortfolioEventFactV1) -> Option<uuid::Uuid> {
    match f {
        PortfolioEventFactV1::TransactionPosted { account_id, .. }
        | PortfolioEventFactV1::PositionChanged { account_id, .. }
        | PortfolioEventFactV1::ValuationRecorded { account_id, .. } => {
            Some(account_id.into_uuid())
        }
        PortfolioEventFactV1::AccountChanged { account_id, .. } => Some(account_id.into_uuid()),
        _ => None,
    }
}
fn instrument_id(f: &PortfolioEventFactV1) -> Option<uuid::Uuid> {
    match f {
        PortfolioEventFactV1::TransactionPosted { instrument_id, .. }
        | PortfolioEventFactV1::PositionChanged { instrument_id, .. }
        | PortfolioEventFactV1::ValuationRecorded { instrument_id, .. } => {
            Some(instrument_id.into_uuid())
        }
        PortfolioEventFactV1::InstrumentCreated { instrument_id } => {
            Some(instrument_id.into_uuid())
        }
        _ => None,
    }
}
fn transaction_id(f: &PortfolioEventFactV1) -> Option<uuid::Uuid> {
    match f {
        PortfolioEventFactV1::TransactionPosted { transaction_id, .. }
        | PortfolioEventFactV1::TransactionReversed { transaction_id, .. }
        | PortfolioEventFactV1::CashSettlementPosted { transaction_id, .. }
        | PortfolioEventFactV1::CashSettlementReversed { transaction_id, .. }
        | PortfolioEventFactV1::CashSettlementCancelledWithoutEffect { transaction_id } => {
            Some(transaction_id.into_uuid())
        }
        _ => None,
    }
}
