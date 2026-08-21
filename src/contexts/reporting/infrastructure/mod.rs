mod loan_projection;
pub(crate) mod projections;
pub(crate) mod queries;
pub(crate) mod sharing_projection;
use super::public::{
    ConversionStatus, ProjectionApplyResult, ReportMetadata, ReportRange, ReportResponse,
};
use crate::contexts::ledger::public::{LedgerEventFactV1, LedgerEventV1};
use crate::contexts::reference_data::public::{FX_OBSERVED_V1, FxObservedV1};
use crate::shared_kernel::UserId;
use chrono::Utc;
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::BTreeMap;
use std::str::FromStr;
#[derive(Clone)]
pub(crate) struct PgReportingStore {
    pub(crate) pool: PgPool,
}
impl PgReportingStore {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub(crate) async fn read(
        &self,
        user: UserId,
        range: ReportRange,
        kind: &'static str,
    ) -> Result<ReportResponse, sqlx::Error> {
        let checkpoint = sqlx::query(
            "SELECT COALESCE(MAX(last_sequence),0) AS sequence,MIN(updated_at) AS updated_at FROM reporting.checkpoints",
        )
        .fetch_one(&self.pool)
        .await?;
        let sequence = checkpoint.get::<i64, _>("sequence");
        let checkpoint_at: Option<chrono::DateTime<Utc>> = checkpoint.get("updated_at");
        let now = Utc::now();
        let mut rows = queries::read_rows(&self.pool, user, range.from, range.to, kind).await?;
        let source_currency = rows
            .iter()
            .filter_map(|row| row.get("currency").and_then(|v| v.as_str()))
            .next()
            .and_then(|v| crate::shared_kernel::CurrencyCode::new(v).ok());
        let conversion_status = if let Some(base) = range.base_currency.as_ref() {
            convert_rows(&self.pool, &mut rows, base, range.to).await?
        } else {
            ConversionStatus::NotRequested
        };
        Ok(ReportResponse {
            metadata: ReportMetadata {
                as_of: now,
                projection_sequence: u64::try_from(sequence).unwrap_or_default(),
                lag_seconds: checkpoint_at
                    .and_then(|value| (now - value).num_seconds().try_into().ok())
                    .unwrap_or_default(),
                source_currency,
                base_currency: range.base_currency,
                conversion_status,
            },
            rows,
        })
    }

