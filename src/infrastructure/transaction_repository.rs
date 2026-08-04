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
    external_balance: Option<Decimal>,
    transacted_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
}

/// Builds the dynamic `WHERE` clause for transaction list/count queries.
/// Returns the joined condition string and the number of bound parameters.
fn build_where_clause(params: &TransactionListParams) -> (String, usize) {
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
    (conditions.join(" AND "), param_count)
}

/// Binds the same set of WHERE-clause parameters in the same order used by `build_where_clause`.
fn bind_where_params<'q, O>(
    mut q: sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>,
    params: &'q TransactionListParams,
) -> sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments> {
    q = q.bind(params.user_id);
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
    q
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
        external_balance: r.external_balance,
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
              external_balance, transacted_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
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
        .bind(tx.external_balance)
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
        let (where_clause, param_count) = build_where_clause(params);
        let limit_param = param_count + 1;
        let offset_param = param_count + 2;

        let sql = format!(
            "SELECT * FROM transactions WHERE {where_clause} \
             ORDER BY transacted_at DESC LIMIT ${limit_param} OFFSET ${offset_param}"
        );

        let q = bind_where_params(sqlx::query_as::<_, TxRow>(&sql), params);
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

    async fn count(&self, params: &TransactionListParams) -> anyhow::Result<i64> {
        let (where_clause, _) = build_where_clause(params);
        let sql = format!("SELECT COUNT(*) FROM transactions WHERE {where_clause}");
        let q = bind_where_params(sqlx::query_as::<_, (i64,)>(&sql), params);
        let (count,) = q.fetch_one(&self.pool).await?;
        Ok(count)
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

    async fn create_idempotent(&self, tx: &Transaction) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "INSERT INTO transactions \
             (id, account_id, user_id, amount, currency, kind, category_id, note, external_id, \
              external_balance, transacted_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
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
        .bind(tx.external_balance)
        .bind(tx.transacted_at)
        .bind(tx.created_at)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn list_match_candidates(
        &self,
        user_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        min_amount: Decimal,
        max_amount: Decimal,
        currency: &str,
    ) -> anyhow::Result<Vec<Transaction>> {
        let rows = sqlx::query_as::<_, TxRow>(
            "SELECT t.* FROM transactions t \
             LEFT JOIN subscription_charges sc ON sc.transaction_id = t.id \
             WHERE t.user_id = $1 \
               AND t.kind = 'Expense' \
               AND t.transacted_at BETWEEN $2 AND $3 \
               AND t.amount BETWEEN $4 AND $5 \
               AND t.currency = $6 \
               AND sc.id IS NULL",
        )
        .bind(user_id)
        .bind(from)
        .bind(to)
        .bind(min_amount)
        .bind(max_amount)
        .bind(currency)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_tx).collect()
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
    use crate::infrastructure::test_db;
    use chrono::TimeZone;
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

    #[tokio::test]
    async fn create_and_find_income_transaction() {
        let (pool, user_id, account_id) = setup(test_db::fresh_pool().await).await;
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

    #[tokio::test]
    async fn list_transactions_filtered_by_kind() {
        let (pool, user_id, account_id) = setup(test_db::fresh_pool().await).await;
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

    fn base_params(user_id: Uuid) -> TransactionListParams {
        TransactionListParams {
            account_id: None,
            user_id,
            kind: None,
            category_id: None,
            from: None,
            to: None,
            limit: 100,
            offset: 0,
        }
    }

    async fn insert_tx(
        repo: &SqliteTransactionRepository,
        account_id: Uuid,
        user_id: Uuid,
        amount: i64,
        kind: TransactionKind,
        category_id: Option<Uuid>,
        transacted_at: DateTime<Utc>,
    ) -> Uuid {
        let tx = Transaction::new(
            account_id,
            user_id,
            Decimal::new(amount, 0),
            "USD".to_string(),
            kind,
            category_id,
            None,
            transacted_at,
        );
        let id = tx.id;
        repo.create(&tx, &TransactionDetails::None).await.unwrap();
        id
    }

    async fn insert_account(pool: &PgPool, user_id: Uuid) -> Uuid {
        let account = Account::new(
            user_id,
            "Other".to_string(),
            AccountType::Cash,
            "USD".to_string(),
        );
        let id = account.id;
        SqliteAccountRepository::new(pool.clone())
            .create(&account, &AccountDetails::None)
            .await
            .unwrap();
        id
    }

    async fn insert_category(pool: &PgPool, user_id: Uuid) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, user_id, name, color, created_at) \
             VALUES ($1, $2, $3, NULL, $4)",
        )
        .bind(id)
        .bind(user_id)
        .bind("Test Cat")
        .bind(Utc::now())
        .execute(pool)
        .await
        .unwrap();
        id
    }

    #[tokio::test]
    async fn list_filtered_by_date_range() {
        let (pool, user_id, account_id) = setup(test_db::fresh_pool().await).await;
        let repo = SqliteTransactionRepository::new(pool);
        let t1 = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();
        let t3 = Utc.with_ymd_and_hms(2024, 12, 1, 0, 0, 0).unwrap();
        insert_tx(
            &repo,
            account_id,
            user_id,
            1,
            TransactionKind::Income,
            None,
            t1,
        )
        .await;
        let mid = insert_tx(
            &repo,
            account_id,
            user_id,
            2,
            TransactionKind::Income,
            None,
            t2,
        )
        .await;
        insert_tx(
            &repo,
            account_id,
            user_id,
            3,
            TransactionKind::Income,
            None,
            t3,
        )
        .await;

        let params = TransactionListParams {
            from: Some(Utc.with_ymd_and_hms(2024, 4, 1, 0, 0, 0).unwrap()),
            to: Some(Utc.with_ymd_and_hms(2024, 8, 1, 0, 0, 0).unwrap()),
            ..base_params(user_id)
        };

        let results = repo.list(&params).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.id, mid);
        assert_eq!(repo.count(&params).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn list_filtered_by_category() {
        let (pool, user_id, account_id) = setup(test_db::fresh_pool().await).await;
        let cat_id = insert_category(&pool, user_id).await;
        let repo = SqliteTransactionRepository::new(pool);
        let categorized = insert_tx(
            &repo,
            account_id,
            user_id,
            10,
            TransactionKind::Expense,
            Some(cat_id),
            Utc::now(),
        )
        .await;
        insert_tx(
            &repo,
            account_id,
            user_id,
            20,
            TransactionKind::Expense,
            None,
            Utc::now(),
        )
        .await;

        let params = TransactionListParams {
            category_id: Some(cat_id),
            ..base_params(user_id)
        };

        let results = repo.list(&params).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.id, categorized);
        assert_eq!(results[0].0.category_id, Some(cat_id));
    }

    #[tokio::test]
    async fn list_filtered_by_account() {
        let (pool, user_id, account_id) = setup(test_db::fresh_pool().await).await;
        let other_account = insert_account(&pool, user_id).await;
        let repo = SqliteTransactionRepository::new(pool);
        let target = insert_tx(
            &repo,
            account_id,
            user_id,
            5,
            TransactionKind::Income,
            None,
            Utc::now(),
        )
        .await;
        insert_tx(
            &repo,
            other_account,
            user_id,
            7,
            TransactionKind::Income,
            None,
            Utc::now(),
        )
        .await;

        let params = TransactionListParams {
            account_id: Some(account_id),
            ..base_params(user_id)
        };

        let results = repo.list(&params).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.id, target);
        assert_eq!(results[0].0.account_id, account_id);
    }

    #[tokio::test]
    async fn list_only_returns_user_own() {
        let (pool, user1, account1) = setup(test_db::fresh_pool().await).await;
        let user2 = Uuid::new_v4();
        let account2 = insert_account(&pool, user2).await;
        let repo = SqliteTransactionRepository::new(pool);

        let mine = insert_tx(
            &repo,
            account1,
            user1,
            1,
            TransactionKind::Income,
            None,
            Utc::now(),
        )
        .await;
        insert_tx(
            &repo,
            account2,
            user2,
            2,
            TransactionKind::Income,
            None,
            Utc::now(),
        )
        .await;

        let params = base_params(user1);
        let results = repo.list(&params).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.id, mine);
        assert_eq!(repo.count(&params).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn list_pagination_orders_newest_first() {
        let (pool, user_id, account_id) = setup(test_db::fresh_pool().await).await;
        let repo = SqliteTransactionRepository::new(pool);
        let times = [
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2024, 2, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2024, 4, 1, 0, 0, 0).unwrap(),
        ];
        let mut ids = Vec::new();
        for (i, t) in times.iter().enumerate() {
            ids.push(
                insert_tx(
                    &repo,
                    account_id,
                    user_id,
                    (i + 1) as i64,
                    TransactionKind::Income,
                    None,
                    *t,
                )
                .await,
            );
        }

        let page1 = repo
            .list(&TransactionListParams {
                limit: 2,
                offset: 0,
                ..base_params(user_id)
            })
            .await
            .unwrap();
        let page2 = repo
            .list(&TransactionListParams {
                limit: 2,
                offset: 2,
                ..base_params(user_id)
            })
            .await
            .unwrap();

        assert_eq!(page1.len(), 2);
        assert_eq!(page2.len(), 2);
        assert_eq!(page1[0].0.id, ids[3]);
        assert_eq!(page1[1].0.id, ids[2]);
        assert_eq!(page2[0].0.id, ids[1]);
        assert_eq!(page2[1].0.id, ids[0]);
    }

    #[tokio::test]
    async fn count_ignores_limit_offset() {
        let (pool, user_id, account_id) = setup(test_db::fresh_pool().await).await;
        let repo = SqliteTransactionRepository::new(pool);
        for i in 0..5 {
            insert_tx(
                &repo,
                account_id,
                user_id,
                (i + 1) as i64,
                TransactionKind::Income,
                None,
                Utc::now(),
            )
            .await;
        }

        let params = TransactionListParams {
            limit: 2,
            offset: 0,
            ..base_params(user_id)
        };
        assert_eq!(repo.list(&params).await.unwrap().len(), 2);
        assert_eq!(repo.count(&params).await.unwrap(), 5);
    }
}
