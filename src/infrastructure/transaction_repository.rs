use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::transaction::{
    TradeDetails, Transaction, TransactionDetails, TransactionKind, TransactionListParams,
    TransactionRepository, TransferLink,
};

pub struct SqliteTransactionRepository {
    pool: PgPool,
}

impl SqliteTransactionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct TxRow {
    id: Uuid,
    account_id: Uuid,
    user_id: Uuid,
    amount: Decimal,
    currency: String,
    kind: String,
    category_id: Option<Uuid>,
    note: Option<String>,
    external_id: Option<String>,
    transacted_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
}

fn row_to_tx(r: TxRow) -> anyhow::Result<Transaction> {
    Ok(Transaction {
        id: r.id,
        account_id: r.account_id,
        user_id: r.user_id,
        amount: r.amount,
        currency: r.currency,
        kind: TransactionKind::from_str(&r.kind)?,
        category_id: r.category_id,
        note: r.note,
        external_id: r.external_id,
        transacted_at: r.transacted_at,
        created_at: r.created_at,
    })
}

#[async_trait::async_trait]
impl TransactionRepository for SqliteTransactionRepository {
    async fn create(&self, tx: &Transaction, details: &TransactionDetails) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO transactions \
             (id, account_id, user_id, amount, currency, kind, category_id, note, external_id, \
              transacted_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(tx.id)
        .bind(tx.account_id)
        .bind(tx.user_id)
        .bind(tx.amount)
        .bind(&tx.currency)
        .bind(tx.kind.as_str())
        .bind(tx.category_id)
        .bind(&tx.note)
        .bind(&tx.external_id)
        .bind(tx.transacted_at)
        .bind(tx.created_at)
        .execute(&self.pool)
        .await?;

        match details {
            TransactionDetails::Transfer(link) => {
                sqlx::query(
                    "INSERT INTO transfer_links (from_transaction_id, to_transaction_id) \
                     VALUES ($1, $2)",
                )
                .bind(link.from_transaction_id)
                .bind(link.to_transaction_id)
                .execute(&self.pool)
                .await?;
            }
            TransactionDetails::Trade(trade) => {
                sqlx::query(
                    "INSERT INTO trade_details \
                     (transaction_id, ticker, quantity, price_per_unit, fee) \
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(trade.transaction_id)
                .bind(&trade.ticker)
                .bind(trade.quantity)
                .bind(trade.price_per_unit)
                .bind(trade.fee)
                .execute(&self.pool)
                .await?;
            }
            TransactionDetails::None => {}
        }
        Ok(())
    }

    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<(Transaction, TransactionDetails)>> {
        let row =
            sqlx::query_as::<_, TxRow>("SELECT * FROM transactions WHERE id = $1 AND user_id = $2")
                .bind(id)
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await?;

        match row {
            None => Ok(None),
            Some(r) => {
                let tx = row_to_tx(r)?;
                let details = self.fetch_details(&tx).await?;
                Ok(Some((tx, details)))
            }
        }
    }

    async fn list(
        &self,
        params: &TransactionListParams,
    ) -> anyhow::Result<Vec<(Transaction, TransactionDetails)>> {
        // PostgreSQL requires numbered placeholders $1, $2, ... — build them dynamically.
        let mut conditions = vec!["user_id = $1".to_string()];
        let mut param_count = 1usize;

        if params.account_id.is_some() {
            param_count += 1;
            conditions.push(format!("account_id = ${param_count}"));
        }
        if params.kind.is_some() {
            param_count += 1;
            conditions.push(format!("kind = ${param_count}"));
        }
        if params.category_id.is_some() {
            param_count += 1;
            conditions.push(format!("category_id = ${param_count}"));
        }
        if params.from.is_some() {
            param_count += 1;
            conditions.push(format!("transacted_at >= ${param_count}"));
        }
        if params.to.is_some() {
            param_count += 1;
            conditions.push(format!("transacted_at <= ${param_count}"));
        }
        param_count += 1;
        let limit_param = param_count;
        param_count += 1;
        let offset_param = param_count;

        let sql = format!(
            "SELECT * FROM transactions WHERE {} \
             ORDER BY transacted_at DESC LIMIT ${limit_param} OFFSET ${offset_param}",
            conditions.join(" AND ")
        );

        let mut q = sqlx::query_as::<_, TxRow>(&sql).bind(params.user_id);
        if let Some(acc) = params.account_id {
            q = q.bind(acc);
        }
        if let Some(k) = &params.kind {
            q = q.bind(k.as_str());
        }
        if let Some(cat) = params.category_id {
            q = q.bind(cat);
        }
        if let Some(from) = params.from {
            q = q.bind(from);
        }
        if let Some(to) = params.to {
            q = q.bind(to);
        }
        let rows = q
            .bind(params.limit)
            .bind(params.offset)
            .fetch_all(&self.pool)
            .await?;

        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let tx = row_to_tx(row)?;
            let details = self.fetch_details(&tx).await?;
            result.push((tx, details));
        }
        Ok(result)
    }