    pub(crate) async fn apply_ledger_event(
        &self,
        event: LedgerEventV1,
    ) -> Result<ProjectionApplyResult, sqlx::Error> {
        if let Err(reason) = super::application::projectors::classify(&event) {
            sqlx::query("INSERT INTO reporting.dead_letters(id,consumer_name,event_id,event_type,reason,recorded_at) VALUES($1,'reporting-ledger-v1',$2,$3,$4,$5) ON CONFLICT(consumer_name,event_id) DO NOTHING")
                .bind(uuid::Uuid::new_v4()).bind(event.metadata.event_id.into_uuid())
                .bind("ledger.unknown").bind(reason).bind(Utc::now()).execute(&self.pool).await?;
            return Err(sqlx::Error::Protocol(reason.into()));
        }
        let sequence = sequence_i64(event.metadata.sequence)?;
        let event_id = event.metadata.event_id.into_uuid();
        let user_id = event.metadata.user_id.into_uuid();
        let event_type = ledger_event_type(&event.fact);
        let digest = payload_digest(&event);
        let mut transaction = self.pool.begin().await?;

        if !claim_event(
            &mut transaction,
            "reporting-ledger-v1",
            event_id,
            event_type,
            sequence,
            &digest,
        )
        .await?
        {
            return Ok(ProjectionApplyResult {
                applied: false,
                sequence: event.metadata.sequence,
            });
        }

        match event.fact {
            LedgerEventFactV1::BalanceChanged {
                account_id,
                balance,
                ..
            } => {
                sqlx::query(
                    r#"
                    INSERT INTO reporting.account_balances
                        (user_id, account_id, currency, account_kind, balance, as_of, source_sequence)
                    VALUES ($1, $2, $3, 'unknown', $4, $5, $6)
                    ON CONFLICT (user_id, account_id) DO UPDATE SET
                        currency = EXCLUDED.currency,
                        balance = EXCLUDED.balance,
                        as_of = EXCLUDED.as_of,
                        source_sequence = EXCLUDED.source_sequence
                    WHERE reporting.account_balances.source_sequence < EXCLUDED.source_sequence
                    "#,
                )
                .bind(user_id)
                .bind(account_id.into_uuid())
                .bind(balance.currency.as_str())
                .bind(balance.amount)
                .bind(event.metadata.occurred_at)
                .bind(sequence)
                .execute(&mut *transaction)
                .await?;
            }
            fact if reconciliation_state(&fact).is_some() => {
                let (case_id, state) = reconciliation_state(&fact)
                    .expect("guarded reconciliation event must contain a state");
                sqlx::query("INSERT INTO reporting.reconciliation_history(user_id,case_id,state,case_version,ledger_event_sequence,event_id,occurred_at) VALUES($1,$2,$3,$4,$4,$5,$6) ON CONFLICT(event_id) DO NOTHING")
                    .bind(user_id).bind(case_id).bind(state).bind(sequence).bind(event_id)
                    .bind(event.metadata.occurred_at).execute(&mut *transaction).await?;
                sqlx::query(
                    r#"
                    INSERT INTO reporting.reconciliations
                        (user_id, case_id, state, case_version, balance_version,
                         observation_sequence, ledger_event_sequence, event_id, updated_at)
                    VALUES ($1, $2, $3, $4, $4, $4, $4, $5, $6)
                    ON CONFLICT (user_id, case_id) DO UPDATE SET
                        state = EXCLUDED.state,
                        case_version = EXCLUDED.case_version,
                        balance_version = EXCLUDED.balance_version,
                        observation_sequence = EXCLUDED.observation_sequence,
                        ledger_event_sequence = EXCLUDED.ledger_event_sequence,
                        event_id = EXCLUDED.event_id,
                        updated_at = EXCLUDED.updated_at
                    WHERE (reporting.reconciliations.case_version,
                           reporting.reconciliations.ledger_event_sequence,
                           reporting.reconciliations.event_id)
                        < (EXCLUDED.case_version, EXCLUDED.ledger_event_sequence, EXCLUDED.event_id)
                    "#,
                )
                .bind(user_id)
                .bind(case_id)
                .bind(state)
                .bind(sequence)
                .bind(event_id)
                .bind(event.metadata.occurred_at)
                .execute(&mut *transaction)
                .await?;
            }
            LedgerEventFactV1::EntryReversed {
                original_journal_entry_id,
                ..
            } => {
                sqlx::query(
                    r#"
                    UPDATE reporting.cashflows
                    SET reversed = true, source_sequence = $3
                    WHERE user_id = $1 AND journal_entry_id = $2 AND source_sequence < $3
                    "#,
                )
                .bind(user_id)
                .bind(original_journal_entry_id.into_uuid())
                .bind(sequence)
                .execute(&mut *transaction)
                .await?;
            }
            _ => {}
        }

        save_checkpoint(&mut transaction, "reporting-ledger-v1", sequence).await?;
        transaction.commit().await?;
        Ok(ProjectionApplyResult {
            applied: true,
            sequence: event.metadata.sequence,
        })
    }

    pub(crate) async fn apply_fx_event(
        &self,
        event: FxObservedV1,
        source_sequence: u64,
    ) -> Result<ProjectionApplyResult, sqlx::Error> {
        let sequence = sequence_i64(source_sequence)?;
        let digest = payload_digest(&event);
        let mut transaction = self.pool.begin().await?;
        if !claim_event(
            &mut transaction,
            "reporting-reference-fx-v1",
            event.observation_id,
            FX_OBSERVED_V1,
            sequence,
            &digest,
        )
        .await?
        {
            return Ok(ProjectionApplyResult {
                applied: false,
                sequence: source_sequence,
            });
        }

        sqlx::query(
            r#"
            INSERT INTO reporting.fx_rates
                (observation_id, source, source_revision, base_currency, quote_currency,
                 rate, effective_at, observed_at, source_sequence)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
            ON CONFLICT (observation_id) DO NOTHING
            "#,
        )
        .bind(event.observation_id)
        .bind(event.source)
        .bind(event.source_revision)
        .bind(event.base_currency.as_str())
        .bind(event.quote_currency.as_str())
        .bind(event.rate)
        .bind(event.effective_at)
        .bind(event.observed_at)
        .bind(sequence)
        .execute(&mut *transaction)
        .await?;
        save_checkpoint(&mut transaction, "reporting-reference-fx-v1", sequence).await?;
        transaction.commit().await?;
        Ok(ProjectionApplyResult {
            applied: true,
            sequence: source_sequence,
        })
    }

