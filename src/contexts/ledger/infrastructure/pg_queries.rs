//! Pool-backed read-only Ledger query adapter.

use rust_decimal::Decimal;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::infrastructure::v2_db::VerifiedV2Pool;
use crate::shared_kernel::{CorrelationId, CurrencyCode, Money, UserId};

use super::super::{
    domain::{
        AccountAuthority, AccountKind, AccountNature, Actor, AnnotationVersion, BalanceVersion,
        JournalEntryId, JournalRelations, JournalSource, LedgerAccountId, LedgerError,
        ObservationId, PostingId, PostingPurpose, ReconciliationCaseId, ReconciliationStatus,
        ReconciliationVersion, SourceReference,
    },
    public::{
        AccountView, ActivityCursor, CorrectionView, JournalView, PostingView, ReconciliationView,
    },
};
use super::rows::AccountRow;

/// SELECT-only accounting-fact queries.
#[derive(Clone)]
pub(crate) struct PgLedgerQueries {
    pool: PgPool,
}

impl PgLedgerQueries {
    pub(crate) fn new(pool: &VerifiedV2Pool) -> Self {
        Self {
            pool: pool.pool().clone(),
        }
    }

    pub(crate) async fn list_accounts(
        &self,
        user_id: UserId,
    ) -> Result<Vec<AccountView>, LedgerError> {
        let rows = sqlx::query_as::<_, AccountBalanceRow>(
            "SELECT a.id, a.user_id, a.name, a.currency, a.nature, a.kind, a.authority, \
                    a.visibility, a.lifecycle, a.system_role, a.version, a.created_at, a.updated_at, \
                    b.signed_balance, b.version AS balance_version, b.as_of \
             FROM ledger.accounts a JOIN ledger.account_balances b \
               ON b.account_id = a.id AND b.user_id = a.user_id \
             WHERE a.user_id = $1 AND a.visibility = 'user_visible' \
             ORDER BY lower(a.name), a.id",
        ).bind(user_id.into_uuid()).fetch_all(&self.pool).await.map_err(LedgerError::database)?;
        rows.into_iter().map(AccountBalanceRow::into_view).collect()
    }

    pub(crate) async fn get_account(
        &self,
        user_id: UserId,
        id: LedgerAccountId,
    ) -> Result<AccountView, LedgerError> {
        let row = sqlx::query_as::<_, AccountBalanceRow>(
            "SELECT a.id, a.user_id, a.name, a.currency, a.nature, a.kind, a.authority, \
                    a.visibility, a.lifecycle, a.system_role, a.version, a.created_at, a.updated_at, \
                    b.signed_balance, b.version AS balance_version, b.as_of \
             FROM ledger.accounts a JOIN ledger.account_balances b \
               ON b.account_id = a.id AND b.user_id = a.user_id \
             WHERE a.user_id = $1 AND a.id = $2 AND a.visibility = 'user_visible'",
        ).bind(user_id.into_uuid()).bind(id.into_uuid())
         .fetch_optional(&self.pool).await.map_err(LedgerError::database)?
         .ok_or_else(LedgerError::not_found)?;
        row.into_view()
    }