    async fn update(&self, tx: &Transaction, details: &TransactionDetails) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE transactions \
             SET amount = $1, currency = $2, kind = $3, category_id = $4, note = $5, \
                 transacted_at = $6 \
             WHERE id = $7 AND user_id = $8",
        )
        .bind(tx.amount)
        .bind(&tx.currency)
        .bind(tx.kind.as_str())
        .bind(tx.category_id)
        .bind(&tx.note)
        .bind(tx.transacted_at)
        .bind(tx.id)
        .bind(tx.user_id)
        .execute(&self.pool)
        .await?;

        sqlx::query("DELETE FROM trade_details WHERE transaction_id = $1")
            .bind(tx.id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM transfer_links WHERE from_transaction_id = $1")
            .bind(tx.id)
            .execute(&self.pool)
            .await?;

        if let TransactionDetails::Trade(trade) = details {
            sqlx::query(
                "INSERT INTO trade_details \
                 (transaction_id, ticker, quantity, price_per_unit, fee) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(trade.transaction_id)
            .bind(&trade.ticker)
            .bind(trade.quantity)
            .bind(trade.price_per_unit)
            .bind(trade.fee)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM transactions WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn create_idempotent(&self, tx: &Transaction) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO transactions \
             (id, account_id, user_id, amount, currency, kind, category_id, note, external_id, \
              transacted_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             ON CONFLICT (external_id) WHERE external_id IS NOT NULL DO NOTHING",
        )
        .bind(tx.id)
        .bind(tx.account_id)
        .bind(tx.user_id)
        .bind(tx.amount)
        .bind(&tx.currency)
        .bind(tx.kind.as_str())
        .bind(tx.category_id)
        .bind(&tx.note)
        .bind(&tx.external_id)
        .bind(tx.transacted_at)
        .bind(tx.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

impl SqliteTransactionRepository {
    async fn fetch_details(&self, tx: &Transaction) -> anyhow::Result<TransactionDetails> {
        match tx.kind {
            TransactionKind::Transfer => {
                let row: Option<(Uuid,)> = sqlx::query_as(
                    "SELECT to_transaction_id FROM transfer_links \
                     WHERE from_transaction_id = $1",
                )
                .bind(tx.id)
                .fetch_optional(&self.pool)
                .await?;
                if let Some((to_id,)) = row {
                    return Ok(TransactionDetails::Transfer(TransferLink {
                        from_transaction_id: tx.id,
                        to_transaction_id: to_id,
                    }));
                }
                Ok(TransactionDetails::None)
            }
            TransactionKind::Buy | TransactionKind::Sell | TransactionKind::StakingReward => {
                let row: Option<(String, Decimal, Option<Decimal>, Option<Decimal>)> =
                    sqlx::query_as(
                        "SELECT ticker, quantity, price_per_unit, fee \
                         FROM trade_details WHERE transaction_id = $1",
                    )
                    .bind(tx.id)
                    .fetch_optional(&self.pool)
                    .await?;
                if let Some((ticker, quantity, price, fee)) = row {
                    return Ok(TransactionDetails::Trade(TradeDetails {
                        transaction_id: tx.id,
                        ticker,
                        quantity,
                        price_per_unit: price,
                        fee,
                    }));
                }
                Ok(TransactionDetails::None)
            }
            _ => Ok(TransactionDetails::None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::account::{Account, AccountDetails, AccountRepository, AccountType};
    use crate::infrastructure::account_repository::SqliteAccountRepository;
    use sqlx::PgPool;

    async fn setup(pool: PgPool) -> (PgPool, Uuid, Uuid) {
        let user_id = Uuid::new_v4();
        let account = Account::new(
            user_id,
            "Cash".to_string(),
            AccountType::Cash,
            "USD".to_string(),
        );
        let account_id = account.id;
        SqliteAccountRepository::new(pool.clone())
            .create(&account, &AccountDetails::None)
            .await
            .unwrap();
        (pool, user_id, account_id)
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn create_and_find_income_transaction(pool: PgPool) {
        let (pool, user_id, account_id) = setup(pool).await;
        let repo = SqliteTransactionRepository::new(pool);
        let tx = Transaction::new(
            account_id,
            user_id,
            Decimal::new(100, 0),
            "USD".to_string(),
            TransactionKind::Income,
            None,
            Some("salary".to_string()),
            Utc::now(),
        );
        repo.create(&tx, &TransactionDetails::None).await.unwrap();
        let (found, _) = repo.find_by_id(tx.id, user_id).await.unwrap().unwrap();
        assert_eq!(found.amount, Decimal::new(100, 0));
        assert_eq!(found.note, Some("salary".to_string()));
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn list_transactions_filtered_by_kind(pool: PgPool) {
        let (pool, user_id, account_id) = setup(pool).await;
        let repo = SqliteTransactionRepository::new(pool);
        let income = Transaction::new(
            account_id,
            user_id,
            Decimal::new(100, 0),
            "USD".to_string(),
            TransactionKind::Income,
            None,
            None,
            Utc::now(),
        );
        let expense = Transaction::new(
            account_id,
            user_id,
            Decimal::new(50, 0),
            "USD".to_string(),
            TransactionKind::Expense,
            None,
            None,
            Utc::now(),
        );
        repo.create(&income, &TransactionDetails::None)
            .await
            .unwrap();
        repo.create(&expense, &TransactionDetails::None)
            .await
            .unwrap();
        let params = TransactionListParams {
            account_id: Some(account_id),
            user_id,
            kind: Some(TransactionKind::Income),
            category_id: None,
            from: None,
            to: None,
            limit: 10,
            offset: 0,
        };
        let results = repo.list(&params).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].0.kind, TransactionKind::Income));
    }
}