    pub(crate) async fn apply_journal_export(
        &self,
        event_id: crate::shared_kernel::EventId,
        source_sequence: u64,
        journal: crate::contexts::ledger::public::JournalView,
    ) -> Result<ProjectionApplyResult, sqlx::Error> {
        let sequence = sequence_i64(source_sequence)?;
        let digest = payload_digest(&journal);
        let user_id = journal.user_id.into_uuid();
        let journal_id = journal.id.into_uuid();
        let mut transaction = self.pool.begin().await?;
        if !claim_event(
            &mut transaction,
            "reporting-ledger-journals-v1",
            event_id.into_uuid(),
            "ledger.journal-posted.v1",
            sequence,
            &digest,
        )
        .await?
        {
            return Ok(ProjectionApplyResult {
                applied: false,
                sequence: source_sequence,
            });
        }

        if let Some(original) = journal.relations.reverses() {
            sqlx::query("UPDATE reporting.cashflows SET reversed=true,source_sequence=$3 WHERE user_id=$1 AND journal_entry_id=$2 AND source_sequence<$3")
                .bind(user_id).bind(original.into_uuid()).bind(sequence)
                .execute(&mut *transaction).await?;
        }

        let mut flows: BTreeMap<(&'static str, String), Decimal> = BTreeMap::new();
        for posting in &journal.postings {
            if posting.account_kind != crate::contexts::ledger::public::AccountKind::System {
                let nature = account_nature(posting.account_nature);
                let updated = sqlx::query(
                    r#"
                    INSERT INTO reporting.account_balances
                        (user_id,account_id,currency,account_kind,balance,as_of,source_sequence)
                    VALUES($1,$2,$3,$4,$5,$6,$7)
                    ON CONFLICT(user_id,account_id) DO UPDATE SET
                        currency=EXCLUDED.currency,account_kind=EXCLUDED.account_kind,
                        balance=reporting.account_balances.balance+EXCLUDED.balance,
                        as_of=EXCLUDED.as_of,source_sequence=EXCLUDED.source_sequence
                    WHERE reporting.account_balances.source_sequence<EXCLUDED.source_sequence
                    "#,
                )
                .bind(user_id)
                .bind(posting.account_id.into_uuid())
                .bind(posting.currency.as_str())
                .bind(nature)
                .bind(posting.display_effect)
                .bind(journal.occurred_at)
                .bind(sequence)
                .execute(&mut *transaction)
                .await?;
                if updated.rows_affected() == 1 {
                    let balance: Decimal = sqlx::query_scalar("SELECT balance FROM reporting.account_balances WHERE user_id=$1 AND account_id=$2")
                        .bind(user_id).bind(posting.account_id.into_uuid())
                        .fetch_one(&mut *transaction).await?;
                    sqlx::query("INSERT INTO reporting.balance_history(user_id,account_id,journal_entry_id,currency,balance,effective_at,source_sequence) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(user_id,account_id,journal_entry_id) DO NOTHING")
                        .bind(user_id).bind(posting.account_id.into_uuid()).bind(journal_id)
                        .bind(posting.currency.as_str()).bind(balance).bind(journal.occurred_at)
                        .bind(sequence).execute(&mut *transaction).await?;
                }
            }
            let flow_kind = match posting.account_nature {
                crate::contexts::ledger::public::AccountNature::Income => Some("income"),
                crate::contexts::ledger::public::AccountNature::Expense => Some("expense"),
                _ => None,
            };
            if let Some(flow_kind) = flow_kind {
                let amount = if flow_kind == "income" {
                    -posting.signed_amount
                } else {
                    posting.signed_amount
                };
                if amount > Decimal::ZERO {
                    let key = (flow_kind, posting.currency.to_string());
                    let current = flows.entry(key).or_insert(Decimal::ZERO);
                    *current = current.checked_add(amount).ok_or_else(|| {
                        sqlx::Error::Protocol("reporting cashflow overflowed".into())
                    })?;
                }
            }
        }
        if journal.relations.reverses().is_none() {
            for ((flow_kind, currency), amount) in flows {
                sqlx::query("INSERT INTO reporting.cashflows(user_id,journal_entry_id,flow_kind,amount,currency,category_id,effective_at,reversed,source_sequence) VALUES($1,$2,$3,$4,$5,$6,$7,false,$8) ON CONFLICT(user_id,journal_entry_id,flow_kind) DO UPDATE SET amount=EXCLUDED.amount,currency=EXCLUDED.currency,category_id=EXCLUDED.category_id,effective_at=EXCLUDED.effective_at,source_sequence=EXCLUDED.source_sequence WHERE reporting.cashflows.source_sequence<EXCLUDED.source_sequence")
                    .bind(user_id).bind(journal_id).bind(flow_kind).bind(amount).bind(currency)
                    .bind(journal.category_id.map(|id|id.into_uuid())).bind(journal.occurred_at)
                    .bind(sequence).execute(&mut *transaction).await?;
            }
        }
        save_checkpoint(&mut transaction, "reporting-ledger-journals-v1", sequence).await?;
        transaction.commit().await?;
        Ok(ProjectionApplyResult {
            applied: true,
            sequence: source_sequence,
        })
    }

    pub(crate) async fn apply_recurring_charge(
        &self,
        event_id: crate::shared_kernel::EventId,
        source_sequence: u64,
        event: crate::contexts::recurring::public::ChargeEvidenceRecordedV1,
    ) -> Result<ProjectionApplyResult, sqlx::Error> {
        let sequence = sequence_i64(source_sequence)?;
        let digest = payload_digest(&event);
        let mut transaction = self.pool.begin().await?;
        if !claim_event(
            &mut transaction,
            "reporting-recurring-charges-v1",
            event_id.into_uuid(),
            crate::contexts::recurring::public::CHARGE_EVIDENCE_RECORDED_V1,
            sequence,
            &digest,
        )
        .await?
        {
            return Ok(ProjectionApplyResult {
                applied: false,
                sequence: source_sequence,
            });
        }
        if let Some(money) = event.money {
            sqlx::query(
                r#"
                INSERT INTO reporting.recurring_summary
                    (user_id,subscription_id,currency,total,charge_count,last_charge_at,source_sequence)
                VALUES($1,$2,$3,$4,1,$5,$6)
                ON CONFLICT(user_id,subscription_id,currency) DO UPDATE SET
                    total=reporting.recurring_summary.total+EXCLUDED.total,
                    charge_count=reporting.recurring_summary.charge_count+1,
                    last_charge_at=GREATEST(reporting.recurring_summary.last_charge_at,EXCLUDED.last_charge_at),
                    source_sequence=EXCLUDED.source_sequence
                WHERE reporting.recurring_summary.source_sequence<EXCLUDED.source_sequence
                "#,
            )
            .bind(event.user_id.into_uuid())
            .bind(event.subscription_id.into_uuid())
            .bind(money.currency().as_str())
            .bind(money.amount())
            .bind(event.charged_at)
            .bind(sequence)
            .execute(&mut *transaction)
            .await?;
        }
        save_checkpoint(&mut transaction, "reporting-recurring-charges-v1", sequence).await?;
        transaction.commit().await?;
        Ok(ProjectionApplyResult {
            applied: true,
            sequence: source_sequence,
        })
    }

    pub(crate) async fn rebuild_journals(
        &self,
        mut journals: Vec<(
            crate::shared_kernel::EventId,
            u64,
            crate::contexts::ledger::public::JournalView,
        )>,
    ) -> Result<(), sqlx::Error> {
        journals.sort_by_key(|(_, sequence, _)| *sequence);
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM reporting.balance_history")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM reporting.cashflows")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM reporting.account_balances")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM reporting.reconciliations")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM reporting.reconciliation_history")
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "DELETE FROM reporting.consumed_events WHERE consumer_name LIKE 'reporting-ledger%'",
        )
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "DELETE FROM reporting.checkpoints WHERE consumer_name LIKE 'reporting-ledger%'",
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        for (event_id, sequence, journal) in journals {
            self.apply_journal_export(event_id, sequence, journal)
                .await?;
        }
        Ok(())
    }
}

