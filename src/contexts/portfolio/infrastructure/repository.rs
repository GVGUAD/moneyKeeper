//! PostgreSQL Portfolio aggregate store and atomic command receipt/outbox unit of work.

use crate::{
    contexts::portfolio::{
        application::{commands::*, queries::*},
        domain::*,
        public::*,
    },
    shared_kernel::{CorrelationId, CurrencyCode, EventId, Money, UserId},
};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    #[error("portfolio fact was not found")]
    NotFound,
    #[error("portfolio version conflict")]
    VersionConflict,
    #[error("portfolio idempotency conflict")]
    IdempotencyConflict,
    #[error("invalid portfolio command: {0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

#[derive(Clone)]
pub(crate) struct PgPortfolioStore {
    pool: PgPool,
}
impl PgPortfolioStore {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) async fn create_instrument(
        &self,
        c: CreateManualOvdpInstrument,
        hash: [u8; 32],
    ) -> Result<PortfolioCommandResult, StoreError> {
        Instrument::manual_ovdp(
            c.user_id,
            c.identifier.clone(),
            c.display_name.clone(),
            c.currency.clone(),
            c.face_value,
            c.issue_date,
            c.maturity_date,
            c.coupon_terms.clone(),
            c.occurred_at,
        )
        .map_err(domain)?;
        let mut tx = self.pool.begin().await?;
        if let Some(v) = claim(
            &mut tx,
            c.user_id,
            "create_manual_ovdp",
            c.idempotency_key.as_str(),
            hash,
        )
        .await?
        {
            tx.commit().await?;
            return replay(v);
        }
        let id = InstrumentId::generate();
        let (coupon_kind, coupon_rate) = coupon_db(&c.coupon_terms);
        sqlx::query("INSERT INTO portfolio.instruments(id,user_id,identifier_kind,identifier,instrument_type,issuer_type,display_name,currency,face_value,issue_date,maturity_date,coupon_kind,coupon_rate,source,version,created_at,updated_at) VALUES($1,$2,$3,$4,'ovdp','sovereign_bond',$5,$6,$7,$8,$9,$10,$11,'manual',1,$12,$12)")
            .bind(id.into_uuid()).bind(c.user_id.into_uuid()).bind(identifier_kind(c.identifier.kind)).bind(&c.identifier.value).bind(&c.display_name).bind(c.currency.as_str()).bind(c.face_value).bind(c.issue_date).bind(c.maturity_date).bind(coupon_kind).bind(coupon_rate).bind(c.occurred_at).execute(&mut *tx).await.map_err(map_unique)?;
        let result = result(
            id.into_uuid(),
            None,
            1,
            "created",
            CashAccountingStatus::NotRequested,
            c.correlation_id,
        );
        append_event(
            &mut tx,
            c.user_id,
            id.to_string(),
            1,
            INSTRUMENT_CREATED_V1,
            c.correlation_id,
            c.occurred_at,
            json!({"instrument_id":id}),
        )
        .await?;
        audit(
            &mut tx,
            c.user_id,
            "instrument",
            id.into_uuid(),
            1,
            "created",
            c.correlation_id,
            c.occurred_at,
        )
        .await?;
        finish(
            &mut tx,
            c.user_id,
            "create_manual_ovdp",
            c.idempotency_key.as_str(),
            201,
            &result,
            id.into_uuid(),
            1,
            c.occurred_at,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn open_account(
        &self,
        c: OpenPortfolioAccount,
        hash: [u8; 32],
    ) -> Result<PortfolioCommandResult, StoreError> {
        PortfolioAccount::open(c.user_id, c.name.clone(), c.occurred_at).map_err(domain)?;
        let mut tx = self.pool.begin().await?;
        if let Some(v) = claim(
            &mut tx,
            c.user_id,
            "open_portfolio_account",
            c.idempotency_key.as_str(),
            hash,
        )
        .await?
        {
            tx.commit().await?;
            return replay(v);
        }
        let id = PortfolioAccountId::generate();
        sqlx::query("INSERT INTO portfolio.accounts(id,user_id,name,lifecycle,version,created_at,updated_at) VALUES($1,$2,$3,'active',1,$4,$4)").bind(id.into_uuid()).bind(c.user_id.into_uuid()).bind(&c.name).bind(c.occurred_at).execute(&mut *tx).await?;
        let result = result(
            id.into_uuid(),
            None,
            1,
            "active",
            CashAccountingStatus::NotRequested,
            c.correlation_id,
        );
        append_event(
            &mut tx,
            c.user_id,
            id.to_string(),
            1,
            ACCOUNT_CHANGED_V1,
            c.correlation_id,
            c.occurred_at,
            json!({"account_id":id,"lifecycle":"active"}),
        )
        .await?;
        audit(
            &mut tx,
            c.user_id,
            "account",
            id.into_uuid(),
            1,
            "opened",
            c.correlation_id,
            c.occurred_at,
        )
        .await?;
        finish(
            &mut tx,
            c.user_id,
            "open_portfolio_account",
            c.idempotency_key.as_str(),
            201,
            &result,
            id.into_uuid(),
            1,
            c.occurred_at,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn change_account(
        &self,
        c: ChangePortfolioAccount,
        scope: &'static str,
        lifecycle: Option<AccountLifecycle>,
        hash: [u8; 32],
    ) -> Result<PortfolioCommandResult, StoreError> {
        let mut tx = self.pool.begin().await?;
        if let Some(v) = claim(&mut tx, c.user_id, scope, c.idempotency_key.as_str(), hash).await? {
            tx.commit().await?;
            return replay(v);
        }
        let row=sqlx::query("SELECT name,lifecycle,version,created_at,updated_at FROM portfolio.accounts WHERE id=$1 AND user_id=$2 FOR UPDATE").bind(c.account_id.into_uuid()).bind(c.user_id.into_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::NotFound)?;
        let version: u64 = row
            .get::<i64, _>("version")
            .try_into()
            .map_err(|_| StoreError::Invalid("version"))?;
        if version != c.expected_version {
            return Err(StoreError::VersionConflict);
        }
        let name = c.name.clone().unwrap_or_else(|| row.get("name"));
        if name.is_empty() || name.trim() != name {
            return Err(StoreError::Invalid("name"));
        }
        let old = row.get::<String, _>("lifecycle");
        let life = lifecycle.map(lifecycle_db).unwrap_or(&old);
        if (scope == "archive_portfolio_account" && old != "active")
            || (scope == "restore_portfolio_account" && old != "archived")
        {
            return Err(StoreError::Invalid("lifecycle"));
        }
        let next = version + 1;
        sqlx::query("UPDATE portfolio.accounts SET name=$3,lifecycle=$4,version=$5,updated_at=$6 WHERE id=$1 AND user_id=$2").bind(c.account_id.into_uuid()).bind(c.user_id.into_uuid()).bind(name).bind(life).bind(i64::try_from(next).unwrap()).bind(c.occurred_at).execute(&mut *tx).await?;
        let result = result(
            c.account_id.into_uuid(),
            None,
            next,
            life,
            CashAccountingStatus::NotRequested,
            c.correlation_id,
        );
        append_event(
            &mut tx,
            c.user_id,
            c.account_id.to_string(),
            next,
            ACCOUNT_CHANGED_V1,
            c.correlation_id,
            c.occurred_at,
            json!({"account_id":c.account_id,"lifecycle":life}),
        )
        .await?;
        audit(
            &mut tx,
            c.user_id,
            "account",
            c.account_id.into_uuid(),
            next,
            scope,
            c.correlation_id,
            c.occurred_at,
        )
        .await?;
        finish(
            &mut tx,
            c.user_id,
            scope,
            c.idempotency_key.as_str(),
            200,
            &result,
            c.account_id.into_uuid(),
            next,
            c.occurred_at,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn record(
        &self,
        c: RecordPortfolioTransaction,
        hash: [u8; 32],
    ) -> Result<PortfolioCommandResult, StoreError> {
        let mut tx = self.pool.begin().await?;
        if let Some(v) = claim(
            &mut tx,
            c.user_id,
            "record_portfolio_transaction",
            c.idempotency_key.as_str(),
            hash,
        )
        .await?
        {
            tx.commit().await?;
            return replay(v);
        }
        let account=sqlx::query("SELECT lifecycle,version FROM portfolio.accounts WHERE id=$1 AND user_id=$2 FOR UPDATE").bind(c.account_id.into_uuid()).bind(c.user_id.into_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::NotFound)?;
        if account.get::<i64, _>("version")
            != i64::try_from(c.expected_account_version).unwrap_or(i64::MAX)
        {
            return Err(StoreError::VersionConflict);
        }
        if account.get::<String, _>("lifecycle") != "active" {
            return Err(StoreError::Invalid("account_archived"));
        }
        let instrument =
            sqlx::query("SELECT currency FROM portfolio.instruments WHERE id=$1 AND user_id=$2")
                .bind(c.instrument_id.into_uuid())
                .bind(c.user_id.into_uuid())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::NotFound)?;
        let currency = CurrencyCode::new(instrument.get::<String, _>("currency"))
            .map_err(|_| StoreError::Invalid("currency"))?;
        sqlx::query("INSERT INTO portfolio.position_projection(user_id,account_id,instrument_id,quantity,known_cost_quantity,unknown_cost_quantity,remaining_known_cost,currency,version,updated_at) VALUES($1,$2,$3,0,0,0,0,$4,0,$5) ON CONFLICT DO NOTHING")
            .bind(c.user_id.into_uuid()).bind(c.account_id.into_uuid()).bind(c.instrument_id.into_uuid()).bind(currency.as_str()).bind(c.recorded_at).execute(&mut *tx).await?;
        let projection=sqlx::query("SELECT quantity,realized_proceeds,realized_allocated_cost,realized_fees,realized_gain_loss,version,source_sequence FROM portfolio.position_projection WHERE user_id=$1 AND account_id=$2 AND instrument_id=$3 FOR UPDATE")
            .bind(c.user_id.into_uuid()).bind(c.account_id.into_uuid()).bind(c.instrument_id.into_uuid()).fetch_one(&mut *tx).await?;
        let version: u64 = projection
            .get::<i64, _>("version")
            .try_into()
            .map_err(|_| StoreError::Invalid("version"))?;
        if version != c.expected_position_version {
            return Err(StoreError::VersionConflict);
        }
        let transaction_id = PortfolioTransactionId::generate();
        let sequence = projection.get::<i64, _>("source_sequence") + 1;
        let prepared = prepare_activity(&c, &currency)?;
        let next_quantity = projection
            .get::<Decimal, _>("quantity")
            .checked_add(prepared.quantity_effect)
            .ok_or(StoreError::Invalid("arithmetic"))?;
        if next_quantity < Decimal::ZERO {
            return Err(StoreError::Invalid("insufficient_quantity"));
        }
        sqlx::query("INSERT INTO portfolio.transactions(id,user_id,account_id,instrument_id,sequence,position_version,kind,status,quantity,currency,source,reason,actor_id,correlation_id,effective_at,recorded_at) VALUES($1,$2,$3,$4,$5,$6,$7,'posted',$8,$9,$10,$11,$12,$13,$14,$15)")
            .bind(transaction_id.into_uuid()).bind(c.user_id.into_uuid()).bind(c.account_id.into_uuid()).bind(c.instrument_id.into_uuid()).bind(sequence).bind(i64::try_from(version+1).unwrap()).bind(prepared.kind).bind(prepared.stored_quantity).bind(currency.as_str()).bind(prepared.source).bind(&prepared.reason).bind(c.actor_id.into_uuid()).bind(c.correlation_id.into_uuid()).bind(prepared.effective_at).bind(c.recorded_at).execute(&mut *tx).await?;
        let mut lots = load_lots(&mut tx, &c).await?;
        let mut allocated_cost = Decimal::ZERO;
        let mut unknown_disposal = false;
        if prepared.quantity_effect < Decimal::ZERO {
            let quantity = -prepared.quantity_effect;
            let allocation = if let Some(requested) = prepared.explicit_allocations.as_deref() {
                let requested = requested
                    .iter()
                    .map(|r| ExplicitLotAllocation {
                        lot_id: r.lot_id,
                        quantity: r.quantity,
                    })
                    .collect::<Vec<_>>();
                allocate_explicit(&lots, quantity, &requested)
            } else {
                allocate_fifo(&lots, quantity)
            }
            .map_err(domain)?;
            allocated_cost = allocation.allocated_known_cost;
            unknown_disposal = allocation.contains_unknown_cost;
            persist_lot_changes(
                &mut tx,
                c.user_id,
                transaction_id,
                &allocation,
                c.recorded_at,
                &currency,
            )
            .await?;
            lots = allocation.lots;
        } else if prepared.quantity_effect > Decimal::ZERO {
            let cost = prepared
                .acquisition_cost
                .map(LotCost::Known)
                .unwrap_or(LotCost::Unknown);
            let lot = PositionLot::new(
                c.user_id,
                c.account_id,
                c.instrument_id,
                transaction_id,
                prepared.quantity_effect,
                cost.clone(),
                currency.clone(),
                prepared.effective_at,
                u64::try_from(sequence).map_err(|_| StoreError::Invalid("sequence"))?,
            )
            .map_err(domain)?;
            sqlx::query("INSERT INTO portfolio.position_lots(id,user_id,account_id,instrument_id,source_transaction_id,original_quantity,remaining_quantity,original_cost,remaining_cost,currency,acquired_at,created_sequence) VALUES($1,$2,$3,$4,$5,$6,$6,$7,$7,$8,$9,$10)")
                .bind(lot.id.into_uuid()).bind(c.user_id.into_uuid()).bind(c.account_id.into_uuid()).bind(c.instrument_id.into_uuid()).bind(transaction_id.into_uuid()).bind(lot.original_quantity).bind(match cost{LotCost::Known(v)=>Some(v),LotCost::Unknown=>None}).bind(currency.as_str()).bind(lot.acquired_at).bind(sequence).execute(&mut *tx).await?;
            lots.push(lot);
        }
        for (kind, amount, known) in prepared.components {
            sqlx::query("INSERT INTO portfolio.transaction_components(id,transaction_id,user_id,component_kind,amount,currency,cost_known) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(Uuid::new_v4()).bind(transaction_id.into_uuid()).bind(c.user_id.into_uuid()).bind(kind).bind(amount).bind(currency.as_str()).bind(known).execute(&mut *tx).await?;
        }
        let known_qty = lots
            .iter()
            .filter(|l| matches!(l.remaining_cost, LotCost::Known(_)))
            .map(|l| l.remaining_quantity)
            .sum::<Decimal>();
        let unknown_qty = lots
            .iter()
            .filter(|l| matches!(l.remaining_cost, LotCost::Unknown))
            .map(|l| l.remaining_quantity)
            .sum::<Decimal>();
        let remaining_cost = lots
            .iter()
            .map(|l| match l.remaining_cost {
                LotCost::Known(v) => v,
                LotCost::Unknown => Decimal::ZERO,
            })
            .sum::<Decimal>();
        let realized_proceeds = projection.get::<Decimal, _>("realized_proceeds")
            + prepared.proceeds.unwrap_or(Decimal::ZERO);
        let realized_allocated =
            projection.get::<Decimal, _>("realized_allocated_cost") + allocated_cost;
        let realized_fees =
            projection.get::<Decimal, _>("realized_fees") + prepared.fee.unwrap_or(Decimal::ZERO);
        let realized = if unknown_disposal
            || projection
                .get::<Option<Decimal>, _>("realized_gain_loss")
                .is_none()
                && projection.get::<Decimal, _>("realized_proceeds") > Decimal::ZERO
        {
            None
        } else {
            Some(realized_proceeds - realized_allocated - realized_fees)
        };
        sqlx::query("UPDATE portfolio.position_projection SET quantity=$4,known_cost_quantity=$5,unknown_cost_quantity=$6,remaining_known_cost=$7,realized_proceeds=$8,realized_allocated_cost=$9,realized_fees=$10,realized_gain_loss=$11,version=$12,source_sequence=$13,updated_at=$14 WHERE user_id=$1 AND account_id=$2 AND instrument_id=$3")
            .bind(c.user_id.into_uuid()).bind(c.account_id.into_uuid()).bind(c.instrument_id.into_uuid()).bind(next_quantity).bind(known_qty).bind(unknown_qty).bind(remaining_cost).bind(realized_proceeds).bind(realized_allocated).bind(realized_fees).bind(realized).bind(i64::try_from(version+1).unwrap()).bind(sequence).bind(c.recorded_at).execute(&mut *tx).await?;
        let cash = if let Some(request) = &c.cash_settlement {
            if request.amount <= Decimal::ZERO {
                return Err(StoreError::Invalid("cash_amount"));
            }
            sqlx::query("INSERT INTO portfolio.cash_settlement_processes(transaction_id,user_id,action,cash_flow,state,cash_account_id,amount,currency,correlation_id,created_at,updated_at) VALUES($1,$2,'post',$3,'pending',$4,$5,$6,$7,$8,$8)").bind(transaction_id.into_uuid()).bind(c.user_id.into_uuid()).bind(if prepared.kind=="buy"{"outgoing"}else{"incoming"}).bind(request.cash_account_id.into_uuid()).bind(request.amount).bind(currency.as_str()).bind(c.correlation_id.into_uuid()).bind(c.recorded_at).execute(&mut *tx).await?;
            CashAccountingStatus::Pending
        } else {
            CashAccountingStatus::NotRequested
        };
        let result = result(
            transaction_id.into_uuid(),
            Some(transaction_id),
            version + 1,
            "posted",
            cash,
            c.correlation_id,
        );
        append_event(&mut tx,c.user_id,transaction_id.to_string(),version+1,TRANSACTION_POSTED_V1,c.correlation_id,c.recorded_at,json!({"transaction_id":transaction_id,"account_id":c.account_id,"instrument_id":c.instrument_id,"kind":prepared.kind,"quantity":prepared.stored_quantity.to_string(),"currency":currency})).await?;
        append_event(&mut tx,c.user_id,format!("{}:{}",c.account_id,c.instrument_id),version+1,POSITION_CHANGED_V1,c.correlation_id,c.recorded_at,json!({"account_id":c.account_id,"instrument_id":c.instrument_id,"quantity":next_quantity.to_string(),"known_cost_quantity":known_qty.to_string(),"unknown_cost_quantity":unknown_qty.to_string(),"remaining_known_cost":remaining_cost.to_string(),"realized_gain_loss":realized.map(|v|v.to_string()),"currency":currency,"position_version":version+1})).await?;
        audit(
            &mut tx,
            c.user_id,
            "transaction",
            transaction_id.into_uuid(),
            version + 1,
            "posted",
            c.correlation_id,
            c.recorded_at,
        )
        .await?;
        finish(
            &mut tx,
            c.user_id,
            "record_portfolio_transaction",
            c.idempotency_key.as_str(),
            201,
            &result,
            transaction_id.into_uuid(),
            version + 1,
            c.recorded_at,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn reverse(
        &self,
        c: ReversePortfolioTransaction,
        hash: [u8; 32],
    ) -> Result<PortfolioCommandResult, StoreError> {
        if c.reason.is_empty() || c.reason.trim() != c.reason {
            return Err(StoreError::Invalid("reason"));
        }
        let mut tx = self.pool.begin().await?;
        if let Some(v) = claim(
            &mut tx,
            c.user_id,
            "reverse_portfolio_transaction",
            c.idempotency_key.as_str(),
            hash,
        )
        .await?
        {
            tx.commit().await?;
            return replay(v);
        }
        let original=sqlx::query("SELECT account_id,instrument_id,kind,quantity,currency,effective_at FROM portfolio.transactions WHERE id=$1 AND user_id=$2 AND reversal_of IS NULL FOR UPDATE").bind(c.transaction_id.into_uuid()).bind(c.user_id.into_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::NotFound)?;
        let account_id = PortfolioAccountId::new(original.get("account_id"));
        let instrument_id = InstrumentId::new(original.get("instrument_id"));
        let account_version: i64 = sqlx::query_scalar(
            "SELECT version FROM portfolio.accounts WHERE id=$1 AND user_id=$2 FOR UPDATE",
        )
        .bind(account_id.into_uuid())
        .bind(c.user_id.into_uuid())
        .fetch_one(&mut *tx)
        .await?;
        if account_version != i64::try_from(c.expected_account_version).unwrap_or(i64::MAX) {
            return Err(StoreError::VersionConflict);
        }
        let p=sqlx::query("SELECT * FROM portfolio.position_projection WHERE user_id=$1 AND account_id=$2 AND instrument_id=$3 FOR UPDATE").bind(c.user_id.into_uuid()).bind(account_id.into_uuid()).bind(instrument_id.into_uuid()).fetch_one(&mut *tx).await?;
        let version: u64 = p
            .get::<i64, _>("version")
            .try_into()
            .map_err(|_| StoreError::Invalid("version"))?;
        if version != c.expected_position_version {
            return Err(StoreError::VersionConflict);
        }
        if sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM portfolio.transactions WHERE reversal_of=$1 AND user_id=$2)").bind(c.transaction_id.into_uuid()).bind(c.user_id.into_uuid()).fetch_one(&mut *tx).await?{return Err(StoreError::IdempotencyConflict)}
        let kind = original.get::<String, _>("kind");
        let quantity = original.get::<Decimal, _>("quantity");
        let effect = match kind.as_str() {
            "opening_position" | "buy" => quantity,
            "sell" | "redemption" => -quantity,
            "coupon" => Decimal::ZERO,
            "position_correction" => quantity,
            _ => return Err(StoreError::Invalid("reversal")),
        };
        let next_quantity = p.get::<Decimal, _>("quantity") - effect;
        if next_quantity < Decimal::ZERO {
            return Err(StoreError::Invalid("dependent_disposal"));
        }
        let reversal_id = PortfolioTransactionId::generate();
        let sequence = p.get::<i64, _>("source_sequence") + 1;
        let currency = CurrencyCode::new(original.get::<String, _>("currency"))
            .map_err(|_| StoreError::Invalid("currency"))?;
        sqlx::query("INSERT INTO portfolio.transactions(id,user_id,account_id,instrument_id,sequence,position_version,kind,status,quantity,currency,source,reason,reversal_of,actor_id,correlation_id,effective_at,recorded_at) VALUES($1,$2,$3,$4,$5,$6,'reversal','posted',$7,$8,'reversal',$9,$10,$11,$12,$13,$14)").bind(reversal_id.into_uuid()).bind(c.user_id.into_uuid()).bind(account_id.into_uuid()).bind(instrument_id.into_uuid()).bind(sequence).bind(i64::try_from(version+1).unwrap()).bind(-quantity).bind(currency.as_str()).bind(&c.reason).bind(c.transaction_id.into_uuid()).bind(c.actor_id.into_uuid()).bind(c.correlation_id.into_uuid()).bind(original.get::<DateTime<Utc>,_>("effective_at")).bind(c.recorded_at).execute(&mut *tx).await?;
        if effect > Decimal::ZERO {
            let lot=sqlx::query("SELECT id,remaining_quantity,remaining_cost FROM portfolio.position_lots WHERE source_transaction_id=$1 AND user_id=$2 FOR UPDATE").bind(c.transaction_id.into_uuid()).bind(c.user_id.into_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::Invalid("source_lot"))?;
            if lot.get::<Decimal, _>("remaining_quantity") != effect {
                return Err(StoreError::Invalid("dependent_disposal"));
            }
            let lot_id: Uuid = lot.get("id");
            let cost: Option<Decimal> = lot.get("remaining_cost");
            sqlx::query("UPDATE portfolio.position_lots SET remaining_quantity=0,remaining_cost=CASE WHEN remaining_cost IS NULL THEN NULL ELSE 0 END WHERE id=$1 AND user_id=$2").bind(lot_id).bind(c.user_id.into_uuid()).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO portfolio.lot_allocations(id,user_id,disposal_transaction_id,lot_id,quantity,allocated_cost,currency,recorded_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)").bind(Uuid::new_v4()).bind(c.user_id.into_uuid()).bind(reversal_id.into_uuid()).bind(lot_id).bind(effect).bind(cost).bind(currency.as_str()).bind(c.recorded_at).execute(&mut *tx).await?;
        } else if effect < Decimal::ZERO {
            let allocations=sqlx::query("SELECT id,lot_id,quantity,allocated_cost FROM portfolio.lot_allocations WHERE disposal_transaction_id=$1 AND user_id=$2 AND reverses_allocation_id IS NULL ORDER BY id FOR UPDATE").bind(c.transaction_id.into_uuid()).bind(c.user_id.into_uuid()).fetch_all(&mut *tx).await?;
            for a in allocations {
                let aid: Uuid = a.get("id");
                let lot: Uuid = a.get("lot_id");
                let q: Decimal = a.get("quantity");
                let cost: Option<Decimal> = a.get("allocated_cost");
                sqlx::query("UPDATE portfolio.position_lots SET remaining_quantity=remaining_quantity+$3,remaining_cost=CASE WHEN remaining_cost IS NULL THEN NULL ELSE remaining_cost+COALESCE($4,0) END WHERE id=$1 AND user_id=$2 AND remaining_quantity+$3<=original_quantity").bind(lot).bind(c.user_id.into_uuid()).bind(q).bind(cost).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO portfolio.lot_allocations(id,user_id,disposal_transaction_id,lot_id,quantity,allocated_cost,currency,reverses_allocation_id,recorded_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)").bind(Uuid::new_v4()).bind(c.user_id.into_uuid()).bind(reversal_id.into_uuid()).bind(lot).bind(q).bind(cost).bind(currency.as_str()).bind(aid).bind(c.recorded_at).execute(&mut *tx).await?;
            }
        }
        let components=sqlx::query("SELECT component_kind,amount,cost_known FROM portfolio.transaction_components WHERE transaction_id=$1 AND user_id=$2").bind(c.transaction_id.into_uuid()).bind(c.user_id.into_uuid()).fetch_all(&mut *tx).await?;
        for component in components {
            sqlx::query("INSERT INTO portfolio.transaction_components(id,transaction_id,user_id,component_kind,amount,currency,cost_known) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(Uuid::new_v4()).bind(reversal_id.into_uuid()).bind(c.user_id.into_uuid()).bind(component.get::<String,_>("component_kind")).bind(component.get::<Option<Decimal>,_>("amount").map(|v|-v)).bind(currency.as_str()).bind(component.get::<bool,_>("cost_known")).execute(&mut *tx).await?;
        }
        let totals=sqlx::query("SELECT COALESCE(SUM(remaining_quantity) FILTER(WHERE remaining_cost IS NOT NULL),0) known_quantity,COALESCE(SUM(remaining_quantity) FILTER(WHERE remaining_cost IS NULL),0) unknown_quantity,COALESCE(SUM(remaining_cost),0) remaining_cost FROM portfolio.position_lots WHERE user_id=$1 AND account_id=$2 AND instrument_id=$3").bind(c.user_id.into_uuid()).bind(account_id.into_uuid()).bind(instrument_id.into_uuid()).fetch_one(&mut *tx).await?;
        let original_proceeds:Decimal=sqlx::query_scalar("SELECT COALESCE(SUM(amount),0) FROM portfolio.transaction_components WHERE transaction_id=$1 AND user_id=$2 AND component_kind IN ('proceeds','coupon')").bind(c.transaction_id.into_uuid()).bind(c.user_id.into_uuid()).fetch_one(&mut *tx).await?;
        let original_fee:Decimal=sqlx::query_scalar("SELECT COALESCE(SUM(amount),0) FROM portfolio.transaction_components WHERE transaction_id=$1 AND user_id=$2 AND component_kind='fee'").bind(c.transaction_id.into_uuid()).bind(c.user_id.into_uuid()).fetch_one(&mut *tx).await?;
        let original_allocated:Decimal=sqlx::query_scalar("SELECT COALESCE(SUM(allocated_cost),0) FROM portfolio.lot_allocations WHERE disposal_transaction_id=$1 AND user_id=$2 AND reverses_allocation_id IS NULL").bind(c.transaction_id.into_uuid()).bind(c.user_id.into_uuid()).fetch_one(&mut *tx).await?;
        let proceeds = p.get::<Decimal, _>("realized_proceeds") - original_proceeds;
        let allocated = p.get::<Decimal, _>("realized_allocated_cost") - original_allocated;
        let fees = p.get::<Decimal, _>("realized_fees") - original_fee;
        let unknown = totals.get::<Decimal, _>("unknown_quantity");
        let realized = if unknown > Decimal::ZERO && proceeds > Decimal::ZERO {
            None
        } else {
            Some(proceeds - allocated - fees)
        };
        sqlx::query("UPDATE portfolio.position_projection SET quantity=$4,known_cost_quantity=$5,unknown_cost_quantity=$6,remaining_known_cost=$7,realized_proceeds=$8,realized_allocated_cost=$9,realized_fees=$10,realized_gain_loss=$11,version=$12,source_sequence=$13,updated_at=$14 WHERE user_id=$1 AND account_id=$2 AND instrument_id=$3").bind(c.user_id.into_uuid()).bind(account_id.into_uuid()).bind(instrument_id.into_uuid()).bind(next_quantity).bind(totals.get::<Decimal,_>("known_quantity")).bind(unknown).bind(totals.get::<Decimal,_>("remaining_cost")).bind(proceeds).bind(allocated).bind(fees).bind(realized).bind(i64::try_from(version+1).unwrap()).bind(sequence).bind(c.recorded_at).execute(&mut *tx).await?;
        let cash_changed=sqlx::query("UPDATE portfolio.cash_settlement_processes SET action='cancel_or_reverse',state='pending',updated_at=$3,completed_at=NULL WHERE transaction_id=$1 AND user_id=$2 AND state NOT IN ('cancelled_no_financial_effect','reversed')").bind(c.transaction_id.into_uuid()).bind(c.user_id.into_uuid()).bind(c.recorded_at).execute(&mut *tx).await?.rows_affected()>0;
        let cash = if cash_changed {
            CashAccountingStatus::Pending
        } else {
            CashAccountingStatus::NotRequested
        };
        let known = totals.get::<Decimal, _>("known_quantity");
        let remaining = totals.get::<Decimal, _>("remaining_cost");
        let result = result(
            reversal_id.into_uuid(),
            Some(reversal_id),
            version + 1,
            "posted",
            cash,
            c.correlation_id,
        );
        append_event(
            &mut tx,
            c.user_id,
            reversal_id.to_string(),
            version + 1,
            TRANSACTION_REVERSED_V1,
            c.correlation_id,
            c.recorded_at,
            json!({"transaction_id":reversal_id,"original_transaction_id":c.transaction_id}),
        )
        .await?;
        append_event(&mut tx,c.user_id,format!("{}:{}",account_id,instrument_id),version+1,POSITION_CHANGED_V1,c.correlation_id,c.recorded_at,json!({"account_id":account_id,"instrument_id":instrument_id,"quantity":next_quantity.to_string(),"known_cost_quantity":known.to_string(),"unknown_cost_quantity":unknown.to_string(),"remaining_known_cost":remaining.to_string(),"realized_gain_loss":realized.map(|v|v.to_string()),"currency":currency,"position_version":version+1})).await?;
        audit(
            &mut tx,
            c.user_id,
            "transaction",
            reversal_id.into_uuid(),
            version + 1,
            "reversed",
            c.correlation_id,
            c.recorded_at,
        )
        .await?;
        finish(
            &mut tx,
            c.user_id,
            "reverse_portfolio_transaction",
            c.idempotency_key.as_str(),
            201,
            &result,
            reversal_id.into_uuid(),
            version + 1,
            c.recorded_at,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn record_valuation(
        &self,
        c: RecordValuationSnapshot,
        hash: [u8; 32],
    ) -> Result<PortfolioCommandResult, StoreError> {
        ValuationSnapshot::record(
            c.user_id,
            c.account_id,
            c.instrument_id,
            c.price_per_instrument,
            c.accrued_interest_per_instrument,
            c.currency.clone(),
            c.source.clone(),
            c.quoted_at,
            c.recorded_at,
        )
        .map_err(domain)?;
        let mut tx = self.pool.begin().await?;
        if let Some(v) = claim(
            &mut tx,
            c.user_id,
            "record_valuation",
            c.idempotency_key.as_str(),
            hash,
        )
        .await?
        {
            tx.commit().await?;
            return replay(v);
        }
        let row=sqlx::query("SELECT p.quantity,p.version,p.source_sequence,i.currency FROM portfolio.position_projection p JOIN portfolio.instruments i ON i.id=p.instrument_id AND i.user_id=p.user_id WHERE p.user_id=$1 AND p.account_id=$2 AND p.instrument_id=$3 FOR UPDATE OF p")
            .bind(c.user_id.into_uuid()).bind(c.account_id.into_uuid()).bind(c.instrument_id.into_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::NotFound)?;
        if row.get::<String, _>("currency") != c.currency.as_str() {
            return Err(StoreError::Invalid("currency"));
        }
        let sequence = row.get::<i64, _>("source_sequence") + 1;
        let id = ValuationSnapshotId::generate();
        let quantity = row.get::<Decimal, _>("quantity");
        let market =
            (quantity * (c.price_per_instrument + c.accrued_interest_per_instrument)).round_dp(2);
        sqlx::query("INSERT INTO portfolio.valuation_snapshots(id,user_id,account_id,instrument_id,price_per_instrument,accrued_interest_per_instrument,currency,source,quoted_at,recorded_at,event_sequence) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)").bind(id.into_uuid()).bind(c.user_id.into_uuid()).bind(c.account_id.into_uuid()).bind(c.instrument_id.into_uuid()).bind(c.price_per_instrument).bind(c.accrued_interest_per_instrument).bind(c.currency.as_str()).bind(&c.source).bind(c.quoted_at).bind(c.recorded_at).bind(sequence).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO portfolio.latest_valuation_projection(user_id,account_id,instrument_id,snapshot_id,quantity,price_per_instrument,accrued_interest_per_instrument,market_value,currency,as_of,source_sequence) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT(user_id,account_id,instrument_id) DO UPDATE SET snapshot_id=EXCLUDED.snapshot_id,quantity=EXCLUDED.quantity,price_per_instrument=EXCLUDED.price_per_instrument,accrued_interest_per_instrument=EXCLUDED.accrued_interest_per_instrument,market_value=EXCLUDED.market_value,currency=EXCLUDED.currency,as_of=EXCLUDED.as_of,source_sequence=EXCLUDED.source_sequence WHERE (EXCLUDED.as_of,EXCLUDED.source_sequence,EXCLUDED.snapshot_id)>(portfolio.latest_valuation_projection.as_of,portfolio.latest_valuation_projection.source_sequence,portfolio.latest_valuation_projection.snapshot_id)")
            .bind(c.user_id.into_uuid()).bind(c.account_id.into_uuid()).bind(c.instrument_id.into_uuid()).bind(id.into_uuid()).bind(quantity).bind(c.price_per_instrument).bind(c.accrued_interest_per_instrument).bind(market).bind(c.currency.as_str()).bind(c.quoted_at).bind(sequence).execute(&mut *tx).await?;
        let version: u64 = row.get::<i64, _>("version").try_into().unwrap_or(0);
        let result = result(
            id.into_uuid(),
            None,
            version,
            "recorded",
            CashAccountingStatus::NotRequested,
            c.correlation_id,
        );
        append_event(&mut tx,c.user_id,id.to_string(),u64::try_from(sequence).unwrap_or(1),VALUATION_RECORDED_V1,c.correlation_id,c.recorded_at,json!({"snapshot_id":id,"account_id":c.account_id,"instrument_id":c.instrument_id,"quantity":quantity.to_string(),"price_per_instrument":c.price_per_instrument.to_string(),"accrued_interest_per_instrument":c.accrued_interest_per_instrument.to_string(),"market_value":market.to_string(),"currency":c.currency,"quoted_at":c.quoted_at,"source":c.source})).await?;
        audit(
            &mut tx,
            c.user_id,
            "valuation",
            id.into_uuid(),
            u64::try_from(sequence).unwrap_or(1),
            "recorded",
            c.correlation_id,
            c.recorded_at,
        )
        .await?;
        finish(
            &mut tx,
            c.user_id,
            "record_valuation",
            c.idempotency_key.as_str(),
            201,
            &result,
            id.into_uuid(),
            u64::try_from(sequence).unwrap_or(1),
            c.recorded_at,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn accounts(
        &self,
        user: UserId,
    ) -> Result<Vec<PortfolioAccountView>, StoreError> {
        let rows=sqlx::query("SELECT id,name,lifecycle,version,created_at,updated_at FROM portfolio.accounts WHERE user_id=$1 ORDER BY created_at,id").bind(user.into_uuid()).fetch_all(&self.pool).await?;
        rows.iter().map(account_view).collect()
    }
    pub(crate) async fn account(
        &self,
        user: UserId,
        id: PortfolioAccountId,
    ) -> Result<Option<PortfolioAccountView>, StoreError> {
        sqlx::query("SELECT id,name,lifecycle,version,created_at,updated_at FROM portfolio.accounts WHERE user_id=$1 AND id=$2").bind(user.into_uuid()).bind(id.into_uuid()).fetch_optional(&self.pool).await?.as_ref().map(account_view).transpose()
    }
    pub(crate) async fn instruments(
        &self,
        user: UserId,
    ) -> Result<Vec<InstrumentView>, StoreError> {
        let rows=sqlx::query("SELECT id,identifier_kind,identifier,display_name,currency,face_value,issue_date,maturity_date,coupon_kind,coupon_rate,version FROM portfolio.instruments WHERE user_id=$1 ORDER BY created_at,id").bind(user.into_uuid()).fetch_all(&self.pool).await?;
        rows.iter().map(instrument_view).collect()
    }
    pub(crate) async fn instrument(
        &self,
        user: UserId,
        id: InstrumentId,
    ) -> Result<Option<InstrumentView>, StoreError> {
        sqlx::query("SELECT id,identifier_kind,identifier,display_name,currency,face_value,issue_date,maturity_date,coupon_kind,coupon_rate,version FROM portfolio.instruments WHERE user_id=$1 AND id=$2").bind(user.into_uuid()).bind(id.into_uuid()).fetch_optional(&self.pool).await?.as_ref().map(instrument_view).transpose()
    }
    pub(crate) async fn positions(
        &self,
        user: UserId,
        account: PortfolioAccountId,
    ) -> Result<Vec<PositionView>, StoreError> {
        let rows=sqlx::query("SELECT p.*,v.market_value,v.as_of FROM portfolio.position_projection p LEFT JOIN portfolio.latest_valuation_projection v USING(user_id,account_id,instrument_id) WHERE p.user_id=$1 AND p.account_id=$2 ORDER BY p.instrument_id").bind(user.into_uuid()).bind(account.into_uuid()).fetch_all(&self.pool).await?;
        rows.iter().map(position_view).collect()
    }
    pub(crate) async fn activity(
        &self,
        user: UserId,
        account: PortfolioAccountId,
    ) -> Result<Vec<PortfolioTransactionView>, StoreError> {
        let rows=sqlx::query("SELECT t.*,COALESCE(p.state,'not_requested') cash_state FROM portfolio.transactions t LEFT JOIN portfolio.cash_settlement_processes p ON p.transaction_id=t.id AND p.user_id=t.user_id WHERE t.user_id=$1 AND t.account_id=$2 ORDER BY t.effective_at DESC,t.sequence DESC,t.id DESC").bind(user.into_uuid()).bind(account.into_uuid()).fetch_all(&self.pool).await?;
        rows.iter().map(transaction_view).collect()
    }
    pub(crate) async fn valuations(
        &self,
        user: UserId,
        account: PortfolioAccountId,
        instrument: InstrumentId,
    ) -> Result<Vec<ValuationView>, StoreError> {
        let rows=sqlx::query("SELECT id,account_id,instrument_id,price_per_instrument,accrued_interest_per_instrument,currency,source,quoted_at,recorded_at FROM portfolio.valuation_snapshots WHERE user_id=$1 AND account_id=$2 AND instrument_id=$3 ORDER BY quoted_at DESC,event_sequence DESC,id DESC").bind(user.into_uuid()).bind(account.into_uuid()).bind(instrument.into_uuid()).fetch_all(&self.pool).await?;
        rows.iter().map(valuation_view).collect()
    }
}

struct PreparedActivity {
    kind: &'static str,
    stored_quantity: Decimal,
    quantity_effect: Decimal,
    acquisition_cost: Option<Decimal>,
    proceeds: Option<Decimal>,
    fee: Option<Decimal>,
    effective_at: DateTime<Utc>,
    source: &'static str,
    reason: Option<String>,
    components: Vec<(&'static str, Option<Decimal>, bool)>,
    explicit_allocations: Option<Vec<RequestedLotAllocation>>,
}
fn prepare_activity(
    c: &RecordPortfolioTransaction,
    currency: &CurrencyCode,
) -> Result<PreparedActivity, StoreError> {
    let m = |v| Money::new(v, currency.clone(), 2).map_err(|_| StoreError::Invalid("money"));
    let p = match &c.activity {
        PortfolioActivityCommand::OpeningPosition {
            quantity,
            acquisition_cost,
            acquisition_date,
            reason,
        } => {
            if let Some(v) = acquisition_cost {
                m(*v)?;
            }
            if *quantity <= Decimal::ZERO || !quantity.fract().is_zero() {
                return Err(StoreError::Invalid("quantity"));
            }
            PreparedActivity {
                kind: "opening_position",
                stored_quantity: *quantity,
                quantity_effect: *quantity,
                acquisition_cost: *acquisition_cost,
                proceeds: None,
                fee: None,
                effective_at: acquisition_date.and_hms_opt(0, 0, 0).unwrap().and_utc(),
                source: "manual",
                reason: Some(reason.clone()),
                components: vec![(
                    "acquisition_cost",
                    *acquisition_cost,
                    acquisition_cost.is_some(),
                )],
                explicit_allocations: None,
            }
        }
        PortfolioActivityCommand::Buy {
            quantity,
            total_acquisition_cost,
            fee,
            accrued_interest,
            trade_at,
        } => {
            m(*total_acquisition_cost)?;
            if let Some(v) = fee {
                m(*v)?;
            }
            if let Some(v) = accrued_interest {
                m(*v)?;
            }
            if *quantity <= Decimal::ZERO || !quantity.fract().is_zero() {
                return Err(StoreError::Invalid("quantity"));
            }
            PreparedActivity {
                kind: "buy",
                stored_quantity: *quantity,
                quantity_effect: *quantity,
                acquisition_cost: Some(*total_acquisition_cost),
                proceeds: None,
                fee: *fee,
                effective_at: *trade_at,
                source: "manual",
                reason: None,
                components: vec![
                    ("acquisition_cost", Some(*total_acquisition_cost), true),
                    ("fee", *fee, true),
                    ("accrued_interest", *accrued_interest, true),
                ]
                .into_iter()
                .filter(|(_, v, _)| v.is_some())
                .collect(),
                explicit_allocations: None,
            }
        }
        PortfolioActivityCommand::Sell {
            quantity,
            proceeds,
            fee,
            trade_at,
            lot_allocations,
        } => {
            m(*proceeds)?;
            if let Some(v) = fee {
                m(*v)?;
            }
            if *quantity <= Decimal::ZERO || !quantity.fract().is_zero() {
                return Err(StoreError::Invalid("quantity"));
            }
            PreparedActivity {
                kind: "sell",
                stored_quantity: *quantity,
                quantity_effect: -*quantity,
                acquisition_cost: None,
                proceeds: Some(*proceeds),
                fee: *fee,
                effective_at: *trade_at,
                source: "manual",
                reason: None,
                components: vec![("proceeds", Some(*proceeds), true), ("fee", *fee, true)]
                    .into_iter()
                    .filter(|(_, v, _)| v.is_some())
                    .collect(),
                explicit_allocations: lot_allocations.clone(),
            }
        }
        PortfolioActivityCommand::Coupon {
            amount,
            payment_date,
            ..
        } => {
            m(*amount)?;
            PreparedActivity {
                kind: "coupon",
                stored_quantity: Decimal::ZERO,
                quantity_effect: Decimal::ZERO,
                acquisition_cost: None,
                proceeds: None,
                fee: None,
                effective_at: payment_date.and_hms_opt(0, 0, 0).unwrap().and_utc(),
                source: "manual",
                reason: None,
                components: vec![("coupon", Some(*amount), true)],
                explicit_allocations: None,
            }
        }
        PortfolioActivityCommand::Redemption {
            quantity,
            proceeds,
            maturity_date,
            reference,
            lot_allocations,
        } => {
            m(*proceeds)?;
            if *quantity <= Decimal::ZERO || !quantity.fract().is_zero() {
                return Err(StoreError::Invalid("quantity"));
            }
            PreparedActivity {
                kind: "redemption",
                stored_quantity: *quantity,
                quantity_effect: -*quantity,
                acquisition_cost: None,
                proceeds: Some(*proceeds),
                fee: None,
                effective_at: maturity_date.and_hms_opt(0, 0, 0).unwrap().and_utc(),
                source: "manual",
                reason: Some(reference.clone()),
                components: vec![("proceeds", Some(*proceeds), true)],
                explicit_allocations: lot_allocations.clone(),
            }
        }
        PortfolioActivityCommand::PositionCorrection {
            quantity_delta,
            cost_delta,
            reason,
            effective_at,
        } => {
            if !quantity_delta.fract().is_zero() {
                return Err(StoreError::Invalid("quantity"));
            }
            if let Some(v) = cost_delta {
                m(*v)?;
            }
            if quantity_delta.is_zero() && cost_delta.is_none_or(|value| value.is_zero()) {
                return Err(StoreError::Invalid("correction"));
            }
            PreparedActivity {
                kind: "position_correction",
                stored_quantity: *quantity_delta,
                quantity_effect: *quantity_delta,
                acquisition_cost: *cost_delta,
                proceeds: None,
                fee: None,
                effective_at: *effective_at,
                source: "correction",
                reason: Some(reason.clone()),
                components: cost_delta
                    .map(|v| vec![("cost_delta", Some(v), true)])
                    .unwrap_or_default(),
                explicit_allocations: None,
            }
        }
    };
    Ok(p)
}

async fn load_lots(
    tx: &mut Transaction<'_, Postgres>,
    c: &RecordPortfolioTransaction,
) -> Result<Vec<PositionLot>, StoreError> {
    let rows=sqlx::query("SELECT * FROM portfolio.position_lots WHERE user_id=$1 AND account_id=$2 AND instrument_id=$3 AND remaining_quantity>0 ORDER BY acquired_at,created_sequence,id FOR UPDATE").bind(c.user_id.into_uuid()).bind(c.account_id.into_uuid()).bind(c.instrument_id.into_uuid()).fetch_all(&mut **tx).await?;
    rows.iter()
        .map(|r| {
            Ok(PositionLot {
                id: LotId::new(r.get("id")),
                user_id: c.user_id,
                account_id: c.account_id,
                instrument_id: c.instrument_id,
                source_transaction_id: PortfolioTransactionId::new(r.get("source_transaction_id")),
                original_quantity: r.get("original_quantity"),
                remaining_quantity: r.get("remaining_quantity"),
                original_cost: r
                    .get::<Option<Decimal>, _>("original_cost")
                    .map(LotCost::Known)
                    .unwrap_or(LotCost::Unknown),
                remaining_cost: r
                    .get::<Option<Decimal>, _>("remaining_cost")
                    .map(LotCost::Known)
                    .unwrap_or(LotCost::Unknown),
                currency: CurrencyCode::new(r.get::<String, _>("currency"))
                    .map_err(|_| StoreError::Invalid("currency"))?,
                acquired_at: r.get("acquired_at"),
                created_sequence: r
                    .get::<i64, _>("created_sequence")
                    .try_into()
                    .map_err(|_| StoreError::Invalid("sequence"))?,
            })
        })
        .collect()
}
async fn persist_lot_changes(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    transaction: PortfolioTransactionId,
    result: &LotAllocationResult,
    now: DateTime<Utc>,
    currency: &CurrencyCode,
) -> Result<(), StoreError> {
    for lot in &result.lots {
        sqlx::query("UPDATE portfolio.position_lots SET remaining_quantity=$3,remaining_cost=$4 WHERE id=$1 AND user_id=$2").bind(lot.id.into_uuid()).bind(user.into_uuid()).bind(lot.remaining_quantity).bind(match lot.remaining_cost{LotCost::Known(v)=>Some(v),LotCost::Unknown=>None}).execute(&mut **tx).await?;
    }
    for a in &result.allocations {
        sqlx::query("INSERT INTO portfolio.lot_allocations(id,user_id,disposal_transaction_id,lot_id,quantity,allocated_cost,currency,recorded_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)").bind(a.id.into_uuid()).bind(user.into_uuid()).bind(transaction.into_uuid()).bind(a.lot_id.into_uuid()).bind(a.quantity).bind(a.allocated_cost).bind(currency.as_str()).bind(now).execute(&mut **tx).await?;
    }
    Ok(())
}

async fn claim(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    scope: &str,
    key: &str,
    hash: [u8; 32],
) -> Result<Option<Value>, StoreError> {
    let inserted=sqlx::query("INSERT INTO portfolio.command_receipts(user_id,command_scope,idempotency_key,canonical_request_hash,status) VALUES($1,$2,$3,$4,'processing') ON CONFLICT DO NOTHING").bind(user.into_uuid()).bind(scope).bind(key).bind(hash.as_slice()).execute(&mut **tx).await?.rows_affected();
    if inserted == 1 {
        return Ok(None);
    }
    let row=sqlx::query("SELECT canonical_request_hash,status,durable_result FROM portfolio.command_receipts WHERE user_id=$1 AND command_scope=$2 AND idempotency_key=$3 FOR UPDATE").bind(user.into_uuid()).bind(scope).bind(key).fetch_one(&mut **tx).await?;
    if row.get::<Vec<u8>, _>("canonical_request_hash") != hash {
        return Err(StoreError::IdempotencyConflict);
    }
    if row.get::<String, _>("status") == "processing" {
        return Err(StoreError::IdempotencyConflict);
    }
    Ok(row.get("durable_result"))
}
#[allow(clippy::too_many_arguments)]
async fn finish(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    scope: &str,
    key: &str,
    status: i16,
    result: &PortfolioCommandResult,
    aggregate: Uuid,
    version: u64,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE portfolio.command_receipts SET status='completed',status_code=$4,durable_result=$5,aggregate_id=$6,aggregate_version=$7,completed_at=$8 WHERE user_id=$1 AND command_scope=$2 AND idempotency_key=$3").bind(user.into_uuid()).bind(scope).bind(key).bind(status).bind(serde_json::to_value(result).map_err(|_|StoreError::Invalid("result"))?).bind(aggregate).bind(i64::try_from(version).unwrap_or(i64::MAX)).bind(now).execute(&mut **tx).await?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn append_event(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    aggregate: String,
    version: u64,
    event_type: &str,
    correlation: CorrelationId,
    now: DateTime<Utc>,
    payload: Value,
) -> Result<(), StoreError> {
    sqlx::query("INSERT INTO integration.outbox_messages(message_id,event_id,message_schema_version,context_name,aggregate_id,aggregate_version,event_type,user_id,occurred_at,correlation_id,payload) VALUES($1,$2,1,'portfolio',$3,$4,$5,$6,$7,$8,$9)").bind(Uuid::new_v4()).bind(EventId::generate().into_uuid()).bind(aggregate).bind(i64::try_from(version).unwrap_or(i64::MAX)).bind(event_type).bind(user.into_uuid()).bind(now).bind(correlation.into_uuid()).bind(payload).execute(&mut **tx).await?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    kind: &str,
    id: Uuid,
    version: u64,
    action: &str,
    correlation: CorrelationId,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    sqlx::query("INSERT INTO portfolio.audit_log(user_id,aggregate_type,aggregate_id,aggregate_version,action,correlation_id,recorded_at) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(user.into_uuid()).bind(kind).bind(id).bind(i64::try_from(version).unwrap_or(i64::MAX)).bind(action).bind(correlation.into_uuid()).bind(now).execute(&mut **tx).await?;
    Ok(())
}
fn result(
    id: Uuid,
    transaction_id: Option<PortfolioTransactionId>,
    version: u64,
    status: &str,
    cash: CashAccountingStatus,
    correlation_id: CorrelationId,
) -> PortfolioCommandResult {
    PortfolioCommandResult {
        aggregate_id: id,
        transaction_id,
        version,
        status: status.to_owned(),
        cash_accounting_status: cash,
        correlation_id,
        replayed: false,
    }
}
fn replay(v: Value) -> Result<PortfolioCommandResult, StoreError> {
    let mut r: PortfolioCommandResult =
        serde_json::from_value(v).map_err(|_| StoreError::Invalid("stored_result"))?;
    r.replayed = true;
    Ok(r)
}
fn domain(_: PortfolioError) -> StoreError {
    StoreError::Invalid("domain")
}
fn map_unique(e: sqlx::Error) -> StoreError {
    if e.as_database_error()
        .is_some_and(|d| d.is_unique_violation())
    {
        StoreError::IdempotencyConflict
    } else {
        StoreError::Database(e)
    }
}
fn identifier_kind(v: IdentifierKind) -> &'static str {
    match v {
        IdentifierKind::Isin => "isin",
        IdentifierKind::Manual => "manual",
    }
}
fn coupon_db(v: &CouponTerms) -> (&'static str, Option<Decimal>) {
    match v {
        CouponTerms::Fixed { annual_rate } => ("fixed", Some(*annual_rate)),
        CouponTerms::ZeroCoupon => ("zero_coupon", None),
        CouponTerms::Unknown => ("unknown", None),
    }
}
fn lifecycle_db(v: AccountLifecycle) -> &'static str {
    match v {
        AccountLifecycle::Active => "active",
        AccountLifecycle::Archived => "archived",
    }
}
fn account_view(r: &sqlx::postgres::PgRow) -> Result<PortfolioAccountView, StoreError> {
    Ok(PortfolioAccountView {
        id: PortfolioAccountId::new(r.get("id")),
        name: r.get("name"),
        lifecycle: match r.get::<String, _>("lifecycle").as_str() {
            "active" => AccountLifecycle::Active,
            "archived" => AccountLifecycle::Archived,
            _ => return Err(StoreError::Invalid("lifecycle")),
        },
        version: r
            .get::<i64, _>("version")
            .try_into()
            .map_err(|_| StoreError::Invalid("version"))?,
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    })
}
fn instrument_view(r: &sqlx::postgres::PgRow) -> Result<InstrumentView, StoreError> {
    let kind = match r.get::<String, _>("identifier_kind").as_str() {
        "isin" => IdentifierKind::Isin,
        "manual" => IdentifierKind::Manual,
        _ => return Err(StoreError::Invalid("identifier")),
    };
    let coupon_terms = match r.get::<String, _>("coupon_kind").as_str() {
        "fixed" => CouponTerms::Fixed {
            annual_rate: r.get("coupon_rate"),
        },
        "zero_coupon" => CouponTerms::ZeroCoupon,
        "unknown" => CouponTerms::Unknown,
        _ => return Err(StoreError::Invalid("coupon")),
    };
    Ok(InstrumentView {
        id: InstrumentId::new(r.get("id")),
        identifier: InstrumentIdentifier {
            kind,
            value: r.get("identifier"),
        },
        display_name: r.get("display_name"),
        currency: CurrencyCode::new(r.get::<String, _>("currency"))
            .map_err(|_| StoreError::Invalid("currency"))?,
        face_value: r.get("face_value"),
        issue_date: r.get::<NaiveDate, _>("issue_date"),
        maturity_date: r.get::<NaiveDate, _>("maturity_date"),
        coupon_terms,
        version: r
            .get::<i64, _>("version")
            .try_into()
            .map_err(|_| StoreError::Invalid("version"))?,
    })
}
fn position_view(r: &sqlx::postgres::PgRow) -> Result<PositionView, StoreError> {
    Ok(PositionView {
        account_id: PortfolioAccountId::new(r.get("account_id")),
        instrument_id: InstrumentId::new(r.get("instrument_id")),
        quantity: r.get("quantity"),
        known_cost_quantity: r.get("known_cost_quantity"),
        unknown_cost_quantity: r.get("unknown_cost_quantity"),
        remaining_known_cost: r.get("remaining_known_cost"),
        realized_gain_loss: r.get("realized_gain_loss"),
        currency: CurrencyCode::new(r.get::<String, _>("currency"))
            .map_err(|_| StoreError::Invalid("currency"))?,
        version: r
            .get::<i64, _>("version")
            .try_into()
            .map_err(|_| StoreError::Invalid("version"))?,
        latest_market_value: r.get("market_value"),
        valuation_as_of: r.get("as_of"),
    })
}
fn transaction_view(r: &sqlx::postgres::PgRow) -> Result<PortfolioTransactionView, StoreError> {
    let kind = match r.get::<String, _>("kind").as_str() {
        "opening_position" => PortfolioTransactionKind::OpeningPosition,
        "buy" => PortfolioTransactionKind::Buy,
        "sell" => PortfolioTransactionKind::Sell,
        "coupon" => PortfolioTransactionKind::Coupon,
        "redemption" => PortfolioTransactionKind::Redemption,
        "position_correction" => PortfolioTransactionKind::PositionCorrection,
        "reversal" => PortfolioTransactionKind::Reversal,
        _ => return Err(StoreError::Invalid("kind")),
    };
    Ok(PortfolioTransactionView {
        id: PortfolioTransactionId::new(r.get("id")),
        account_id: PortfolioAccountId::new(r.get("account_id")),
        instrument_id: InstrumentId::new(r.get("instrument_id")),
        kind,
        quantity: r.get("quantity"),
        currency: CurrencyCode::new(r.get::<String, _>("currency"))
            .map_err(|_| StoreError::Invalid("currency"))?,
        reversal_of: r
            .get::<Option<Uuid>, _>("reversal_of")
            .map(PortfolioTransactionId::new),
        correlation_id: CorrelationId::new(r.get("correlation_id")),
        effective_at: r.get("effective_at"),
        recorded_at: r.get("recorded_at"),
        cash_accounting_status: cash_status(&r.get::<String, _>("cash_state"))?,
    })
}
fn valuation_view(r: &sqlx::postgres::PgRow) -> Result<ValuationView, StoreError> {
    Ok(ValuationView {
        id: ValuationSnapshotId::new(r.get("id")),
        account_id: PortfolioAccountId::new(r.get("account_id")),
        instrument_id: InstrumentId::new(r.get("instrument_id")),
        price_per_instrument: r.get("price_per_instrument"),
        accrued_interest_per_instrument: r.get("accrued_interest_per_instrument"),
        currency: CurrencyCode::new(r.get::<String, _>("currency"))
            .map_err(|_| StoreError::Invalid("currency"))?,
        source: r.get("source"),
        quoted_at: r.get("quoted_at"),
        recorded_at: r.get("recorded_at"),
    })
}
fn cash_status(v: &str) -> Result<CashAccountingStatus, StoreError> {
    Ok(match v {
        "not_requested" => CashAccountingStatus::NotRequested,
        "pending" => CashAccountingStatus::Pending,
        "posted" => CashAccountingStatus::Posted,
        "retrying" => CashAccountingStatus::Retrying,
        "failed" => CashAccountingStatus::Failed,
        "cancelled_no_financial_effect" => CashAccountingStatus::CancelledNoFinancialEffect,
        "reversed" => CashAccountingStatus::Reversed,
        _ => return Err(StoreError::Invalid("cash_state")),
    })
}
