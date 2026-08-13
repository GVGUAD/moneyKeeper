//! Operational balance-projection verification and rebuild.

use rust_decimal::Decimal;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::infrastructure::v2_db::VerifiedV2Pool;
use crate::shared_kernel::{CurrencyCode, UserId};

use super::super::{
    domain::{LedgerAccountId, LedgerError},
    public::ProjectionMismatch,
};

#[derive(Clone)]
pub(crate) struct PgLedgerProjection {
    pool: PgPool,
}

impl PgLedgerProjection {
    pub(crate) fn new(pool: &VerifiedV2Pool) -> Self {
        Self {
            pool: pool.pool().clone(),
        }
    }

    pub(crate) async fn verify(&self) -> Result<Vec<ProjectionMismatch>, LedgerError> {
        #[derive(FromRow)]
        struct Row {
            account_id: Uuid,
            user_id: Uuid,
            currency: String,
            projected: Decimal,
            posting_sum: Decimal,
        }
        let rows = sqlx::query_as::<_, Row>(
            "SELECT b.account_id, b.user_id, b.currency, b.signed_balance AS projected, \
                    COALESCE(SUM(p.signed_amount), 0)::numeric AS posting_sum \
             FROM ledger.account_balances b LEFT JOIN ledger.postings p \
               ON p.account_id = b.account_id AND p.user_id = b.user_id \
             GROUP BY b.account_id, b.user_id, b.currency, b.signed_balance \
             HAVING b.signed_balance <> COALESCE(SUM(p.signed_amount), 0)",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(LedgerError::database)?;
        rows.into_iter()
            .map(|row| {
                Ok(ProjectionMismatch {
                    account_id: LedgerAccountId::new(row.account_id),
                    user_id: UserId::new(row.user_id),
                    currency: CurrencyCode::new(row.currency)
                        .map_err(|_| LedgerError::persistence("stored currency invalid"))?,
                    projected: row.projected,
                    posting_sum: row.posting_sum,
                    delta: row.posting_sum - row.projected,
                })
            })
            .collect()
    }

    pub(crate) async fn rebuild(&self) -> Result<(), LedgerError> {
        let mut tx = self.pool.begin().await.map_err(LedgerError::database)?;
        sqlx::query("LOCK TABLE ledger.account_balances IN EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await
            .map_err(LedgerError::database)?;
        sqlx::query(
            "UPDATE ledger.account_balances b SET signed_balance = facts.balance, \
                    version = b.version + 1, as_of = clock_timestamp() \
             FROM (SELECT a.id, a.user_id, COALESCE(SUM(p.signed_amount), 0)::numeric AS balance \
                   FROM ledger.accounts a LEFT JOIN ledger.postings p \
                     ON p.account_id = a.id AND p.user_id = a.user_id \
                   GROUP BY a.id, a.user_id) facts \
             WHERE b.account_id = facts.id AND b.user_id = facts.user_id",
        )
        .execute(&mut *tx)
        .await
        .map_err(LedgerError::database)?;
        tx.commit().await.map_err(LedgerError::database)
    }
}
