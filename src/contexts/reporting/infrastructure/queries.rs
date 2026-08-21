use crate::shared_kernel::UserId;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
pub(crate) async fn read_rows(
    pool: &PgPool,
    user: UserId,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    kind: &str,
) -> Result<Vec<serde_json::Value>, sqlx::Error> {
    let sql = match kind {
        "balance_history" => {
            "SELECT jsonb_build_object('account_id',account_id,'currency',currency,'balance',balance::text,'effective_at',effective_at) AS value FROM reporting.balance_history WHERE user_id=$1 AND effective_at >= $2 AND effective_at < $3 ORDER BY effective_at,account_id"
        }
        "cashflow" => {
            "SELECT jsonb_build_object('journal_entry_id',journal_entry_id,'flow_kind',flow_kind,'amount',amount::text,'currency',currency,'effective_at',effective_at) AS value FROM reporting.cashflows WHERE user_id=$1 AND effective_at >= $2 AND effective_at < $3 AND NOT reversed ORDER BY effective_at,journal_entry_id"
        }
        "spending" => {
            "SELECT jsonb_build_object('journal_entry_id',journal_entry_id,'flow_kind',flow_kind,'amount',amount::text,'currency',currency,'effective_at',effective_at) AS value FROM reporting.cashflows WHERE user_id=$1 AND effective_at >= $2 AND effective_at < $3 AND flow_kind='expense' AND NOT reversed ORDER BY effective_at,journal_entry_id"
        }
        "liabilities" => {
            "SELECT jsonb_build_object('account_id',account_id,'currency',currency,'balance',balance::text) AS value FROM reporting.account_balances WHERE user_id=$1 AND account_kind='liability' AND $2 <= $3 ORDER BY account_id"
        }
        "reconciliations" => {
            "SELECT jsonb_build_object('case_id',case_id,'state',state,'case_version',case_version,'updated_at',updated_at) AS value FROM reporting.reconciliations WHERE user_id=$1 AND updated_at >= $2 AND updated_at < $3 ORDER BY updated_at,case_id"
        }
        "recurring" => {
            "SELECT jsonb_build_object('subscription_id',subscription_id,'currency',currency,'total',total::text,'charge_count',charge_count) AS value FROM reporting.recurring_summary WHERE user_id=$1 AND $2 <= $3 ORDER BY subscription_id,currency"
        }
        "net_worth" => {
            "SELECT jsonb_build_object('account_id',account_id,'currency',currency,'balance',balance::text,'account_kind',account_kind) AS value FROM reporting.account_balances WHERE user_id=$1 AND $2 <= $3 ORDER BY account_id"
        }
        _ => return Ok(vec![]),
    };
    sqlx::query(sql)
        .bind(user.into_uuid())
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| r.try_get("value"))
        .collect()
}
