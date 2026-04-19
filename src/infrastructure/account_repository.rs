use anyhow::Context;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::account::{
    Account, AccountDetails, AccountRepository, AccountType, BinanceDetails, CompoundingPeriod,
    InvestmentDetails, LoanDetails, LoanDirection, SavingsDetails,
};
use crate::domain::transaction::TransactionKind;

pub struct SqliteAccountRepository {
    pool: PgPool,
}

impl SqliteAccountRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct AccountRow {
    id: Uuid,
    user_id: Uuid,
    name: String,
    account_type: String,
    currency: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn row_to_account(r: AccountRow) -> anyhow::Result<Account> {
    Ok(Account {
        id: r.id,
        user_id: r.user_id,
        name: r.name,
        account_type: AccountType::from_str(&r.account_type)?,
        currency: r.currency,
        created_at: r.created_at,
        updated_at: r.updated_at,
    })
}

#[async_trait::async_trait]
impl AccountRepository for SqliteAccountRepository {
    async fn create(&self, account: &Account, details: &AccountDetails) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO accounts (id, user_id, name, account_type, currency, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(account.id)
        .bind(account.user_id)
        .bind(&account.name)
        .bind(account.account_type.as_str())
        .bind(&account.currency)
        .bind(account.created_at)
        .bind(account.updated_at)
        .execute(&self.pool)
        .await
        .context("insert account")?;

        self.insert_details(account.id, details).await
    }

    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<(Account, AccountDetails)>> {
        let row = sqlx::query_as::<_, AccountRow>(
            "SELECT * FROM accounts WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            None => Ok(None),
            Some(r) => {
                let account = row_to_account(r)?;
                let details = self
                    .fetch_details(account.id, &account.account_type)
                    .await?;
                Ok(Some((account, details)))
            }
        }
    }

    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<(Account, AccountDetails)>> {
        let rows = sqlx::query_as::<_, AccountRow>(
            "SELECT * FROM accounts WHERE user_id = $1 ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let account = row_to_account(row)?;
            let details = self
                .fetch_details(account.id, &account.account_type)
                .await?;
            result.push((account, details));
        }
        Ok(result)
    }

    async fn update(&self, account: &Account, details: &AccountDetails) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE accounts SET name = $1, currency = $2, updated_at = $3 \
             WHERE id = $4 AND user_id = $5",
        )
        .bind(&account.name)
        .bind(&account.currency)
        .bind(account.updated_at)
        .bind(account.id)
        .bind(account.user_id)
        .execute(&self.pool)
        .await?;