fn account_nature(nature: crate::contexts::ledger::public::AccountNature) -> &'static str {
    match nature {
        crate::contexts::ledger::public::AccountNature::Asset => "asset",
        crate::contexts::ledger::public::AccountNature::Liability => "liability",
        crate::contexts::ledger::public::AccountNature::Equity => "equity",
        crate::contexts::ledger::public::AccountNature::Income => "income",
        crate::contexts::ledger::public::AccountNature::Expense => "expense",
    }
}

fn sequence_i64(sequence: u64) -> Result<i64, sqlx::Error> {
    i64::try_from(sequence).map_err(|_| sqlx::Error::Protocol("event sequence exceeds i64".into()))
}

fn payload_digest(value: &impl serde::Serialize) -> [u8; 32] {
    Sha256::digest(serde_json::to_vec(value).expect("integration events serialize")).into()
}

async fn claim_event(
    transaction: &mut Transaction<'_, Postgres>,
    consumer: &str,
    event_id: uuid::Uuid,
    event_type: &str,
    source_sequence: i64,
    digest: &[u8; 32],
) -> Result<bool, sqlx::Error> {
    let inserted = sqlx::query(
        r#"
        INSERT INTO reporting.consumed_events
            (consumer_name,event_id,event_type,source_sequence,payload_digest,processed_at)
        VALUES ($1,$2,$3,$4,$5,$6)
        ON CONFLICT (consumer_name,event_id) DO NOTHING
        "#,
    )
    .bind(consumer)
    .bind(event_id)
    .bind(event_type)
    .bind(source_sequence)
    .bind(digest.as_slice())
    .bind(Utc::now())
    .execute(&mut **transaction)
    .await?;
    if inserted.rows_affected() == 1 {
        return Ok(true);
    }
    let existing: Vec<u8> = sqlx::query_scalar(
        "SELECT payload_digest FROM reporting.consumed_events WHERE consumer_name=$1 AND event_id=$2",
    )
    .bind(consumer)
    .bind(event_id)
    .fetch_one(&mut **transaction)
    .await?;
    if existing.as_slice() != digest {
        return Err(sqlx::Error::Protocol(
            "event id was replayed with a different payload".into(),
        ));
    }
    Ok(false)
}