    pub(crate) async fn account_activity(
        &self,
        user_id: UserId,
        account_id: LedgerAccountId,
        after: Option<ActivityCursor>,
        limit: u32,
    ) -> Result<Vec<JournalView>, LedgerError> {
        if limit == 0 || limit > 200 {
            return Err(LedgerError::invalid_state(
                "activity limit must be 1 to 200",
            ));
        }
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT j.id FROM ledger.journal_entries j \
             JOIN ledger.postings p ON p.journal_entry_id = j.id AND p.user_id = j.user_id \
             WHERE j.user_id = $1 AND p.account_id = $2 \
               AND ($3::timestamptz IS NULL OR (j.occurred_at, j.ledger_sequence) < ($3, $4)) \
             GROUP BY j.id, j.occurred_at, j.ledger_sequence \
             ORDER BY j.occurred_at DESC, j.ledger_sequence DESC LIMIT $5",
        )
        .bind(user_id.into_uuid())
        .bind(account_id.into_uuid())
        .bind(after.map(|cursor| cursor.occurred_at))
        .bind(after.map(|cursor| cursor.ledger_sequence))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(LedgerError::database)?;
        let mut views = Vec::with_capacity(ids.len());
        for id in ids {
            views.push(self.get_journal(user_id, JournalEntryId::new(id)).await?);
        }
        Ok(views)
    }

    pub(crate) async fn list_journals(
        &self,
        user_id: UserId,
        after: Option<ActivityCursor>,
        limit: u32,
    ) -> Result<Vec<JournalView>, LedgerError> {
        if limit == 0 || limit > 200 {
            return Err(LedgerError::invalid_state(
                "activity limit must be 1 to 200",
            ));
        }
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM ledger.journal_entries WHERE user_id = $1 \
             AND ($2::timestamptz IS NULL OR (occurred_at, ledger_sequence) < ($2, $3)) \
             ORDER BY occurred_at DESC, ledger_sequence DESC LIMIT $4",
        )
        .bind(user_id.into_uuid())
        .bind(after.map(|cursor| cursor.occurred_at))
        .bind(after.map(|cursor| cursor.ledger_sequence))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(LedgerError::database)?;
        let mut views = Vec::with_capacity(ids.len());
        for id in ids {
            views.push(self.get_journal(user_id, JournalEntryId::new(id)).await?);
        }
        Ok(views)
    }

    pub(crate) async fn get_journal(
        &self,
        user_id: UserId,
        id: JournalEntryId,
    ) -> Result<JournalView, LedgerError> {
        #[derive(FromRow)]
        struct JournalRow {
            id: Uuid,
            user_id: Uuid,
            ledger_sequence: i64,
            source: String,
            purpose: String,
            description: String,
            actor_kind: String,
            actor_reference: Option<String>,
            occurred_at: chrono::DateTime<chrono::Utc>,
            recorded_at: chrono::DateTime<chrono::Utc>,
            correlation_id: Uuid,
            reverses_transaction_id: Option<Uuid>,
            corrects_transaction_id: Option<Uuid>,
            replaces_transaction_id: Option<Uuid>,
            annotation_version: Option<i64>,
            category_id: Option<Uuid>,
            correction_account_id: Option<Uuid>,
            correction_before: Option<Decimal>,
            correction_target: Option<Decimal>,
            correction_delta: Option<Decimal>,
            correction_balance_version: Option<i64>,
            correction_reason: Option<String>,
            correction_observed_at: Option<chrono::DateTime<chrono::Utc>>,
        }
        let row = sqlx::query_as::<_, JournalRow>(
            "SELECT j.id, j.user_id, j.ledger_sequence, j.source, j.purpose, j.description, j.actor_kind, j.actor_reference, j.occurred_at, \
                    j.recorded_at, j.correlation_id, j.reverses_transaction_id, \
                    j.corrects_transaction_id, j.replaces_transaction_id, a.version AS annotation_version, a.category_id, \
                    c.account_id AS correction_account_id, c.before_display_balance AS correction_before, \
                    c.target_display_balance AS correction_target, c.display_delta AS correction_delta, \
                    c.observed_balance_version AS correction_balance_version, c.reason AS correction_reason, \
                    c.observed_at AS correction_observed_at \
             FROM ledger.journal_entries j LEFT JOIN ledger.transaction_annotations a \
               ON a.journal_entry_id = j.id AND a.user_id = j.user_id \
             LEFT JOIN ledger.balance_correction_details c ON c.journal_entry_id = j.id AND c.user_id = j.user_id \
             WHERE j.id = $1 AND j.user_id = $2",
        ).bind(id.into_uuid()).bind(user_id.into_uuid())
         .fetch_optional(&self.pool).await.map_err(LedgerError::database)?
         .ok_or_else(LedgerError::not_found)?;
        #[derive(FromRow)]
        struct PostingRow {
            id: Uuid,
            account_id: Uuid,
            account_kind: String,
            account_authority: String,
            position: i16,
            currency: String,
            account_nature: String,
            signed_amount: Decimal,
        }
        let posting_rows = sqlx::query_as::<_, PostingRow>(
            "SELECT p.id, p.account_id, p.position, p.currency, p.account_nature, p.signed_amount, \
                    a.kind AS account_kind,a.authority AS account_authority \
             FROM ledger.postings p JOIN ledger.accounts a ON a.id=p.account_id AND a.user_id=p.user_id \
             WHERE p.journal_entry_id = $1 AND p.user_id = $2 ORDER BY p.position",
        )
        .bind(id.into_uuid())
        .bind(user_id.into_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(LedgerError::database)?;
        let postings = posting_rows
            .into_iter()
            .map(|posting| {
                let nature = AccountNature::parse(&posting.account_nature)?;
                Ok(PostingView {
                    id: PostingId::new(posting.id),
                    account_id: LedgerAccountId::new(posting.account_id),
                    account_kind: AccountKind::parse(&posting.account_kind)?,
                    account_nature: nature,
                    account_authority: AccountAuthority::parse(&posting.account_authority)?,
                    position: u16::try_from(posting.position)
                        .map_err(|_| LedgerError::persistence("stored position invalid"))?,
                    currency: CurrencyCode::new(posting.currency)
                        .map_err(|_| LedgerError::persistence("stored currency invalid"))?,
                    signed_amount: posting.signed_amount,
                    display_effect: posting.signed_amount * Decimal::from(nature.normal_sign()),
                })
            })
            .collect::<Result<Vec<_>, LedgerError>>()?;
        let relations = if let Some(related) = row.reverses_transaction_id {
            JournalRelations::reversal_of(JournalEntryId::new(related))
        } else if let Some(related) = row.corrects_transaction_id {
            JournalRelations::correction_of(JournalEntryId::new(related))
        } else if let Some(related) = row.replaces_transaction_id {
            JournalRelations::replacement_of(JournalEntryId::new(related))
        } else {
            JournalRelations::none()
        };
        Ok(JournalView {
            id: JournalEntryId::new(row.id),
            user_id: UserId::new(row.user_id),
            ledger_sequence: row.ledger_sequence,
            source: match row.source.as_str() {
                "manual" => JournalSource::Manual,
                "import" => JournalSource::Import,
                "system" => JournalSource::System,
                "correction" => JournalSource::Correction,
                "reconciliation" => JournalSource::Reconciliation,
                _ => return Err(LedgerError::persistence("stored source invalid")),
            },
            purpose: PostingPurpose::parse(&row.purpose)?,
            actor: match row.actor_kind.as_str() {
                "user" => Actor::User(UserId::new(
                    Uuid::parse_str(row.actor_reference.as_deref().unwrap_or(""))
                        .map_err(|_| LedgerError::persistence("stored user actor is invalid"))?,
                )),
                "system" => Actor::System,
                "external" => {
                    let reference = row.actor_reference.unwrap_or_default();
                    let (source_kind, source_reference) = reference
                        .split_once(':')
                        .unwrap_or(("external", reference.as_str()));
                    Actor::External {
                        source_kind: source_kind.to_owned(),
                        source_reference: source_reference.to_owned(),
                    }
                }
                _ => return Err(LedgerError::persistence("stored actor kind invalid")),
            },
            description: row.description,
            occurred_at: row.occurred_at,
            recorded_at: row.recorded_at,
            correlation_id: CorrelationId::new(row.correlation_id),
            relations,
            postings,
            annotation_version: row
                .annotation_version
                .map(AnnotationVersion::new)
                .transpose()?,
            category_id: row
                .category_id
                .map(crate::contexts::classification::public::CategoryId::new),
            correction: match (
                row.correction_account_id,
                row.correction_before,
                row.correction_target,
                row.correction_delta,
                row.correction_balance_version,
                row.correction_reason,
                row.correction_observed_at,
            ) {
                (
                    Some(account_id),
                    Some(before_display_balance),
                    Some(target_display_balance),
                    Some(display_delta),
                    Some(observed_balance_version),
                    Some(reason),
                    Some(observed_at),
                ) => Some(CorrectionView {
                    account_id: LedgerAccountId::new(account_id),
                    before_display_balance,
                    target_display_balance,
                    display_delta,
                    observed_balance_version,
                    reason,
                    observed_at,
                }),
                (None, None, None, None, None, None, None) => None,
                _ => {
                    return Err(LedgerError::persistence(
                        "stored correction detail is incomplete",
                    ));
                }
            },
        })
    }

    pub(crate) async fn list_reconciliations(
        &self,
        user_id: UserId,
    ) -> Result<Vec<ReconciliationView>, LedgerError> {
        let rows = sqlx::query_as::<_, ReconciliationViewRow>(
            "SELECT id, account_id, observation_id, source_kind, source_stream_id, source_item_id, \
             observed_at, source_sequence, provider_reported_balance, available_balance, currency, \
             captured_ledger_balance, captured_balance_version, delta, status, version, \
             approval_journal_id, reason, created_at, updated_at FROM ledger.reconciliation_cases \
             WHERE user_id = $1 ORDER BY observed_at DESC, source_sequence DESC, observation_id DESC",
        ).bind(user_id.into_uuid()).fetch_all(&self.pool).await.map_err(LedgerError::database)?;
        rows.into_iter()
            .map(ReconciliationViewRow::into_view)
            .collect()
    }

    pub(crate) async fn get_reconciliation(
        &self,
        user_id: UserId,
        id: ReconciliationCaseId,
    ) -> Result<ReconciliationView, LedgerError> {
        sqlx::query_as::<_, ReconciliationViewRow>(
            "SELECT id, account_id, observation_id, source_kind, source_stream_id, source_item_id, \
             observed_at, source_sequence, provider_reported_balance, available_balance, currency, \
             captured_ledger_balance, captured_balance_version, delta, status, version, \
             approval_journal_id, reason, created_at, updated_at FROM ledger.reconciliation_cases \
             WHERE user_id = $1 AND id = $2",
        )
        .bind(user_id.into_uuid())
        .bind(id.into_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(LedgerError::database)?
        .ok_or_else(LedgerError::not_found)?
        .into_view()
    }
}

#[derive(FromRow)]
struct ReconciliationViewRow {
    id: Uuid,
    account_id: Uuid,
    observation_id: Uuid,
    source_kind: String,
    source_stream_id: String,
    source_item_id: String,
    observed_at: chrono::DateTime<chrono::Utc>,
    source_sequence: i64,
    provider_reported_balance: Decimal,
    available_balance: Option<Decimal>,
    currency: String,
    captured_ledger_balance: Decimal,
    captured_balance_version: i64,
    delta: Decimal,
    status: String,
    version: i64,
    approval_journal_id: Option<Uuid>,
    reason: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl ReconciliationViewRow {
    fn into_view(self) -> Result<ReconciliationView, LedgerError> {
        let currency = CurrencyCode::new(self.currency)
            .map_err(|_| LedgerError::persistence("stored currency invalid"))?;
        let money = |amount| {
            Money::new(amount, currency.clone(), 8)
                .map_err(|error| LedgerError::persistence(error.to_string()))
        };
        Ok(ReconciliationView {
            id: ReconciliationCaseId::new(self.id),
            account_id: LedgerAccountId::new(self.account_id),
            observation_id: ObservationId::new(self.observation_id),
            source: SourceReference::new(
                self.source_kind,
                self.source_stream_id,
                self.source_item_id,
            )?,
            observed_at: self.observed_at,
            source_sequence: self.source_sequence,
            provider_reported: money(self.provider_reported_balance)?,
            available: self.available_balance.map(money).transpose()?,
            captured_ledger_balance: money(self.captured_ledger_balance)?,
            captured_balance_version: BalanceVersion::new(self.captured_balance_version)?,
            delta: money(self.delta)?,
            status: ReconciliationStatus::parse(&self.status)?,
            version: ReconciliationVersion::new(self.version)?,
            approval_journal_id: self.approval_journal_id.map(JournalEntryId::new),
            reason: self.reason,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(FromRow)]
struct AccountBalanceRow {
    id: Uuid,
    user_id: Uuid,
    name: String,
    currency: String,
    nature: String,
    kind: String,
    authority: String,
    visibility: String,
    lifecycle: String,
    system_role: Option<String>,
    version: i64,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    signed_balance: Decimal,
    balance_version: i64,
    as_of: chrono::DateTime<chrono::Utc>,
}

impl AccountBalanceRow {
    fn into_view(self) -> Result<AccountView, LedgerError> {
        let signed_balance = self.signed_balance;
        let account = AccountRow {
            id: self.id,
            user_id: self.user_id,
            name: self.name,
            currency: self.currency,
            nature: self.nature,
            kind: self.kind,
            authority: self.authority,
            visibility: self.visibility,
            lifecycle: self.lifecycle,
            system_role: self.system_role,
            version: self.version,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
        .into_domain()?;
        Ok(AccountView {
            id: account.id(),
            user_id: account.user_id(),
            name: account.name().to_owned(),
            currency: account.currency().clone(),
            nature: account.nature(),
            kind: account.kind(),
            authority: account.authority(),
            visibility: account.visibility(),
            lifecycle: account.lifecycle(),
            version: account.version(),
            signed_balance,
            display_balance: signed_balance * Decimal::from(account.normal_sign()),
            balance_version: self.balance_version,
            as_of: self.as_of,
            provider_reported: None,
            available: None,
            reconciliation_difference: None,
        })
    }
}