        self.delete_details(account.id, &account.account_type)
            .await?;
        self.insert_details(account.id, details).await
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM accounts WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn compute_balance(&self, account_id: Uuid, user_id: Uuid) -> anyhow::Result<Decimal> {
        let rows: Vec<(Decimal, String)> = sqlx::query_as(
            "SELECT amount, kind FROM transactions WHERE account_id = $1 AND user_id = $2",
        )
        .bind(account_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        let mut balance = Decimal::ZERO;
        for (amount, kind_str) in rows {
            let kind = TransactionKind::from_str(&kind_str)?;
            if kind.affects_balance_positively() {
                balance += amount;
            } else {
                balance -= amount;
            }
        }
        Ok(balance)
    }
}

impl SqliteAccountRepository {
    async fn insert_details(&self, id: Uuid, details: &AccountDetails) -> anyhow::Result<()> {
        match details {
            AccountDetails::Savings(d) => {
                sqlx::query(
                    "INSERT INTO savings_details (account_id, interest_rate, compounding_period) \
                     VALUES ($1, $2, $3)",
                )
                .bind(id)
                .bind(d.interest_rate)
                .bind(d.compounding_period.as_str())
                .execute(&self.pool)
                .await?;
            }
            AccountDetails::Loan(d) => {
                sqlx::query(
                    "INSERT INTO loan_details \
                     (account_id, counterparty, direction, interest_rate, due_date) \
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(id)
                .bind(&d.counterparty)
                .bind(d.direction.as_str())
                .bind(d.interest_rate)
                .bind(d.due_date)
                .execute(&self.pool)
                .await?;
            }
            AccountDetails::Investment(d) => {
                sqlx::query("INSERT INTO investment_details (account_id, broker) VALUES ($1, $2)")
                    .bind(id)
                    .bind(&d.broker)
                    .execute(&self.pool)
                    .await?;
            }
            AccountDetails::Binance(d) => {
                sqlx::query("INSERT INTO binance_details (account_id, label) VALUES ($1, $2)")
                    .bind(id)
                    .bind(&d.label)
                    .execute(&self.pool)
                    .await?;
            }
            AccountDetails::None => {}
        }
        Ok(())
    }

    async fn delete_details(&self, id: Uuid, account_type: &AccountType) -> anyhow::Result<()> {
        match account_type {
            AccountType::Savings => {
                sqlx::query("DELETE FROM savings_details WHERE account_id = $1")
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
            }
            AccountType::Loan => {
                sqlx::query("DELETE FROM loan_details WHERE account_id = $1")
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
            }
            AccountType::Investment => {
                sqlx::query("DELETE FROM investment_details WHERE account_id = $1")
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
            }
            AccountType::Binance => {
                sqlx::query("DELETE FROM binance_details WHERE account_id = $1")
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn fetch_details(
        &self,
        id: Uuid,
        account_type: &AccountType,
    ) -> anyhow::Result<AccountDetails> {
        match account_type {
            AccountType::Savings => {
                let row: Option<(Decimal, String)> = sqlx::query_as(
                    "SELECT interest_rate, compounding_period \
                     FROM savings_details WHERE account_id = $1",
                )
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
                match row {
                    Some((rate, period)) => Ok(AccountDetails::Savings(SavingsDetails {
                        account_id: id,
                        interest_rate: rate,
                        compounding_period: CompoundingPeriod::from_str(&period)?,
                    })),
                    None => Ok(AccountDetails::None),
                }
            }
            AccountType::Loan => {
                let row: Option<(String, String, Option<Decimal>, Option<NaiveDate>)> =
                    sqlx::query_as(
                        "SELECT counterparty, direction, interest_rate, due_date \
                         FROM loan_details WHERE account_id = $1",
                    )
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await?;
                match row {
                    Some((counterparty, direction, rate, due_date)) => {
                        Ok(AccountDetails::Loan(LoanDetails {
                            account_id: id,
                            counterparty,
                            direction: LoanDirection::from_str(&direction)?,
                            interest_rate: rate,
                            due_date,
                        }))
                    }
                    None => Ok(AccountDetails::None),
                }
            }
            AccountType::Investment => {
                let row: Option<(Option<String>,)> =
                    sqlx::query_as("SELECT broker FROM investment_details WHERE account_id = $1")
                        .bind(id)
                        .fetch_optional(&self.pool)
                        .await?;
                match row {
                    Some((broker,)) => Ok(AccountDetails::Investment(InvestmentDetails {
                        account_id: id,
                        broker,
                    })),
                    None => Ok(AccountDetails::None),
                }
            }
            AccountType::Binance => {
                let row: Option<(Option<String>,)> =
                    sqlx::query_as("SELECT label FROM binance_details WHERE account_id = $1")
                        .bind(id)
                        .fetch_optional(&self.pool)
                        .await?;
                match row {
                    Some((label,)) => Ok(AccountDetails::Binance(BinanceDetails {
                        account_id: id,
                        label,
                    })),
                    None => Ok(AccountDetails::None),
                }
            }
            _ => Ok(AccountDetails::None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn create_and_find_cash_account(pool: PgPool) {
        let repo = SqliteAccountRepository::new(pool);
        let user_id = Uuid::new_v4();
        let account = Account::new(
            user_id,
            "Wallet".to_string(),
            AccountType::Cash,
            "USD".to_string(),
        );
        repo.create(&account, &AccountDetails::None).await.unwrap();
        let (found, details) = repo.find_by_id(account.id, user_id).await.unwrap().unwrap();
        assert_eq!(found.name, "Wallet");
        assert!(matches!(details, AccountDetails::None));
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn create_and_find_savings_account(pool: PgPool) {
        let repo = SqliteAccountRepository::new(pool);
        let user_id = Uuid::new_v4();
        let account = Account::new(
            user_id,
            "Savings".to_string(),
            AccountType::Savings,
            "USD".to_string(),
        );
        let details = AccountDetails::Savings(SavingsDetails {
            account_id: account.id,
            interest_rate: Decimal::new(5, 2),
            compounding_period: CompoundingPeriod::Monthly,
        });
        repo.create(&account, &details).await.unwrap();
        let (_, found_details) = repo.find_by_id(account.id, user_id).await.unwrap().unwrap();
        if let AccountDetails::Savings(s) = found_details {
            assert_eq!(s.compounding_period, CompoundingPeriod::Monthly);
        } else {
            panic!("expected savings details");
        }
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn cannot_find_other_users_account(pool: PgPool) {
        let repo = SqliteAccountRepository::new(pool);
        let user_id = Uuid::new_v4();
        let account = Account::new(
            user_id,
            "Wallet".to_string(),
            AccountType::Cash,
            "USD".to_string(),
        );
        repo.create(&account, &AccountDetails::None).await.unwrap();
        let other_user_id = Uuid::new_v4();
        let result = repo.find_by_id(account.id, other_user_id).await.unwrap();
        assert!(result.is_none());
    }
}