async fn save_checkpoint(
    transaction: &mut Transaction<'_, Postgres>,
    consumer: &str,
    sequence: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO reporting.checkpoints (consumer_name,last_sequence,updated_at)
        VALUES ($1,$2,$3)
        ON CONFLICT (consumer_name) DO UPDATE SET
            last_sequence=GREATEST(reporting.checkpoints.last_sequence,EXCLUDED.last_sequence),
            updated_at=EXCLUDED.updated_at
        "#,
    )
    .bind(consumer)
    .bind(sequence)
    .bind(Utc::now())
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn ledger_event_type(fact: &LedgerEventFactV1) -> &'static str {
    match fact {
        LedgerEventFactV1::AccountLifecycleChanged { .. } => "ledger.account-lifecycle-changed.v1",
        LedgerEventFactV1::EntryPosted { .. } => "ledger.journal-posted.v1",
        LedgerEventFactV1::EntryReversed { .. } => "ledger.journal-reversed.v1",
        LedgerEventFactV1::EntryReplaced { .. } => "ledger.journal-replaced.v1",
        LedgerEventFactV1::AnnotationChanged { .. } => "ledger.annotation-changed.v1",
        LedgerEventFactV1::BalanceChanged { .. } => "ledger.balance-changed.v1",
        LedgerEventFactV1::ReconciliationObserved { .. } => "ledger.reconciliation-observed.v1",
        LedgerEventFactV1::ReconciliationMatched { .. } => "ledger.reconciliation-matched.v1",
        LedgerEventFactV1::ReconciliationSuperseded { .. } => "ledger.reconciliation-superseded.v1",
        LedgerEventFactV1::ReconciliationIgnoredOlder { .. } => {
            "ledger.reconciliation-ignored-older.v1"
        }
        LedgerEventFactV1::ReconciliationApproved { .. } => "ledger.reconciliation-approved.v1",
        LedgerEventFactV1::ReconciliationDismissed { .. } => "ledger.reconciliation-dismissed.v1",
        LedgerEventFactV1::ReconciliationStale { .. } => "ledger.reconciliation-stale.v1",
        LedgerEventFactV1::InternalAccountingCommandPosted { .. } => {
            "ledger.internal-accounting-command-posted.v1"
        }
        LedgerEventFactV1::InternalAccountingCommandFailed { .. } => {
            "ledger.internal-accounting-command-failed.v1"
        }
    }
}

