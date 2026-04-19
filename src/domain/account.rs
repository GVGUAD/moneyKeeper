use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub enum AccountType {
    Cash,
    Bank,
    Savings,
    Loan,
    Investment,
    Binance,
}

impl AccountType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cash => "Cash",
            Self::Bank => "Bank",
            Self::Savings => "Savings",
            Self::Loan => "Loan",
            Self::Investment => "Investment",
            Self::Binance => "Binance",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "Cash" => Ok(Self::Cash),
            "Bank" => Ok(Self::Bank),
            "Savings" => Ok(Self::Savings),
            "Loan" => Ok(Self::Loan),
            "Investment" => Ok(Self::Investment),
            "Binance" => Ok(Self::Binance),
            other => anyhow::bail!("unknown account type: {other}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Account {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub account_type: AccountType,
    pub currency: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Account {
    pub fn new(user_id: Uuid, name: String, account_type: AccountType, currency: String) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            user_id,
            name,
            account_type,
            currency,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CompoundingPeriod {
    Daily,
    Monthly,
    Quarterly,
    Annually,
}

impl CompoundingPeriod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Daily => "Daily",
            Self::Monthly => "Monthly",
            Self::Quarterly => "Quarterly",
            Self::Annually => "Annually",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "Daily" => Ok(Self::Daily),
            "Monthly" => Ok(Self::Monthly),
            "Quarterly" => Ok(Self::Quarterly),
            "Annually" => Ok(Self::Annually),
            other => anyhow::bail!("unknown compounding period: {other}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LoanDirection {
    Borrowed,
    Lent,
}

impl LoanDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Borrowed => "Borrowed",
            Self::Lent => "Lent",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "Borrowed" => Ok(Self::Borrowed),
            "Lent" => Ok(Self::Lent),
            other => anyhow::bail!("unknown loan direction: {other}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SavingsDetails {
    #[allow(dead_code)]
    pub account_id: Uuid,
    pub interest_rate: Decimal,
    pub compounding_period: CompoundingPeriod,
}

#[derive(Debug, Clone)]
pub struct LoanDetails {
    #[allow(dead_code)]
    pub account_id: Uuid,
    pub counterparty: String,
    pub direction: LoanDirection,
    pub interest_rate: Option<Decimal>,
    pub due_date: Option<chrono::NaiveDate>,
}

#[derive(Debug, Clone)]
pub struct InvestmentDetails {
    #[allow(dead_code)]
    pub account_id: Uuid,
    pub broker: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BinanceDetails {
    #[allow(dead_code)]
    pub account_id: Uuid,
    pub label: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AccountDetails {
    Savings(SavingsDetails),
    Loan(LoanDetails),
    Investment(InvestmentDetails),
    Binance(BinanceDetails),
    None,
}

#[async_trait::async_trait]
pub trait AccountRepository: Send + Sync {
    async fn create(&self, account: &Account, details: &AccountDetails) -> anyhow::Result<()>;
    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<(Account, AccountDetails)>>;
    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<(Account, AccountDetails)>>;
    async fn update(&self, account: &Account, details: &AccountDetails) -> anyhow::Result<()>;
    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()>;
    async fn compute_balance(&self, account_id: Uuid, user_id: Uuid) -> anyhow::Result<Decimal>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_new_sets_type() {
        let user_id = Uuid::new_v4();
        let acct = Account::new(
            user_id,
            "My Cash".to_string(),
            AccountType::Cash,
            "USD".to_string(),
        );
        assert!(matches!(acct.account_type, AccountType::Cash));
    }

    #[test]
    fn account_type_roundtrip() {
        for t in [
            AccountType::Cash,
            AccountType::Bank,
            AccountType::Savings,
            AccountType::Loan,
            AccountType::Investment,
            AccountType::Binance,
        ] {
            assert_eq!(AccountType::from_str(t.as_str()).unwrap(), t);
        }
    }

    #[test]
    fn compounding_period_roundtrip() {
        for p in [
            CompoundingPeriod::Daily,
            CompoundingPeriod::Monthly,
            CompoundingPeriod::Quarterly,
            CompoundingPeriod::Annually,
        ] {
            assert_eq!(CompoundingPeriod::from_str(p.as_str()).unwrap(), p);
        }
    }

    #[test]
    fn loan_direction_roundtrip() {
        for d in [LoanDirection::Borrowed, LoanDirection::Lent] {
            assert_eq!(LoanDirection::from_str(d.as_str()).unwrap(), d);
        }
    }
}