fn reconciliation_state(fact: &LedgerEventFactV1) -> Option<(uuid::Uuid, &'static str)> {
    let (case_id, state) = match fact {
        LedgerEventFactV1::ReconciliationObserved { case_id } => (case_id, "observed"),
        LedgerEventFactV1::ReconciliationMatched { case_id } => (case_id, "matched"),
        LedgerEventFactV1::ReconciliationSuperseded { case_id } => (case_id, "superseded"),
        LedgerEventFactV1::ReconciliationIgnoredOlder { case_id } => (case_id, "ignored_older"),
        LedgerEventFactV1::ReconciliationApproved { case_id, .. } => (case_id, "approved"),
        LedgerEventFactV1::ReconciliationDismissed { case_id } => (case_id, "dismissed"),
        LedgerEventFactV1::ReconciliationStale { case_id } => (case_id, "stale"),
        _ => return None,
    };
    Some((case_id.into_uuid(), state))
}

async fn convert_rows(
    pool: &PgPool,
    rows: &mut [serde_json::Value],
    base: &crate::shared_kernel::CurrencyCode,
    as_of: chrono::DateTime<Utc>,
) -> Result<ConversionStatus, sqlx::Error> {
    let mut converted = 0usize;
    let mut missing = 0usize;
    for row in rows.iter_mut() {
        let Some(object) = row.as_object_mut() else {
            continue;
        };
        let Some(source) = object.get("currency").and_then(|v| v.as_str()) else {
            continue;
        };
        if source == base.as_str() {
            object.insert("conversion_status".into(), serde_json::json!("complete"));
            converted += 1;
            continue;
        }
        let rate:Option<Decimal>=sqlx::query_scalar("SELECT rate FROM reporting.fx_rates WHERE base_currency=$1 AND quote_currency=$2 AND effective_at<=$3 ORDER BY effective_at DESC,observed_at DESC,source_sequence DESC LIMIT 1").bind(source).bind(base.as_str()).bind(as_of).fetch_optional(pool).await?;
        let rate = if let Some(rate) = rate {
            Some(rate)
        } else {
            sqlx::query_scalar::<_,Decimal>("SELECT rate FROM reporting.fx_rates WHERE base_currency=$2 AND quote_currency=$1 AND effective_at<=$3 ORDER BY effective_at DESC,observed_at DESC,source_sequence DESC LIMIT 1").bind(source).bind(base.as_str()).bind(as_of).fetch_optional(pool).await?.and_then(|r|Decimal::ONE.checked_div(r))
        };
        let rate = if rate.is_some() || source == "UAH" || base.as_str() == "UAH" {
            rate
        } else {
            let source_to_uah: Option<Decimal> = sqlx::query_scalar("SELECT rate FROM reporting.fx_rates WHERE base_currency=$1 AND quote_currency='UAH' AND effective_at<=$2 ORDER BY effective_at DESC,observed_at DESC,source_sequence DESC LIMIT 1")
                .bind(source).bind(as_of).fetch_optional(pool).await?;
            let base_to_uah: Option<Decimal> = sqlx::query_scalar("SELECT rate FROM reporting.fx_rates WHERE base_currency=$1 AND quote_currency='UAH' AND effective_at<=$2 ORDER BY effective_at DESC,observed_at DESC,source_sequence DESC LIMIT 1")
                .bind(base.as_str()).bind(as_of).fetch_optional(pool).await?;
            source_to_uah
                .zip(base_to_uah)
                .and_then(|(left, right)| left.checked_div(right))
        };
        let Some(rate) = rate else {
            object.insert(
                "conversion_status".into(),
                serde_json::json!("missing_historical_rate"),
            );
            missing += 1;
            continue;
        };
        for field in ["amount", "balance", "total"] {
            if let Some(value) = object
                .get(field)
                .and_then(|v| v.as_str())
                .and_then(|v| Decimal::from_str(v).ok())
                && let Some(converted_amount) = value.checked_mul(rate)
            {
                object.insert(
                    format!("base_{field}"),
                    serde_json::json!(converted_amount.to_string()),
                );
            }
        }
        object.insert("base_currency".into(), serde_json::json!(base.as_str()));
        object.insert("conversion_status".into(), serde_json::json!("complete"));
        converted += 1;
    }
    Ok(match (converted, missing) {
        (_, 0) => ConversionStatus::Complete,
        (0, _) => ConversionStatus::MissingHistoricalRate,
        _ => ConversionStatus::Partial,
    })
}
